use std::path::PathBuf;
use std::process::Command;

// These run the actual compiled `wtclaude` binary as a subprocess in a fresh
// temp directory, same rationale as tests/headless.rs: avoids mutating this
// test process's own cwd, which would race against other tests.

fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wtclaude-integration-test-path-{}-{}",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn make_executable_script(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).expect("write stub script");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .expect("stat stub script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod stub script");
}

fn git_init_dir(label: &str) -> PathBuf {
    let dir = unique_temp_dir(label);
    let out = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn git init");
    assert!(
        out.status.success(),
        "git init failed for {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn stub_path_env(stub_dir: &std::path::Path) -> String {
    format!(
        "{}:{}",
        stub_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn path_subcommand_bails_on_nonexistent_directory() {
    let missing = unique_temp_dir("missing-parent").join("does-not-exist");
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", missing.to_str().unwrap(), "hello"])
        .output()
        .expect("failed to spawn wtclaude");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not exist"), "stderr: {stderr}");
}

#[test]
fn path_subcommand_bails_on_unknown_mode() {
    let dir = unique_temp_dir("unknown-mode");
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args([
            "path",
            "--mode",
            "not-a-real-mode",
            dir.to_str().unwrap(),
            "hello",
        ])
        .output()
        .expect("failed to spawn wtclaude");
    std::fs::remove_dir_all(&dir).ok();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown mode"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn path_subcommand_prints_usage_on_parse_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", "--mode"])
        .output()
        .expect("failed to spawn wtclaude");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a value is required for '--mode <MODE>'"),
        "stderr: {stderr}"
    );
}

#[test]
fn path_subcommand_runs_without_a_git_repo_and_omits_repo_root_env() {
    let dir = unique_temp_dir("non-git-dir");
    let home = unique_temp_dir("non-git-home");
    let stub_dir = unique_temp_dir("non-git-stub");
    let log_path = stub_dir.join("result.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\n\
         if [ -n \"$WTCLAUDE_REPO_ROOT\" ]; then\n\
         echo \"set:$WTCLAUDE_REPO_ROOT\" > \"$RESULT_LOG_PATH\"\n\
         else\n\
         echo unset > \"$RESULT_LOG_PATH\"\n\
         fi\n\
         exit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", dir.to_str().unwrap(), "hello"])
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("RESULT_LOG_PATH", &log_path)
        // Simulates running nested inside another wtclaude sandbox, which
        // would already have this set in the ambient environment — proves
        // run_path's `env_remove` actually strips it rather than merely
        // never setting it (the two are indistinguishable without this).
        .env(
            "WTCLAUDE_REPO_ROOT",
            "/some/ambient/repo/from/an/outer/sandbox",
        )
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("reading result log at {}: {e}", log_path.display()));

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(logged.trim(), "unset");
}

#[test]
fn path_subcommand_sets_repo_root_env_when_directory_is_inside_a_repo() {
    let repo = git_init_dir("repo-root-env-repo");
    let home = unique_temp_dir("repo-root-env-home");
    let stub_dir = unique_temp_dir("repo-root-env-stub");
    let log_path = stub_dir.join("result.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s' \"$WTCLAUDE_REPO_ROOT\" > \"$RESULT_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", repo.to_str().unwrap(), "hello"])
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("RESULT_LOG_PATH", &log_path)
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("reading result log at {}: {e}", log_path.display()));
    let expected_repo_root = repo.canonicalize().expect("canonicalize repo path");

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(PathBuf::from(logged.trim()), expected_repo_root);
}

