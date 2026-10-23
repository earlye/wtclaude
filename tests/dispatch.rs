use std::process::Command;

// These exercise main.rs's top-level dispatch: the first-token peek that
// routes known subcommand keywords through the clap `Cli`/`Commands`
// subcommand tree, versus falling through to `LaunchArgs` for the bare
// `wtclaude WORKTREE_NAME [PROMPT]` invocation with no keyword. Run as
// subprocesses since clap's own parse-error/--help paths call
// `process::exit` directly.

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(args)
        .output()
        .expect("failed to spawn wtclaude")
}

#[test]
fn bare_invocation_with_no_args_requires_worktree_name() {
    let output = run(&[]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WORKTREE_NAME"), "stderr: {stderr}");
}

#[test]
fn unknown_top_level_flag_falls_through_to_launch_args_and_errors() {
    // "--bogus-flag" isn't a known subcommand keyword, so it must fall
    // through to LaunchArgs::try_parse_from rather than the Cli subcommand
    // tree, and clap should reject it as an unrecognized argument there.
    let output = run(&["--bogus-flag"]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument"), "stderr: {stderr}");
}

#[test]
fn session_worktree_with_no_arg_errors_via_clap() {
    let output = run(&["session-worktree"]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("SESSION_ID"), "stderr: {stderr}");
}

#[test]
fn session_worktree_with_empty_arg_errors_via_clap() {
    // NonEmptyStringValueParser rejects "" the same way it rejects a
    // missing value, so this must not silently succeed (it did briefly
    // during development, before the value_parser was added).
    let output = run(&["session-worktree", ""]);
    assert!(
        !output.status.success(),
        "empty SESSION_ID must not silently succeed"
    );
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
}

#[test]
fn top_level_help_documents_the_full_command_surface() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The primary, most-frequently-typed invocation form...
    assert!(stdout.contains("WORKTREE_NAME"), "stdout: {stdout}");
    // ...plus the rest of the command surface via after_help.
    assert!(stdout.contains("wtclaude headless"), "stdout: {stdout}");
    assert!(
        stdout.contains("wtclaude --completions zsh|bash"),
        "stdout: {stdout}"
    );
    // Regression guard: the after_help wiring note in src/launch.rs must
    // stay a plain `//` comment, not a `///` doc comment — the latter
    // leaks it into this exact --help output (caught once during review).
    assert!(
        !stdout.contains("injects an after_help"),
        "stdout leaked an internal implementation comment: {stdout}"
    );
}

#[test]
fn no_arg_subcommand_rejects_trailing_arguments() {
    let output = run(&["modes", "extra-arg"]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument"), "stderr: {stderr}");
}

#[test]
fn completions_rejects_an_unsupported_shell_and_lists_the_supported_ones() {
    let output = run(&["--completions", "fish"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Supported: zsh, bash"), "stderr: {stderr}");
}

/// Writes `wtclaude --completions SHELL` to a file under `dir`.
fn completion_script(dir: &std::path::Path, shell: &str) -> std::path::PathBuf {
    let output = run(&["--completions", shell]);
    assert!(output.status.success());
    let path = dir.join(format!("completions.{shell}"));
    std::fs::write(&path, &output.stdout).expect("write completion script");
    path
}

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wtclaude-integration-test-dispatch-{}-{}",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn completion_scripts_parse() {
    let dir = unique_temp_dir("syntax");
    let bash = completion_script(&dir, "bash");
    let zsh = completion_script(&dir, "zsh");
    let bash_check = Command::new("bash").arg("-n").arg(&bash).output();
    let zsh_check = Command::new("zsh").arg("-n").arg(&zsh).output();
    std::fs::remove_dir_all(&dir).ok();

    let bash_check = bash_check.expect("spawning bash");
    assert!(
        bash_check.status.success(),
        "bash -n: {}",
        String::from_utf8_lossy(&bash_check.stderr)
    );
    match zsh_check {
        Ok(out) => assert!(
            out.status.success(),
            "zsh -n: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping zsh -n: zsh is not installed");
        }
        Err(e) => panic!("spawning zsh: {e}"),
    }
}

/// A fake `wtclaude` that writes a resume command to the handoff file.
const WRITES_RESUME_COMMAND: &str = "[ -n \"$WTCLAUDE_RESUME_FILE\" ] && echo 'wtclaude --resume abc feat/x' > \"$WTCLAUDE_RESUME_FILE\"\n";

/// Sources the bash script in a shell whose `wtclaude` on PATH is a fake
/// that records the handoff path it was given, runs `body`, and exits 3.
/// Returns the shell's stdout and stderr, and the recorded path.
fn run_bash_wrapper(
    dir: &std::path::Path,
    state_home: &std::path::Path,
    body: &str,
) -> (String, String, String) {
    let script = completion_script(dir, "bash");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin dir");
    let fake = bin.join("wtclaude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n\
             printf '%s' \"${{WTCLAUDE_RESUME_FILE-unset}}\" > \"$SEEN_LOG_PATH\"\n\
             {body}\
             exit 3\n"
        ),
    )
    .expect("write fake wtclaude");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake wtclaude");
    let seen_log = dir.join("seen.log");

    let output = Command::new("bash")
        .args(["--norc", "--noprofile", "-c"])
        .arg(r#"set -o history; source "$1"; wtclaude x; echo "rc=$?"; history"#)
        .arg("bash")
        .arg(&script)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", state_home)
        .env("HISTFILE", dir.join("history"))
        .env("SEEN_LOG_PATH", &seen_log)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .expect("spawning bash");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = std::fs::read_to_string(&seen_log).expect("reading seen log");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        seen,
    )
}

#[test]
fn bash_wrapper_pushes_the_resume_command_onto_history() {
    let dir = unique_temp_dir("bash-wrapper");
    let state_home = dir.join("state");
    let (stdout, stderr, handoff) = run_bash_wrapper(&dir, &state_home, WRITES_RESUME_COMMAND);
    let handoff_left = std::path::Path::new(&handoff).exists();
    std::fs::remove_dir_all(&dir).ok();

    assert!(stdout.contains("rc=3\n"), "stdout: {stdout}");
    assert!(
        stdout.contains("wtclaude --resume abc feat/x\n"),
        "history should hold the resume command: {stdout}"
    );
    assert!(
        handoff.starts_with(&format!("{}/wtclaude/", state_home.display())),
        "handoff file should live under XDG_STATE_HOME, got {handoff}"
    );
    assert!(!handoff_left, "the wrapper should delete {handoff}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
}

#[test]
fn bash_wrapper_still_runs_wtclaude_when_it_cannot_make_a_handoff_file() {
    let dir = unique_temp_dir("bash-wrapper-no-state");
    // A regular file where the state dir should be makes mkdir fail.
    let state_home = dir.join("state");
    std::fs::write(&state_home, "").expect("create blocking file");
    let (stdout, stderr, handoff) = run_bash_wrapper(&dir, &state_home, WRITES_RESUME_COMMAND);
    std::fs::remove_dir_all(&dir).ok();

    assert!(stdout.contains("rc=3\n"), "stdout: {stdout}");
    assert_eq!(handoff, "unset");
    assert!(
        stderr.contains(&format!(
            "wtclaude: warning: cannot create a handoff file in {}/wtclaude; \
             the resume command will not be added to history",
            state_home.display()
        )),
        "stderr: {stderr}"
    );
    assert!(!stdout.contains("--resume"), "stdout: {stdout}");
}

#[test]
fn bash_wrapper_does_not_follow_a_symlinked_handoff_file() {
    let dir = unique_temp_dir("bash-wrapper-symlink");
    let state_home = dir.join("state");
    let target = dir.join("planted");
    std::fs::write(&target, "echo pwned\n").expect("create symlink target");
    let (stdout, _stderr, handoff) = run_bash_wrapper(
        &dir,
        &state_home,
        &format!(
            "rm -f \"$WTCLAUDE_RESUME_FILE\" && ln -s '{}' \"$WTCLAUDE_RESUME_FILE\"\n",
            target.display()
        ),
    );
    let link_left = std::fs::symlink_metadata(&handoff).is_ok();
    let target_after = std::fs::read_to_string(&target).expect("reading symlink target");
    std::fs::remove_dir_all(&dir).ok();

    assert!(stdout.contains("rc=3\n"), "stdout: {stdout}");
    assert!(
        !stdout.contains("pwned"),
        "a symlinked handoff file must not be injected: {stdout}"
    );
    assert_eq!(target_after, "echo pwned\n");
    assert!(
        !link_left,
        "the wrapper should delete the symlink {handoff}"
    );
}

#[test]
fn bash_wrapper_sweeps_week_old_handoff_files() {
    let dir = unique_temp_dir("bash-wrapper-sweep");
    let state_home = dir.join("state");
    let wt_dir = state_home.join("wtclaude");
    std::fs::create_dir_all(&wt_dir).expect("create state dir");
    let stale = wt_dir.join("last-session.stale1");
    let fresh = wt_dir.join("last-session.fresh1");
    std::fs::write(&stale, "").expect("create stale file");
    std::fs::write(&fresh, "").expect("create fresh file");
    let eight_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .and_then(|f| f.set_modified(eight_days_ago))
        .expect("backdate stale file");
    run_bash_wrapper(&dir, &state_home, WRITES_RESUME_COMMAND);
    let (stale_left, fresh_left) = (stale.exists(), fresh.exists());
    std::fs::remove_dir_all(&dir).ok();

    assert!(!stale_left, "a handoff file over a week old should be swept");
    assert!(
        fresh_left,
        "a recent handoff file may belong to a live wrapper"
    );
}
