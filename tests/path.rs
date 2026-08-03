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
