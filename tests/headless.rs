use std::path::PathBuf;
use std::process::Command;

// These run the actual compiled `wtclaude` binary as a subprocess in a fresh
// temp directory, rather than calling `run_headless` in-process, so the test
// doesn't have to mutate this process's own working directory (which would
// race against other tests running in parallel). The tests in this first
// group all bail before ever touching `$HOME` or spawning `claude`, so none
// of them need a fake HOME or a `claude` binary on PATH.

fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wtclaude-integration-test-{}-{}",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn headless_subcommand_bails_outside_git_repo() {
    let dir = unique_temp_dir("non-git-repo");
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "hello"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn wtclaude");
    std::fs::remove_dir_all(&dir).ok();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("existing git repository"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn headless_subcommand_bails_on_unknown_mode() {
    let dir = unique_temp_dir("unknown-mode");
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "--mode", "not-a-real-mode", "hello"])
        .current_dir(&dir)
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
fn headless_subcommand_prints_usage_on_parse_error() {
    let dir = unique_temp_dir("parse-error");
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "--mode"])
        .current_dir(&dir)
        .output()
        .expect("failed to spawn wtclaude");
    std::fs::remove_dir_all(&dir).ok();
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a value is required for '--mode <MODE>'"),
        "stderr: {stderr}"
    );
}

// The following tests exercise the actual `claude` subprocess invocation
// (command construction and exit-code/signal passthrough). They stand up a
// minimal stub script named `claude` on PATH instead of the real binary —
// not to fake AI behavior, just to observe/control the child process's argv
// and exit condition. Each also points HOME at a fresh, `.claude.json`-less
// scratch directory so `update_trust` is a no-op and never touches the
// developer's real trust config.

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
fn headless_subcommand_translates_signal_kill_to_exit_128_plus_signal() {
    let repo = git_init_dir("signal-repo");
    let home = unique_temp_dir("signal-home");
    let stub_dir = unique_temp_dir("signal-stub");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nexec sh -c 'kill -9 $$'\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "hello"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .output()
        .expect("failed to spawn wtclaude");

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert_eq!(
        output.status.code(),
        Some(137),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("signal 9"));
}

#[test]
fn headless_subcommand_places_resume_and_print_flags_and_forwards_prompt() {
    let repo = git_init_dir("argv-repo");
    let home = unique_temp_dir("argv-home");
    let stub_dir = unique_temp_dir("argv-stub");
    let log_path = stub_dir.join("argv.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGV_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "--resume", "sess-123", "hello there"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("ARGV_LOG_PATH", &log_path)
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path).unwrap_or_else(|e| {
        panic!("reading argv log at {}: {e}", log_path.display());
    });

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged_args: Vec<&str> = logged.lines().collect();
    let print_pos = logged_args
        .iter()
        .position(|a| *a == "--print")
        .expect("--print should be present in claude's argv");
    let resume_pos = logged_args
        .iter()
        .position(|a| *a == "--resume")
        .expect("--resume should be present in claude's argv");
    assert_eq!(logged_args.get(resume_pos + 1), Some(&"sess-123"));
    assert!(
        resume_pos < print_pos,
        "expected --resume before --print, got {logged_args:?}"
    );
    assert_eq!(logged_args.last(), Some(&"hello there"));
    assert!(
        !logged_args.contains(&"--output-format"),
        "expected --output-format to be omitted when not requested, got {logged_args:?}"
    );
    assert!(
        !logged_args.contains(&"--include-partial-messages"),
        "expected --include-partial-messages to be omitted when not requested, got {logged_args:?}"
    );
}

#[test]
fn headless_subcommand_forwards_output_format_to_claude() {
    let repo = git_init_dir("output-format-repo");
    let home = unique_temp_dir("output-format-home");
    let stub_dir = unique_temp_dir("output-format-stub");
    let log_path = stub_dir.join("argv.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGV_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "--output-format", "json", "hello there"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("ARGV_LOG_PATH", &log_path)
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path).unwrap_or_else(|e| {
        panic!("reading argv log at {}: {e}", log_path.display());
    });

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged_args: Vec<&str> = logged.lines().collect();
    let output_format_pos = logged_args
        .iter()
        .position(|a| *a == "--output-format")
        .expect("--output-format should be present in claude's argv");
    assert_eq!(logged_args.get(output_format_pos + 1), Some(&"json"));
    assert_eq!(logged_args.last(), Some(&"hello there"));
}

#[test]
fn headless_subcommand_forwards_include_partial_messages_to_claude() {
    let repo = git_init_dir("include-partial-messages-repo");
    let home = unique_temp_dir("include-partial-messages-home");
    let stub_dir = unique_temp_dir("include-partial-messages-stub");
    let log_path = stub_dir.join("argv.log");
    make_executable_script(
        &stub_dir.join("claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGV_LOG_PATH\"\nexit 0\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["headless", "--include-partial-messages", "hello there"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", stub_path_env(&stub_dir))
        .env("ARGV_LOG_PATH", &log_path)
        .output()
        .expect("failed to spawn wtclaude");

    let logged = std::fs::read_to_string(&log_path).unwrap_or_else(|e| {
        panic!("reading argv log at {}: {e}", log_path.display());
    });

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&stub_dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged_args: Vec<&str> = logged.lines().collect();
    assert!(
        logged_args.contains(&"--include-partial-messages"),
        "--include-partial-messages should be present in claude's argv, got {logged_args:?}"
    );
    assert_eq!(logged_args.last(), Some(&"hello there"));
}
