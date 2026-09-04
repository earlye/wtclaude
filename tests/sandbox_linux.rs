//! End-to-end enforcement tests for the Landlock backend.
//!
//! These assert that the kernel actually refuses writes, not merely that we
//! emitted the rules we meant to — the failure mode of issue 019f8ff2 was a
//! rule that looked correct and matched nothing.
//!
//! Every case spawns `wtclaude sandbox` as a subprocess. That is not
//! incidental: `landlock_restrict_self` is irreversible for the calling
//! process, and `cargo test` runs tests as threads within one binary, so
//! applying a ruleset in-process would poison every test that ran after it.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A path that is genuinely outside the plan.
///
/// Not under `/tmp`: temp dirs are granted wholesale and always have been,
/// because build tools need them, so a "write outside" assertion aimed there
/// passes for the wrong reason. A path directly in `$HOME` is what the
/// sandbox actually exists to protect — the main checkout lives there — and
/// no built-in grant covers `$HOME` itself.
fn outside_the_plan(label: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set");
    PathBuf::from(home).join(format!(
        ".wtclaude-landlock-test-{}-{label}",
        std::process::id()
    ))
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wtclaude-landlock-test-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.canonicalize().expect("canonicalize temp dir")
}

/// Runs `script` under `wtclaude sandbox` with `sandbox_root` as the one
/// directory the session owns.
fn run_sandboxed(sandbox_root: &Path, script: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["sandbox", "--", "/bin/bash", "-c", script])
        .env("WTCLAUDE_SANDBOX", sandbox_root)
        .env_remove("WTCLAUDE_REPO_ROOT")
        .output()
        .expect("failed to spawn wtclaude sandbox")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn combined(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn writes_inside_the_sandbox_root_succeed() {
    let root = unique_temp_dir("inside");
    let output = run_sandboxed(
        &root,
        &format!("echo written > {}/f.txt && echo OK", root.display()),
    );
    let contents = std::fs::read_to_string(root.join("f.txt"));
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(stdout_of(&output), "OK", "{}", combined(&output));
    assert_eq!(contents.expect("file should exist").trim(), "written");
}

#[test]
fn writes_outside_the_sandbox_root_are_refused_by_the_kernel() {
    let root = unique_temp_dir("outside");
    let forbidden = outside_the_plan("should-not-appear.txt");
    let output = run_sandboxed(
        &root,
        &format!(
            "echo leaked > {} 2>/dev/null || echo DENIED",
            forbidden.display()
        ),
    );
    let landed = forbidden.exists();
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_file(&forbidden).ok();

    assert_eq!(stdout_of(&output), "DENIED", "{}", combined(&output));
    assert!(
        !landed,
        "the write must not have landed at {}",
        forbidden.display()
    );
}

#[test]
fn subdirectories_created_inside_the_sandbox_are_writable() {
    // The grant is a subtree, not a single directory, so paths that did not
    // exist when the ruleset was applied must still be writable.
    let root = unique_temp_dir("subtree");
    let output = run_sandboxed(
        &root,
        &format!(
            "mkdir -p {0}/a/b/c && echo deep > {0}/a/b/c/f.txt && echo OK",
            root.display()
        ),
    );
    let landed = root.join("a/b/c/f.txt").exists();
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(stdout_of(&output), "OK", "{}", combined(&output));
    assert!(landed);
}

#[test]
fn dev_null_and_shell_redirection_still_work() {
    // Regression test for the single most disruptive omission: without a
    // grant covering /dev/null, git, mktemp, stty and any script redirecting
    // to it all fail with "Permission denied", which looks nothing like a
    // sandbox problem.
    let root = unique_temp_dir("devnull");
    let output = run_sandboxed(
        &root,
        "echo discard > /dev/null && mktemp > /dev/null && echo OK",
    );
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(stdout_of(&output), "OK", "{}", combined(&output));
}

#[test]
fn git_can_commit_inside_the_sandbox() {
    // Signing is switched off explicitly: `~/.gnupg` being in the plan is
    // asserted as a unit test, and a signing key that wants a passphrase
    // would make this hang rather than fail.
    let root = unique_temp_dir("git");
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("git")
                .success(),
            "git {args:?} failed"
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);

    let output = run_sandboxed(
        &root,
        &format!(
            "cd {} && echo hello > tracked.txt && git add -A && git commit -qm 'from inside' \
             && git log --oneline | head -1",
            root.display()
        ),
    );
    let log = stdout_of(&output);
    std::fs::remove_dir_all(&root).ok();
    assert!(log.contains("from inside"), "{}", combined(&output));
}

