use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

use crate::config;

pub enum SbplBreakage {
    Hide,
    Missing,
}

pub struct Args {
    pub mode: Option<String>,
    pub name: String,
    pub prompt: Option<String>,
    pub test_sbpl_breakage: Option<SbplBreakage>,
}

pub fn parse_args(raw: Vec<String>) -> Result<Args> {
    let mut iter = raw.into_iter().peekable();
    let mut mode = None;
    let mut name = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut test_sbpl_breakage = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mode" => {
                mode = Some(iter.next().context("--mode requires a value")?);
            }
            "--test-sbpl-breakage" => {
                let val = iter
                    .next()
                    .context("--test-sbpl-breakage requires a value")?;
                test_sbpl_breakage = Some(match val.as_str() {
                    "hide" => SbplBreakage::Hide,
                    "missing" => SbplBreakage::Missing,
                    other => bail!(
                        "--test-sbpl-breakage must be 'hide' or 'missing', got '{}'",
                        other
                    ),
                });
            }
            _ if arg.starts_with("--") => {
                bail!("unknown flag: {}", arg);
            }
            _ if name.is_none() => {
                name = Some(arg);
            }
            _ => {
                prompt_parts.push(arg);
            }
        }
    }

    let name = name.context("worktree name is required")?;
    let prompt = if prompt_parts.is_empty() {
        None
    } else {
        Some(prompt_parts.join(" "))
    };

    Ok(Args {
        mode,
        name,
        prompt,
        test_sbpl_breakage,
    })
}

pub fn run(args: Args) -> Result<i32> {
    let config = config::load()?;
    let mode = args.mode.unwrap_or_else(|| config.default_mode.clone());
    let mode_config = config
        .modes
        .get(&mode)
        .with_context(|| format!("unknown mode: {}", mode))?;

    let repo_root = repo_root()?;
    let worktree_path = repo_root
        .join(".claude")
        .join("worktrees")
        .join(sanitize_name(&args.name));

    rename_tmux_window(&args.name);
    update_trust(&repo_root)?;

    let binary_path = std::env::current_exe().context("resolving binary path")?;
    let _settings = write_hook_settings(&binary_path)?;
    let _sbpl_policy = write_sbpl_policy(&worktree_path, &repo_root, &sanitize_name(&args.name))?;

    let mut cmd = Command::new("claude");
    cmd.arg("--worktree").arg(&args.name);
    for flag in &mode_config.claude_flags {
        cmd.arg(flag);
    }
    cmd.arg("--settings").arg(&_settings.0);
    cmd.env("WTCLAUDE_SANDBOX", &worktree_path);
    match args.test_sbpl_breakage {
        None => {
            cmd.env("WTCLAUDE_SBPL", &_sbpl_policy.0);
        }
        Some(SbplBreakage::Hide) => {}
        Some(SbplBreakage::Missing) => {
            cmd.env(
                "WTCLAUDE_SBPL",
                format!("/tmp/wtclaude-sbpl-missing-{}.sb", std::process::id()),
            );
        }
    }
    if let Some(prompt) = args.prompt {
        cmd.arg(prompt);
    }

    let status = cmd.status().context("launching claude")?;
    Ok(status.code().unwrap_or(1))
}

fn sanitize_name(name: &str) -> String {
    // claude replaces '/' with '+' in worktree directory names
    name.replace('/', "+")
}

fn repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running git rev-parse")?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse failed: {}", msg.trim());
    }
    let path = String::from_utf8(output.stdout)
        .context("git output")?
        .trim()
        .to_string();
    Ok(PathBuf::from(path))
}

fn rename_tmux_window(name: &str) {
    if std::env::var("TMUX").is_err() {
        return;
    }
    if let Err(e) = Command::new("tmux").args(["rename-window", name]).status() {
        eprintln!("wtclaude: warning: tmux rename-window: {e}");
    }
}

fn update_trust(repo_root: &std::path::Path) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let claude_json = PathBuf::from(home).join(".claude.json");
    if !claude_json.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&claude_json).context("reading .claude.json")?;
    let mut json: serde_json::Value =
        serde_json::from_str(&content).context("parsing .claude.json")?;

    if !json["projects"].is_null() && !json["projects"].is_object() {
        bail!(".claude.json has unexpected structure: 'projects' is not an object");
    }

    let repo_str = repo_root.to_string_lossy().to_string();
    json["projects"][&repo_str]["hasTrustDialogAccepted"] = serde_json::Value::Bool(true);

    let updated = serde_json::to_string_pretty(&json)?;
    let tmp = claude_json.with_extension("json.tmp");
    std::fs::write(&tmp, updated).context("writing .claude.json (tmp)")?;
    std::fs::rename(&tmp, &claude_json).context("renaming .claude.json")?;
    Ok(())
}

fn write_sbpl_policy(sandbox: &Path, repo_root: &Path, worktree_name: &str) -> Result<TempFile> {
    let canonical = sandbox
        .canonicalize()
        .unwrap_or_else(|_| sandbox.to_path_buf());
    let git_dir = repo_root.join(".git");
    let git_dir_canonical = git_dir
        .canonicalize()
        .unwrap_or_else(|_| git_dir.to_path_buf());
    let _ = worktree_name; // git_dir covers worktrees/<name> as a subpath
    let policy = format!(
        "(version 1)\n(allow default)\n(deny file-write* (subpath \"/\"))\n(allow file-write* (literal \"/dev/null\"))\n(allow file-write* (subpath \"{}\"))\n(allow file-write* (subpath \"{}\"))\n",
        canonical.to_string_lossy(),
        git_dir_canonical.to_string_lossy(),
    );
    let path = PathBuf::from(format!("/tmp/wtclaude-sbpl-{}.sb", std::process::id()));
    std::fs::write(&path, policy).context("writing sbpl policy")?;
    Ok(TempFile(path))
}

fn write_hook_settings(binary_path: &std::path::Path) -> Result<TempFile> {
    let binary = binary_path.to_string_lossy();
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": ".*",
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("\"{}\" hook", binary)
                        }
                    ]
                }
            ],
            "PostToolUseFailure": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {
                            "type": "command",
                            "command": "printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PostToolUseFailure\",\"additionalContext\":\"Note: Bash commands in this session are wrapped in sandbox-exec. File writes are restricted to: %s. Commands writing outside this path fail with Operation not permitted.\"}}\n' \"$WTCLAUDE_SANDBOX\""
                        }
                    ]
                }
            ]
        }
    });

    let path = PathBuf::from(format!("/tmp/wtclaude-{}.json", std::process::id()));
    std::fs::write(&path, serde_json::to_string_pretty(&settings)?)
        .context("writing settings file")?;
    Ok(TempFile(path))
}
