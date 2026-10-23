use anyhow::{Context, Result, bail};
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

    let projects_dir = projects_dir()?;

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

    let projects_dir = projects_dir()?;

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

/// Whether Claude has written a transcript for `session_id` under any
/// project. Ok(false) means it hasn't: Claude Code (observed in 2.1.309)
/// writes the transcript only once the first message is sent; if that
/// changes, sessions exited without a message get a history entry too. A
/// missing projects dir is an error, since a session has just run and
/// should have created it.
pub fn session_exists(session_id: &str) -> Result<bool> {
    let projects_dir = projects_dir()?;
    if !projects_dir
        .try_exists()
        .with_context(|| format!("checking {}", projects_dir.display()))?
    {
        bail!("{} does not exist", projects_dir.display());
    }
    let entries = std::fs::read_dir(&projects_dir)
        .with_context(|| format!("reading {}", projects_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", projects_dir.display()))?;
        let transcript = entry.path().join(format!("{session_id}.jsonl"));
        if transcript
            .try_exists()
            .with_context(|| format!("checking {}", transcript.display()))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Where Claude keeps per-project transcripts: `$CLAUDE_CONFIG_DIR/projects`
/// when that is set and non-empty, else `~/.claude/projects`.
fn projects_dir() -> Result<PathBuf> {
    let config_dir = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var("HOME").context("HOME not set")?).join(".claude"),
    };
    Ok(config_dir.join("projects"))
}

fn path_to_slug(path: &std::path::Path) -> String {
    path.to_string_lossy().replace(['/', '.'], "-")
}