#[test]
fn path_subcommand_sandboxes_a_subdirectory_while_repo_root_is_the_enclosing_repo() {
    // Every other repo-mode test above passes DIRECTORY == the repo root
    // itself, where the sandbox boundary and repo_root happen to coincide.
    // This is the one that discriminates: an implementation that reused
    // repo_root as the sandbox boundary (instead of DIRECTORY) would pass
    // every other test here unnoticed.
    let repo = git_init_dir("subdir-repo");
    let subdir = repo.join("sub");
    std::fs::create_dir_all(&subdir).expect("create subdir");
    let home = unique_temp_dir("subdir-home");
    let stub_dir = unique_temp_dir("subdir-stub");
    let log_path = stub_dir.join("result.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf 'sandbox=%s\\nrepo_root=%s\\n' \"$WTCLAUDE_SANDBOX\" \"$WTCLAUDE_REPO_ROOT\" > \"$RESULT_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", subdir.to_str().unwrap(), "hello"])
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("RESULT_LOG_PATH", &log_path)
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("reading result log at {}: {e}", log_path.display()));
    let expected_repo_root = repo.canonicalize().expect("canonicalize repo path");
    let expected_sandbox = subdir.canonicalize().expect("canonicalize subdir path");

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut sandbox_value = None;
    let mut repo_root_value = None;
    for line in logged.lines() {
        if let Some(v) = line.strip_prefix("sandbox=") {
            sandbox_value = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("repo_root=") {
            repo_root_value = Some(v.to_string());
        }
    }

    assert_eq!(
        sandbox_value.as_deref(),
        Some(expected_sandbox.to_string_lossy().as_ref())
    );
    assert_eq!(
        repo_root_value.as_deref(),
        Some(expected_repo_root.to_string_lossy().as_ref())
    );
    assert_ne!(
        sandbox_value, repo_root_value,
        "sandbox boundary must be the subdirectory, not the enclosing repo root"
    );
}

#[test]
fn path_subcommand_sandbox_notice_mentions_dot_git_only_when_repo_present() {
    let repo = git_init_dir("notice-repo");
    let non_repo = unique_temp_dir("notice-non-repo");
    let home = unique_temp_dir("notice-home");
    let stub_dir = unique_temp_dir("notice-stub");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGV_LOG_PATH\"\nexit 0\n",
    );

    let notice_for = |dir: &std::path::Path, log_path: &std::path::Path| -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
            .args(["path", dir.to_str().unwrap(), "hello"])
            .env("HOME", &home)
            .env("PATH", stub_path_env(&stub_dir))
            .env("ARGV_LOG_PATH", log_path)
            .env_remove("TMUX")
            .output()
            .expect("failed to spawn wtclaude");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let logged = std::fs::read_to_string(log_path)
            .unwrap_or_else(|e| panic!("reading argv log at {}: {e}", log_path.display()));
        let logged_args: Vec<String> = logged.lines().map(str::to_string).collect();
        let notice_pos = logged_args
            .iter()
            .position(|a| a == "--append-system-prompt")
            .expect("--append-system-prompt should be present in claude's argv");
        logged_args[notice_pos + 1].clone()
    };

    let repo_log = stub_dir.join("repo-argv.log");
    let non_repo_log = stub_dir.join("non-repo-argv.log");
    let repo_notice = notice_for(&repo, &repo_log);
    let non_repo_notice = notice_for(&non_repo, &non_repo_log);

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&non_repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    // sandbox_warning_common()'s shared trailing text mentions `.git` too
    // (unrelated to repo-vs-non-repo), so check the specific phrase this
    // conditional actually controls, not a bare ".git" substring.
    assert!(
        repo_notice.contains("and the repo's .git"),
        "repo-mode notice should mention the repo's .git: {repo_notice}"
    );
    assert!(
        !non_repo_notice.contains("and the repo's .git"),
        "non-repo-mode notice should not mention the repo's .git: {non_repo_notice}"
    );
    assert!(
        repo_notice.contains("package-manager cache dirs"),
        "repo-mode notice: {repo_notice}"
    );
    assert!(
        non_repo_notice.contains("package-manager cache dirs"),
        "non-repo-mode notice should still mention package-manager cache dirs: {non_repo_notice}"
    );
}

