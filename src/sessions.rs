use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::config;
use crate::launch::repo_root;

pub fn run_modes() -> Result<()> {
    let config = config::load()?;
    let mut names: Vec<&String> = config.modes.keys().collect();
    names.sort();
    for name in names {
        println!("{}", name);
    }
    Ok(())
}

pub fn run_sessions() -> Result<()> {
    let repo_root = repo_root()?;
    let slug_prefix = path_to_slug(&repo_root) + "--claude-worktrees-";

    let home = std::env::var("HOME").context("HOME not set")?;
    let projects_dir = PathBuf::from(&home).join(".claude").join("projects");

    if !projects_dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<(SystemTime, String, String)> = Vec::new();

    for entry in std::fs::read_dir(&projects_dir).context("reading projects dir")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        if !name.starts_with(&slug_prefix) {
            continue;
        }

        let worktree = name[slug_prefix.len()..].to_string();

        for session_entry in std::fs::read_dir(entry.path())? {
            let session_entry = session_entry?;
            let fname = session_entry.file_name().to_string_lossy().to_string();

            if !fname.ends_with(".jsonl") {
                continue;
            }

            let session_id = fname.trim_end_matches(".jsonl").to_string();
            // Validate it looks like a UUID (basic check)
            if session_id.len() != 36 {
                continue;
            }

            let mtime = session_entry.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((mtime, session_id, worktree.clone()));
        }
    }

    entries.sort_by(|a, b| b.0.cmp(&a.0));

    for (mtime, session_id, worktree) in entries {
        let epoch = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        println!("{}\t{}\t{}", session_id, worktree, epoch);
    }

    Ok(())
}

pub fn run_worktrees() -> Result<()> {
    let repo_root = repo_root()?;
    let worktrees_dir = repo_root.join(".claude").join("worktrees");

    if !worktrees_dir.exists() {
        return Ok(());
    }

    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&worktrees_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    for name in names {
        println!("{}", name);
    }

    Ok(())
}

pub fn run_session_worktree(session_id: &str) -> Result<()> {
    let repo_root = repo_root()?;
    let slug_prefix = path_to_slug(&repo_root) + "--claude-worktrees-";

    let home = std::env::var("HOME").context("HOME not set")?;
    let projects_dir = PathBuf::from(&home).join(".claude").join("projects");

    for entry in std::fs::read_dir(&projects_dir).context("reading projects dir")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        if !name.starts_with(&slug_prefix) {
            continue;
        }

        let worktree = &name[slug_prefix.len()..];
        let jsonl = entry.path().join(format!("{}.jsonl", session_id));

        if jsonl.exists() {
            println!("{}", worktree);
            return Ok(());
        }
    }

    Ok(())
}

fn path_to_slug(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('/', "-")
        .replace('.', "-")
}
