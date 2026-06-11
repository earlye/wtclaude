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
    pub no_pull: bool,
    pub prompt: Option<String>,
    pub resume: Option<String>,
    pub test_sbpl_breakage: Option<SbplBreakage>,
}

pub fn parse_args(raw: Vec<String>) -> Result<Args> {
    let mut iter = raw.into_iter().peekable();
    let mut mode = None;
    let mut name = None;
    let mut no_pull = false;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut resume = None;
    let mut test_sbpl_breakage = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mode" => {
                mode = Some(iter.next().context("--mode requires a value")?);
            }
            "--no-pull" => {
                no_pull = true;
            }
            "--resume" => {
                resume = Some(iter.next().context("--resume requires a session id")?);
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
        no_pull,
        prompt,
        resume,
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

    if !args.no_pull {
        git_pull(&repo_root)?;
    }
    let _window_name = TmuxWindowName::rename(&args.name);
    update_trust(&repo_root)?;

    ensure_worktree(&worktree_path, &args.name, &repo_root)?;

    let canonical = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.clone());

    let binary_path = std::env::current_exe().context("resolving binary path")?;
    let _settings = write_hook_settings(&binary_path)?;
    let _sbpl_policy = write_sbpl_policy(&canonical, &repo_root, &sanitize_name(&args.name))?;

    let sandbox_notice = format!(
        "You are running in a git worktree sandbox for branch '{}'. \
         You may only write files under: {}.\n\n",
        args.name,
        canonical.display()
    );
    let prompt = match args.prompt {
        Some(p) => format!("{}{}", sandbox_notice, p),
        None => sandbox_notice,
    };

    let mut cmd = Command::new("claude");
    cmd.current_dir(&canonical);
    for flag in &mode_config.claude_flags {
        cmd.arg(flag);
    }
    if let Some(session_id) = args.resume {
        cmd.arg("--resume").arg(session_id);
    }
    cmd.arg("--settings").arg(&_settings.0);
    cmd.env("WTCLAUDE_SANDBOX", &canonical);
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
    cmd.arg(prompt);

    let status = cmd.status().context("launching claude")?;
    let exit_code = status.code().unwrap_or(1);

    if let Err(e) = post_exit_menu(&args.name, &worktree_path, &repo_root) {
        eprintln!("wtclaude: warning: post-exit menu: {e}");
    }

    Ok(exit_code)
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

fn git_pull(repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["remote"])
        .current_dir(repo_root)
        .output()
        .context("running git remote")?;
    if String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        return Ok(());
    }
    let status = Command::new("git")
        .args(["pull"])
        .current_dir(repo_root)
        .status()
        .context("running git pull")?;
    if !status.success() {
        bail!("git pull failed");
    }
    Ok(())
}

fn ensure_worktree(worktree_path: &Path, name: &str, repo_root: &Path) -> Result<()> {
    if worktree_path.exists() {
        if is_registered_worktree(worktree_path, repo_root)? {
            return Ok(());
        }
        bail!(
            "directory {} exists but is not a registered git worktree; remove it manually",
            worktree_path.display()
        );
    }

    // Try creating a new branch + worktree
    let out = Command::new("git")
        .args(["worktree", "add", "-b", name])
        .arg(worktree_path)
        .current_dir(repo_root)
        .output()
        .context("git worktree add")?;

    if out.status.success() {
        return Ok(());
    }

    // Branch already exists — check it out into the worktree
    let out = Command::new("git")
        .args(["worktree", "add"])
        .arg(worktree_path)
        .arg(name)
        .current_dir(repo_root)
        .output()
        .context("git worktree add (existing branch)")?;

    if out.status.success() {
        return Ok(());
    }

    let msg = String::from_utf8_lossy(&out.stderr);
    bail!("git worktree add failed: {}", msg.trim())
}