#[test]
fn heredocs_work_under_the_linux_shell() {
    // The heredoc ban is Seatbelt's, for Apple's bash 3.2. This is the exact
    // shape that breaks there — a quoted heredoc inside a command
    // substitution inside double quotes, with an embedded apostrophe.
    let root = unique_temp_dir("heredoc");
    let output = run_sandboxed(&root, "printf '%s\\n' \"$(cat <<'EOF'\nit's fine\nEOF\n)\"");
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(stdout_of(&output), "it's fine", "{}", combined(&output));
}

#[test]
fn restrictions_are_inherited_by_grandchildren() {
    // Landlock restrictions survive exec and apply to descendants, which is
    // what makes wrapping only the top-level shell sufficient.
    let root = unique_temp_dir("inherit");
    let forbidden = outside_the_plan("inherit-nope.txt");
    let output = run_sandboxed(
        &root,
        &format!(
            "bash -c 'bash -c \"echo leaked > {} 2>/dev/null\"' || echo DENIED",
            forbidden.display()
        ),
    );
    let landed = forbidden.exists();
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_file(&forbidden).ok();

    assert_eq!(stdout_of(&output), "DENIED", "{}", combined(&output));
    assert!(!landed);
}

#[test]
fn sandboxes_nest_rather_than_failing() {
    // Landlock rulesets stack, each intersecting the last, so a wtclaude
    // session inside a wtclaude session works. `sandbox-exec` cannot do this
    // at all — macOS refuses `sandbox_apply` from an already-sandboxed
    // process, which is why hook.rs's round-trip test is #[ignore]d there.
    let outer = unique_temp_dir("nest-outer");
    let inner = outer.join("inner");
    std::fs::create_dir_all(&inner).unwrap();

    let nested = format!(
        "{} sandbox -- /bin/bash -c 'echo nested > {}/f.txt && echo OK'",
        env!("CARGO_BIN_EXE_wtclaude"),
        inner.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["sandbox", "--", "/bin/bash", "-c", &nested])
        .env("WTCLAUDE_SANDBOX", &outer)
        .env_remove("WTCLAUDE_REPO_ROOT")
        .output()
        .expect("spawn");

    let landed = inner.join("f.txt").exists();
    std::fs::remove_dir_all(&outer).ok();

    assert_eq!(stdout_of(&output), "OK", "{}", combined(&output));
    assert!(landed);
}

#[test]
fn nesting_intersects_rather_than_widening() {
    // The inner sandbox may not grant itself something the outer one denied.
    let outer = unique_temp_dir("nest-narrow-outer");
    let elsewhere = outside_the_plan("nest-narrow-elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("create dir the outer plan excludes");
    let target = elsewhere.join("nope.txt");

    // The inner invocation names `elsewhere` as its own sandbox root, which
    // on its own would make it writable.
    let nested = format!(
        "WTCLAUDE_SANDBOX={} {} sandbox -- /bin/bash -c 'echo leaked > {} 2>/dev/null' || echo DENIED",
        elsewhere.display(),
        env!("CARGO_BIN_EXE_wtclaude"),
        target.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["sandbox", "--", "/bin/bash", "-c", &nested])
        .env("WTCLAUDE_SANDBOX", &outer)
        .env_remove("WTCLAUDE_REPO_ROOT")
        .output()
        .expect("spawn");

    let landed = target.exists();
    std::fs::remove_dir_all(&outer).ok();
    std::fs::remove_dir_all(&elsewhere).ok();

    assert!(
        !landed,
        "the outer sandbox must still deny {}",
        target.display()
    );
    assert_eq!(stdout_of(&output), "DENIED", "{}", combined(&output));
}

#[test]
fn sandbox_subcommand_refuses_without_a_sandbox_root() {
    // `wtclaude sandbox` is invoked by the hook, never directly, and must not
    // silently run a command unconfined if the environment is incomplete.
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["sandbox", "--", "/bin/bash", "-c", "echo SHOULD_NOT_RUN"])
        .env_remove("WTCLAUDE_SANDBOX")
        .env_remove("WTCLAUDE_REPO_ROOT")
        .output()
        .expect("spawn");

    assert!(!output.status.success(), "{}", combined(&output));
    assert!(
        !stdout_of(&output).contains("SHOULD_NOT_RUN"),
        "{}",
        combined(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("WTCLAUDE_SANDBOX is not set"),
        "{}",
        combined(&output)
    );
}

#[test]
fn describe_lists_the_sandbox_root_and_reports_kernel_capabilities() {
    let root = unique_temp_dir("describe");
    let output = Command::new(env!("CARGO_BIN_EXE_wtclaude"))
        .args(["sandbox", "--describe"])
        .env("WTCLAUDE_SANDBOX", &root)
        .env_remove("WTCLAUDE_REPO_ROOT")
        .output()
        .expect("spawn");
    let stdout = stdout_of(&output);
    std::fs::remove_dir_all(&root).ok();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(
        stdout.contains(&format!("write-subtree {}", root.display())),
        "{stdout}"
    );
    assert!(stdout.contains("refer=yes"), "{stdout}");
}
