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
        stdout.contains("wtclaude --completions zsh"),
        "stdout: {stdout}"
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