#[test]
fn path_subcommand_surfaces_a_real_git_error_instead_of_treating_it_as_no_repo() {
    let dir = unique_temp_dir("git-missing-dir");
    let stub_dir = unique_temp_dir("git-missing-stub");
    make_executable_script(&stub_dir.join("claude"), "#!/bin/sh\nexit 0\n");

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", dir.to_str().unwrap(), "hello"])
        // No system PATH at all: `git` itself can't be found, which must
        // surface as a hard error rather than being silently treated the
        // same as "DIRECTORY just isn't inside a repo".
        .env("PATH", stub_dir.to_str().unwrap())
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        !output.status.success(),
        "expected wtclaude to bail when git itself can't run, not silently proceed as repo-less"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("resolving repo root"), "stderr: {stderr}");
}

#[test]
fn path_subcommand_forwards_resume_and_prompt_and_omits_print() {
    let dir = unique_temp_dir("argv-dir");
    let home = unique_temp_dir("argv-home");
    let stub_dir = unique_temp_dir("argv-stub");
    let log_path = stub_dir.join("argv.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGV_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args([
            "path",
            "--resume",
            "sess-123",
            dir.to_str().unwrap(),
            "hello there",
        ])
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("ARGV_LOG_PATH", &log_path)
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("reading argv log at {}: {e}", log_path.display()));

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged_args: Vec<&str> = logged.lines().collect();
    let resume_pos = logged_args
        .iter()
        .position(|a| *a == "--resume")
        .expect("--resume should be present in claude's argv");
    assert_eq!(logged_args.get(resume_pos + 1), Some(&"sess-123"));
    assert!(
        !logged_args.contains(&"--print"),
        "path mode is interactive and must not pass --print, got {logged_args:?}"
    );
    assert_eq!(logged_args.last(), Some(&"hello there"));
}

#[test]
fn path_subcommand_rejects_home_directory_reached_through_a_symlink() {
    // Regression test: is_sandbox_nullifying_root used to compare a raw,
    // uncanonicalized $HOME against the canonicalized DIRECTORY, so this
    // exact case (a HOME whose path is itself a symlink, same as macOS's
    // real /tmp -> /private/tmp) silently bypassed the guard.
    let real_home = unique_temp_dir("symlinked-home-real");
    let home_parent = unique_temp_dir("symlinked-home-parent");
    let home_symlink = home_parent.join("home-symlink");
    std::os::unix::fs::symlink(&real_home, &home_symlink).expect("create symlink");

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", home_symlink.to_str().unwrap(), "hello"])
        .env("HOME", &home_symlink)
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    std::fs::remove_dir_all(&real_home).ok();
    std::fs::remove_dir_all(&home_parent).ok();

    assert!(
        !output.status.success(),
        "expected wtclaude to refuse sandboxing against $HOME even reached via a symlink"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("refusing to sandbox"), "stderr: {stderr}");
}

#[test]
fn path_subcommand_accepts_a_literal_dot_as_the_current_directory() {
    // The flag's own --help text recommends `.` as the idiomatic way to
    // mean "here" — every other test above passes a pre-resolved absolute
    // path instead, so this is the one exercising that documented usage.
    let dir = unique_temp_dir("dot-arg-dir");
    let home = unique_temp_dir("dot-arg-home");
    let stub_dir = unique_temp_dir("dot-arg-stub");
    let log_path = stub_dir.join("result.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s' \"$WTCLAUDE_SANDBOX\" > \"$RESULT_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", ".", "hello"])
        .current_dir(&dir)
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("RESULT_LOG_PATH", &log_path)
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("reading result log at {}: {e}", log_path.display()));
    let expected = dir.canonicalize().expect("canonicalize dir");

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(PathBuf::from(logged.trim()), expected);
}

