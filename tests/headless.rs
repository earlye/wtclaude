use std::path::PathBuf;
use std::process::Command;

// These run the actual compiled `wtclaude` binary as a subprocess in a fresh
// temp directory, rather than calling `run_headless` in-process, so the test
// doesn't have to mutate this process's own working directory (which would
// race against other tests running in parallel). Both bail before ever
// touching `$HOME` or spawning `claude`, so neither needs a fake HOME or a
// `claude` binary on PATH.

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