fn is_registered_worktree(path: &Path, repo_root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .context("git worktree list")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    for line in text.lines() {
        if let Some(wt_path) = line.strip_prefix("worktree ") {
            let wt_canonical = PathBuf::from(wt_path)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(wt_path));
            if wt_canonical == canonical {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn post_exit_menu(name: &str, worktree_path: &Path, repo_root: &Path) -> Result<()> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyModifiers},
        execute,
        style::Stylize,
        terminal::{self, ClearType},
    };
    use std::io::{self, Write};

    let labels = [
        format!("keep worktree {name}"),
        format!("remove worktree {name}"),
    ];
    let n = labels.len();
    let mut sel = 0usize;

    if terminal::enable_raw_mode().is_err() {
        return Ok(());
    }

    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
        }
    }
    let _guard = RawGuard;

    let mut stdout = io::stdout();

    let draw = |stdout: &mut io::Stdout, sel: usize| -> io::Result<()> {
        for (i, label) in labels.iter().enumerate() {
            execute!(
                stdout,
                terminal::Clear(ClearType::CurrentLine),
                cursor::MoveToColumn(0),
            )?;
            if i == sel {
                write!(stdout, "{}", format!("> {label}").reverse())?;
            } else {
                write!(stdout, "  {label}")?;
            }
            if i + 1 < labels.len() {
                write!(stdout, "\r\n")?;
            }
        }
        stdout.flush()
    };

    write!(stdout, "\r\n")?;
    draw(&mut stdout, sel)?;

    loop {
        match event::read()? {
            Event::Key(key) => match (key.code, key.modifiers) {
                (KeyCode::Up, _) => {
                    if sel > 0 {
                        sel -= 1;
                        execute!(stdout, cursor::MoveUp((n - 1) as u16))?;
                        draw(&mut stdout, sel)?;
                    }
                }
                (KeyCode::Down, _) => {
                    if sel < n - 1 {
                        sel += 1;
                        execute!(stdout, cursor::MoveUp((n - 1) as u16))?;
                        draw(&mut stdout, sel)?;
                    }
                }
                (KeyCode::Enter, _) => break,
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    sel = 0;
                    break;
                }
                _ => {}
            },
            _ => {}
        }
    }

    write!(stdout, "\r\n")?;
    stdout.flush()?;
    drop(_guard);

    if sel == 1 {
        remove_worktree(worktree_path, repo_root)?;
    }
    Ok(())
}

fn remove_worktree(worktree_path: &Path, repo_root: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["worktree", "remove"])
        .arg(worktree_path)
        .current_dir(repo_root)
        .status()
        .context("git worktree remove")?;
    if !status.success() {
        bail!("git worktree remove failed");
    }
    Ok(())
}

enum TmuxRestore {
    AutoRename,
    Name(String),
}

struct TmuxWindowName(Option<TmuxRestore>);

impl TmuxWindowName {
    fn rename(name: &str) -> Self {
        if std::env::var("TMUX").is_err() {
            return Self(None);
        }
        let auto_rename = Command::new("tmux")
            .args(["show-window-options", "-v", "automatic-rename"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() != "off")
            .unwrap_or(true);
        let restore = if auto_rename {
            TmuxRestore::AutoRename
        } else {
            let old = Command::new("tmux")
                .args(["display-message", "-p", "#W"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            TmuxRestore::Name(old)
        };
        if let Err(e) = Command::new("tmux").args(["rename-window", name]).status() {
            eprintln!("wtclaude: warning: tmux rename-window: {e}");
        }
        Self(Some(restore))
    }
}

impl Drop for TmuxWindowName {
    fn drop(&mut self) {
        match &self.0 {
            Some(TmuxRestore::AutoRename) => {
                if let Err(e) = Command::new("tmux")
                    .args(["set-window-option", "automatic-rename", "on"])
                    .status()
                {
                    eprintln!("wtclaude: warning: tmux set automatic-rename on: {e}");
                }
            }
            Some(TmuxRestore::Name(name)) => {
                if let Err(e) = Command::new("tmux").args(["rename-window", name]).status() {
                    eprintln!("wtclaude: warning: tmux rename-window (restore): {e}");
                }
            }
            None => {}
        }
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

    let home = std::env::var("HOME").context("HOME not set")?;
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());

    // Package manager cache/data dirs — caching only, not installers (no homebrew etc.)
    let pkg_cache_dirs = [
        ".cargo",
        ".rustup",
        ".npm",
        ".pnpm-store",
        ".local/share/pnpm",
        ".yarn",
        ".cache/yarn",
        ".cache/pip",
        ".cache/uv",
        ".cache/pypoetry",
        ".gem",
        ".bundle",
        ".m2",
        ".gradle",
        "go/pkg/mod",
        ".composer",
        ".nuget",
        ".conan2",
        ".docker",
    ];

    let mut lines = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        "(deny file-write* (subpath \"/\"))".to_string(),
        "(allow file-write* (literal \"/dev/null\"))".to_string(),
        format!(
            "(allow file-write* (subpath \"{}\"))",
            canonical.to_string_lossy()
        ),
        format!(
            "(allow file-write* (subpath \"{}\"))",
            git_dir_canonical.to_string_lossy()
        ),
        "(allow file-write* (subpath \"/tmp\"))".to_string(),
        format!("(allow file-write* (subpath \"{}\"))", tmpdir),
    ];

    for dir in &pkg_cache_dirs {
        lines.push(format!(
            "(allow file-write* (subpath \"{}/{}\"))",
            home, dir
        ));
    }

    let policy = lines.join("\n") + "\n";
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