#[test]
fn path_subcommand_sbpl_policy_allowlists_directory_and_repo_git_dir() {
    // Confirms the *actual generated policy content* for repo-mode `path`,
    // not just the WTCLAUDE_REPO_ROOT env var wiring: a regression that
    // accidentally passed `None` instead of `repo_root.as_deref()` into
    // `write_sbpl_policy` would leave every existing env-var-only test
    // passing while silently dropping the repo's .git from the allowlist.
    let repo = git_init_dir("sbpl-policy-repo");
    let home = unique_temp_dir("sbpl-policy-home");

    let mut child = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", "--show-policy", repo.to_str().unwrap(), "hello"])
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin") // no `claude` on PATH: never reached
        .env_remove("TMUX")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn wtclaude");

    // --show-policy prints the policy, then blocks on a stdin read waiting
    // for Enter; closing stdin immediately unblocks it without needing to
    // press anything, and it then fails to exec `claude` (not on PATH),
    // which is fine — the policy was already printed by that point.
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for wtclaude");

    let expected_repo_dir = repo.canonicalize().expect("canonicalize repo path");
    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            expected_repo_dir.display()
        )),
        "policy should allowlist DIRECTORY itself: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            expected_repo_dir.join(".git").display()
        )),
        "policy should allowlist the repo's .git dir: {stdout}"
    );
}

#[test]
fn path_subcommand_treats_directory_as_repo_less_regardless_of_locale() {
    // Regression test: repo_root_at distinguishes "genuinely not a repo"
    // from other git failures by pattern-matching git's stderr for "not a
    // git repository" — which git translates under a non-C locale, so
    // repo-less `path` mode used to hard-fail entirely for non-English
    // users. Best-effort: if fr_FR.UTF-8 isn't installed on this machine,
    // git/libc silently ignore it and this just re-confirms the C-locale
    // case, still useful but not exercising the original bug.
    let dir = unique_temp_dir("locale-non-repo-dir");
    let home = unique_temp_dir("locale-non-repo-home");
    let stub_dir = unique_temp_dir("locale-non-repo-stub");
    make_executable_script(&stub_dir.join("claude"), "#!/bin/sh\nexit 0\n");

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["path", dir.to_str().unwrap(), "hello"])
        .env("HOME", &home)
        .env("LANG", "fr_FR.UTF-8")
        .env("LC_ALL", "fr_FR.UTF-8")
        .env("PATH", stub_path_env(&stub_dir))
        .env_remove("TMUX")
        .output()
        .expect("failed to spawn wtclaude");

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("resolving repo root"),
        "a non-C locale must not make repo-less DIRECTORY detection fail: stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hook_regenerates_missing_sbpl_policy_without_repo_root_when_directory_is_repo_less() {
    // Directly exercises hook.rs's regenerate_sbpl_policy with
    // WTCLAUDE_REPO_ROOT unset — the exact scenario `wtclaude path` against
    // a non-repo DIRECTORY produces, and the code path this diff moved from
    // a hard `bail!` to tolerating a missing repo root. Runs as its own
    // subprocess so mutating WTCLAUDE_SANDBOX/WTCLAUDE_SBPL/WTCLAUDE_REPO_ROOT
    // here can't race with any other test's use of those process-global vars.
    let dir = unique_temp_dir("hook-regen-dir");
    // Deliberately does not exist yet: wrap_bash_in_sandbox regenerates a
    // missing policy file, which is the behavior under test.
    let sbpl_path = dir.join("missing-policy.sb");
    let payload = format!(
        r#"{{"tool_name":"Bash","cwd":"{}","tool_input":{{"command":"echo hi"}}}}"#,
        dir.to_str().unwrap()
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["hook"])
        .env("WTCLAUDE_SANDBOX", &dir)
        .env("WTCLAUDE_SBPL", &sbpl_path)
        .env_remove("WTCLAUDE_REPO_ROOT")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn wtclaude hook");

    use std::io::Write;
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(payload.as_bytes())
        .expect("write hook payload");

    let output = child.wait_with_output().expect("wait for wtclaude hook");
    let regenerated = std::fs::read_to_string(&sbpl_path);

    std::fs::remove_dir_all(&dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("updatedInput"),
        "hook should wrap the Bash command, not deny it: stdout: {stdout}"
    );
    assert!(
        !stdout.contains("deny"),
        "hook should not deny the Bash call: stdout: {stdout}"
    );
    let regenerated = regenerated.expect("policy file should have been regenerated");
    assert!(
        !regenerated.contains(".git"),
        "regenerated policy should have no repo .git allowlisted: {regenerated}"
    );
}
