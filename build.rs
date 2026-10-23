// Sets WTCLAUDE_VERSION for `wtclaude --version`: CARGO_PKG_VERSION plus
// `+<short-hash>` (and `.dirty` with uncommitted changes) when built from a
// git checkout, plain CARGO_PKG_VERSION otherwise. The package version itself
// is written to Cargo.toml by CI on merge to main; build.rs cannot change it.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

fn main() {
    let pkg_version = env!("CARGO_PKG_VERSION");

    // Once any rerun-if-changed is emitted, Cargo stops rerunning build.rs on
    // arbitrary package changes, so source edits must be listed explicitly or
    // `.dirty` goes stale.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let Some(hash) = git(&["rev-parse", "--short", "HEAD"]) else {
        println!("cargo:rustc-env=WTCLAUDE_VERSION={pkg_version}");
        return;
    };

    // In a linked worktree `.git` is a file, so ask git where these live.
    // Missing paths are skipped: Cargo treats them as always changed.
    let mut watched = vec![
        "HEAD".to_string(),
        "index".to_string(),
        "packed-refs".to_string(),
    ];
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(head_ref);
    }
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--git-path", &name])
            && std::path::Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let suffix = if dirty { ".dirty" } else { "" };
    println!("cargo:rustc-env=WTCLAUDE_VERSION={pkg_version}+{hash}{suffix}");
}
