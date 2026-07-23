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
    pub show_policy: bool,
    pub test_sbpl_breakage: Option<SbplBreakage>,
}

pub fn parse_args(raw: Vec<String>) -> Result<Args> {
    let mut iter = raw.into_iter().peekable();
    let mut mode = None;
    let mut name = None;
    let mut no_pull = false;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut resume = None;
    let mut show_policy = false;
    let mut test_sbpl_breakage = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mode" => {
                mode = Some(iter.next().context("--mode requires a value")?);
            }
            "--no-pull" => {
                no_pull = true;
            }
            "--show-policy" => {
                show_policy = true;
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
        show_policy,
        test_sbpl_breakage,
    })
}

pub fn run(args: Args) -> Result<i32> {
    let config = config::load()?;
    let mode = args.mode
        .or_else(|| std::env::var("WTCLAUDE_DEFAULT_MODE").ok())
        .unwrap_or_else(|| config.default_mode.clone());
    let mode_config = config
        .modes
        .get(&mode)
        .with_context(|| format!("unknown mode: {}", mode))?;

    let repo_root = match repo_root() {
        Ok(r) => r,
        Err(_) => {
            let cwd = std::env::current_dir().context("getting current directory")?;
            if !offer_git_init(&cwd)? {
                bail!("no git repository found; run 'git init' to initialize one");
            }
            cwd
        }
    };

    let fresh = is_fresh_repo(&repo_root);
    let in_place = fresh
        || current_branch()
            .ok()
            .map(|b| b == args.name)
            .unwrap_or(false);

    if !args.no_pull {
        git_pull(&repo_root)?;
    }
    let _window_name = TmuxWindowName::rename(&args.name);
    update_trust(&repo_root)?;

    let canonical = if in_place {
        repo_root.canonicalize().unwrap_or_else(|_| repo_root.clone())
    } else {
        let worktree_path = repo_root
            .join(".claude")
            .join("worktrees")
            .join(sanitize_name(&args.name));
        ensure_worktree(&worktree_path, &args.name, &repo_root)?;
        worktree_path
            .canonicalize()
            .unwrap_or_else(|_| worktree_path.clone())
    };

    let binary_path = std::env::current_exe().context("resolving binary path")?;
    let _settings = write_hook_settings(&binary_path)?;
    let _sbpl_policy = write_sbpl_policy(&canonical, &repo_root, &sanitize_name(&args.name))?;

    if args.show_policy {
        let policy = std::fs::read_to_string(&_sbpl_policy.0).context("reading sbpl policy")?;
        println!("{}", policy);
        print!("Press Enter to continue...");
        use std::io::{self, BufRead, Write};
        io::stdout().flush()?;
        let _ = io::stdin().lock().lines().next();
    }

    let sandbox_notice = format!(
        "You are running in a git worktree sandbox for branch '{}'. \
         You may only write files under: {}. {}",
        args.name,
        canonical.display(),
        sandbox_warning_common()
    );

    let mut cmd = Command::new("claude");
    cmd.current_dir(&canonical);
    for flag in &mode_config.claude_flags {
        cmd.arg(flag);
    }
    if let Some(session_id) = args.resume {
        cmd.arg("--resume").arg(session_id);
    }
    cmd.arg("--settings").arg(&_settings.0);
    cmd.arg("--append-system-prompt").arg(&sandbox_notice);
    cmd.env("WTCLAUDE_SANDBOX", &canonical);
    cmd.env("WTCLAUDE_REPO_ROOT", &repo_root);
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
    if let Some(p) = args.prompt {
        cmd.arg(p);
    }

    let status = cmd.status().context("launching claude")?;
    let exit_code = status.code().unwrap_or(1);

    if !in_place {
        let worktree_path = repo_root
            .join(".claude")
            .join("worktrees")
            .join(sanitize_name(&args.name));
        if let Err(e) = post_exit_menu(&args.name, &worktree_path, &repo_root) {
            eprintln!("wtclaude: warning: post-exit menu: {e}");
        }
    }

    Ok(exit_code)
}

fn sandbox_warning_common() -> &'static str {
    "Do not attempt to create new worktrees (e.g. via `git worktree add`, `wtclaude new`, \
     or an EnterWorktree/spawn-agent-in-worktree tool) from within this sandbox: creating a \
     worktree requires writing to the main repository's `.git` directory, which is outside \
     this sandbox and will be rejected. \
     Avoid heredocs (e.g. `cat <<'EOF' ... EOF`) nested inside a `$(...)` command \
     substitution inside double quotes — the common `git commit -m \"$(cat <<'EOF' ... \
     EOF)\"` idiom included. Apple's bash 3.2 (which backs both /bin/sh and /bin/bash on \
     macOS, and is what this sandbox's `sh -c` wrapper runs) has a real parsing bug there: \
     depending on the exact single-quote/backslash content of the heredoc body, it can \
     miscount quote nesting and fail with 'unexpected EOF while looking for matching' or a \
     syntax error, even though the same text is valid POSIX shell and works fine in zsh. \
     This is unrelated to sandboxing and reproduces with no sandbox involved at all. Prefer \
     writing multi-line content (like a commit message) to a temp file and using it \
     directly, e.g. `git commit -F /tmp/msg.txt`, instead of the heredoc-in-command-\
     substitution pattern."
}

pub struct HeadlessArgs {
    mode: Option<String>,
    prompt: Option<String>,
    resume: Option<String>,
    show_policy: bool,
}

pub fn parse_headless_args(raw: Vec<String>) -> Result<HeadlessArgs> {
    let mut iter = raw.into_iter().peekable();
    let mut mode = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut resume = None;
    let mut show_policy = false;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mode" => {
                mode = Some(iter.next().context("--mode requires a value")?);
            }
            "--show-policy" => {
                show_policy = true;
            }
            "--resume" => {
                resume = Some(iter.next().context("--resume requires a session id")?);
            }
            _ if arg.starts_with("--") => {
                bail!("unknown flag: {}", arg);
            }
            _ => {
                prompt_parts.push(arg);
            }
        }
    }

    let prompt = if prompt_parts.is_empty() {
        None
    } else {
        Some(prompt_parts.join(" "))
    };

    Ok(HeadlessArgs {
        mode,
        prompt,
        resume,
        show_policy,
    })
}

pub fn run_headless(args: HeadlessArgs) -> Result<i32> {
    let config = config::load()?;
    let mode = args
        .mode
        .or_else(|| std::env::var("WTCLAUDE_DEFAULT_MODE").ok())
        .unwrap_or_else(|| config.default_mode.clone());
    let mode_config = config
        .modes
        .get(&mode)
        .with_context(|| format!("unknown mode: {}", mode))?;

    let repo_root = repo_root()
        .context("resolving repo root (headless mode requires an existing git repository)")?;
    let cwd = std::env::current_dir().context("getting current directory")?;
    let canonical = cwd.canonicalize().unwrap_or_else(|e| {
        eprintln!(
            "wtclaude: warning: could not canonicalize sandbox root {}: {e}",
            cwd.display()
        );
        cwd.clone()
    });

    update_trust(&repo_root)?;

    let binary_path = std::env::current_exe().context("resolving binary path")?;
    let _settings = write_hook_settings(&binary_path)?;
    let _sbpl_policy = write_sbpl_policy(&canonical, &repo_root, "headless")?;

    if args.show_policy {
        // Printed to stderr, not stdout, so it never mixes into `claude
        // --print`'s own stdout output (headless mode's passthrough result).
        let policy = std::fs::read_to_string(&_sbpl_policy.0).context("reading sbpl policy")?;
        eprintln!("{}", policy);
    }

    let sandbox_notice = format!(
        "You are running in a headless wtclaude sandbox rooted at: {}. \
         You may only write files under that directory (plus the repo's .git and a few \
         package-manager cache dirs). {}",
        canonical.display(),
        sandbox_warning_common()
    );

    let mut cmd = Command::new("claude");
    cmd.current_dir(&canonical);
    for flag in &mode_config.claude_flags {
        cmd.arg(flag);
    }
    if let Some(session_id) = args.resume {
        cmd.arg("--resume").arg(session_id);
    }
    cmd.arg("--print");
    cmd.arg("--settings").arg(&_settings.0);
    cmd.arg("--append-system-prompt").arg(&sandbox_notice);
    cmd.env("WTCLAUDE_SANDBOX", &canonical);
    cmd.env("WTCLAUDE_REPO_ROOT", &repo_root);
    cmd.env("WTCLAUDE_SBPL", &_sbpl_policy.0);
    if let Some(p) = args.prompt {
        cmd.arg(p);
    }

    let status = cmd.status().context("launching claude")?;
    match status.code() {
        Some(code) => Ok(code),
        None => {
            use std::os::unix::process::ExitStatusExt;
            match status.signal() {
                Some(signal) => {
                    eprintln!("wtclaude: claude was terminated by signal {signal}");
                    Ok(128 + signal)
                }
                None => {
                    eprintln!("wtclaude: claude exited with an unrecognized status ({status:?})");
                    Ok(1)
                }
            }
        }
    }
}

fn sanitize_name(name: &str) -> String {
    // claude replaces '/' with '+' in worktree directory names
    name.replace('/', "+")
}

fn offer_git_init(dir: &Path) -> Result<bool> {
    use std::io::{self, Write};
    print!(
        "No git repository found in {}. Run 'git init'? [Y/n] ",
        dir.display()
    );
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
        let status = Command::new("git")
            .arg("init")
            .current_dir(dir)
            .status()
            .context("running git init")?;
        if !status.success() {
            bail!("git init failed");
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

fn is_fresh_repo(repo_root: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output();
    match out {
        Ok(o) => !o.status.success(),
        Err(_) => true,
    }
}

fn current_branch() -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("running git rev-parse")?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse failed: {}", msg.trim());
    }
    Ok(String::from_utf8(output.stdout)
        .context("git output")?
        .trim()
        .to_string())
}

pub(crate) fn repo_root() -> Result<PathBuf> {
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
        .args(["fetch"])
        .current_dir(repo_root)
        .status()
        .context("running git fetch")?;
    if !status.success() {
        bail!("git fetch failed");
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

    // Branch already exists locally — check it out into the worktree
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

    // Try creating a new branch tracking the remote
    let remote_ref = format!("origin/{}", name);
    let out = Command::new("git")
        .args(["worktree", "add", "--track", "-b", name])
        .arg(worktree_path)
        .arg(&remote_ref)
        .current_dir(repo_root)
        .output()
        .context("git worktree add (remote branch)")?;

    if out.status.success() {
        return Ok(());
    }

    // No remote — create a new local branch at HEAD
    let out = Command::new("git")
        .args(["worktree", "add", "-b", name])
        .arg(worktree_path)
        .current_dir(repo_root)
        .output()
        .context("git worktree add")?;

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
        queue,
        style::{Print, Stylize},
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
        queue!(stdout, cursor::Hide)?;
        for (i, label) in labels.iter().enumerate() {
            queue!(
                stdout,
                terminal::Clear(ClearType::CurrentLine),
                cursor::MoveToColumn(0),
            )?;
            if i == sel {
                queue!(stdout, Print(format!("> {label}").reverse()))?;
            } else {
                queue!(stdout, Print(format!("  {label}")))?;
            }
            if i + 1 < labels.len() {
                queue!(stdout, Print("\r\n"))?;
            }
        }
        queue!(stdout, cursor::Show)?;
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
                        queue!(stdout, cursor::MoveUp((n - 1) as u16))?;
                        draw(&mut stdout, sel)?;
                    }
                }
                (KeyCode::Down, _) => {
                    if sel < n - 1 {
                        sel += 1;
                        queue!(stdout, cursor::MoveUp((n - 1) as u16))?;
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
        use std::io::{self, Write};
        if let Ok(out) = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree_path)
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = text.lines().collect();
            for line in lines.iter().take(10) {
                println!("{}", line);
            }
            if lines.len() > 10 {
                println!("(… and {} more)", lines.len() - 10);
            }
        }
        print!("git worktree remove failed. Force-remove? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim().eq_ignore_ascii_case("y") {
            let force_status = Command::new("git")
                .args(["worktree", "remove", "-f"])
                .arg(worktree_path)
                .current_dir(repo_root)
                .status()
                .context("git worktree remove -f")?;
            if !force_status.success() {
                bail!("git worktree remove -f failed");
            }
        }
    }
    Ok(())
}

enum TmuxRestore {
    AutoRename,
    Name(String),
}

struct TmuxWindowName(Option<TmuxWindowNameState>);

struct TmuxWindowNameState {
    restore: TmuxRestore,
    pane: String,
}

impl TmuxWindowName {
    fn rename(name: &str) -> Self {
        if std::env::var("TMUX").is_err() {
            return Self(None);
        }
        // Captured once, at the pane this process's tty belongs to. Unlike
        // tmux's own "current window" (which follows whatever the user last
        // selected, possibly in another pane), $TMUX_PANE is fixed for the
        // lifetime of this process, so it still identifies the right window
        // even if the user switches panes/windows while wtclaude is starting.
        let pane = match std::env::var("TMUX_PANE") {
            Ok(p) => p,
            Err(_) => return Self(None),
        };
        let auto_rename = Command::new("tmux")
            .args(["show-window-options", "-t", &pane, "-v", "automatic-rename"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() != "off")
            .unwrap_or(true);
        let restore = if auto_rename {
            TmuxRestore::AutoRename
        } else {
            let old = Command::new("tmux")
                .args(["display-message", "-t", &pane, "-p", "#W"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            TmuxRestore::Name(old)
        };
        if let Err(e) = Command::new("tmux")
            .args(["rename-window", "-t", &pane, name])
            .status()
        {
            eprintln!("wtclaude: warning: tmux rename-window: {e}");
        }
        Self(Some(TmuxWindowNameState { restore, pane }))
    }
}

impl Drop for TmuxWindowName {
    fn drop(&mut self) {
        let Some(state) = &self.0 else { return };
        match &state.restore {
            TmuxRestore::AutoRename => {
                if let Err(e) = Command::new("tmux")
                    .args(["set-window-option", "-t", &state.pane, "automatic-rename", "on"])
                    .status()
                {
                    eprintln!("wtclaude: warning: tmux set automatic-rename on: {e}");
                }
            }
            TmuxRestore::Name(name) => {
                if let Err(e) = Command::new("tmux")
                    .args(["rename-window", "-t", &state.pane, name])
                    .status()
                {
                    eprintln!("wtclaude: warning: tmux rename-window (restore): {e}");
                }
            }
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

/// Resolve a literal allowlist/sandbox path for the SBPL `subpath` rule it
/// becomes. Delegates to `config::resolve_existing_prefix`, which is a
/// strict superset of a plain `canonicalize().unwrap_or(p)`: identical
/// result when `p` exists in full, but also walks up to the longest
/// existing ancestor and canonicalizes *that* when it doesn't — e.g. a
/// literal entry for a not-yet-created directory under `/tmp` (symlinked to
/// `/private/tmp` on macOS) still resolves through the symlink instead of
/// silently compiling into a `subpath` rule that can never match the
/// kernel-resolved path. Keeps literal-entry resolution consistent with
/// `resolve_glob_prefix` and `hook.rs`'s `normalize_path`.
fn resolve(p: PathBuf) -> PathBuf {
    config::resolve_existing_prefix(&p)
}

/// Resolves the git metadata directory that actually needs to be writable
/// for sandbox-policy purposes. For a normal checkout, `<repo_root>/.git`
/// already is that directory. For a linked worktree (`git worktree add`),
/// `<repo_root>/.git` is instead a pointer file containing `gitdir: <path>`,
/// and the real metadata — `FETCH_HEAD`, the per-worktree index, `HEAD`,
/// `MERGE_HEAD`, etc. — lives at that path, under the main repo's
/// `.git/worktrees/<name>/`.
fn git_metadata_dir(repo_root: &Path) -> PathBuf {
    let dot_git = repo_root.join(".git");
    if dot_git.is_file()
        && let Ok(contents) = std::fs::read_to_string(&dot_git)
        && let Some(gitdir) = contents.trim().strip_prefix("gitdir:")
    {
        let gitdir = PathBuf::from(gitdir.trim());
        return if gitdir.is_absolute() {
            gitdir
        } else {
            repo_root.join(gitdir)
        };
    }
    dot_git
}

/// Translate a shell-style glob (`*` only, usable anywhere in the pattern)
/// into a whole-string-anchored SBPL regex. `subpath` matches by path
/// component, not by glob/wildcard, so it can't express `.tmpXXXX`-style
/// siblings of a file — entries like `~/.claude.json*` need `regex` instead.
fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '.' | '^' | '$' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    out
}

/// Escapes a string for embedding inside an SBPL `#"..."` string literal.
/// `glob_to_regex`'s output can itself contain backslashes (from escaping
/// regex metacharacters); those and any literal `"` both need escaping here
/// so the emitted profile stays valid SBPL rather than truncating the
/// string literal or failing to parse.
fn sbpl_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Splits `allowlist` into entries containing a `*` glob and plain literal
/// entries, expanding `~` to `home` in both. Pure and parameterized by
/// `home` (rather than reading `$HOME` itself) so it's directly unit
/// testable without mutating global process state.
fn partition_allowlist(allowlist: &[String], home: &str) -> (Vec<String>, Vec<String>) {
    allowlist
        .iter()
        .map(|p| p.replace('~', home))
        .partition(|p| p.contains('*'))
}

pub(crate) fn generate_sbpl_policy(sandbox: &Path, repo_root: &Path) -> Result<String> {
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
        "Library/Caches/cargo-xwin",
    ];

    let user_config = config::load_user()?;

    // Entries containing `*` are globs (e.g. `~/.claude.json*` covering the
    // file plus its atomic-write tmp siblings) — `canonicalize`/`subpath`
    // can't express those, since no real path literally contains a `*`.
    // Route them to a separate `regex` rule instead of the subpath pipeline.
    let (glob_allow, literal_allow) = partition_allowlist(&user_config.allowlist, &home);

    let raw: Vec<PathBuf> = [
        sandbox.to_path_buf(),
        repo_root.join(".git"),
        git_metadata_dir(repo_root),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
        PathBuf::from(&tmpdir),
    ]
    .into_iter()
    .chain(pkg_cache_dirs.iter().map(|d| PathBuf::from(&home).join(d)))
    .chain(literal_allow.into_iter().map(PathBuf::from))
    .collect();

    let mut seen = std::collections::HashSet::new();
    let allow_paths: Vec<PathBuf> = raw
        .into_iter()
        .map(resolve)
        .filter(|p| seen.insert(p.clone()))
        .collect();

    let socket_paths: Vec<PathBuf> = user_config
        .socket_allowlist
        .iter()
        .map(|p| resolve(PathBuf::from(p.replace("~", &home))))
        .collect();

    let mut lines = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        "(deny file-write* (subpath \"/\"))".to_string(),
        "(allow file-write* (literal \"/dev/null\"))".to_string(),
        // Keychain: allow writes so tools like saml2aws can store tokens
        format!("(allow file-write* (subpath \"{}/Library/Keychains\"))", home),
        // osascript / AppleEvents support
        "(allow process-exec* (literal \"/usr/bin/osascript\"))".to_string(),
        "(allow appleevent-send)".to_string(),
    ];

    for p in &allow_paths {
        lines.push(format!(
            "(allow file-write* (subpath \"{}\"))",
            p.to_string_lossy()
        ));
    }

    for pattern in &glob_allow {
        // Resolve the pattern's static prefix the same way literal entries
        // are resolved (line ~717 above): Seatbelt evaluates rules against
        // the kernel-resolved, symlink-free path, so a glob rooted under a
        // symlinked prefix (e.g. anything under /tmp) would otherwise
        // compile into a regex that can never match — silently permitting
        // nothing, the exact bug class this glob support exists to fix.
        let resolved_pattern = config::resolve_glob_prefix(pattern);
        lines.push(format!(
            "(allow file-write* (regex #\"{}\"))",
            sbpl_string_escape(&glob_to_regex(&resolved_pattern))
        ));
    }

    for p in &socket_paths {
        let s = p.to_string_lossy();
        lines.push(format!("(allow file-write* (subpath \"{s}\"))"));
        lines.push(format!("(allow network-outbound (subpath \"{s}\"))"));
        lines.push(format!("(allow network-bind    (subpath \"{s}\"))"));
    }

    Ok(lines.join("\n") + "\n")
}

fn write_sbpl_policy(sandbox: &Path, repo_root: &Path, _worktree_name: &str) -> Result<TempFile> {
    let policy = generate_sbpl_policy(sandbox, repo_root)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_resolves_a_symlinked_existing_ancestor_for_a_nonexistent_literal_entry() {
        // Regression test: a literal (non-glob) allowlist entry for a path
        // that doesn't exist yet under /tmp (symlinked to /private/tmp on
        // macOS) must still resolve through that symlink — otherwise the
        // emitted SBPL `subpath` rule can never match the kernel-resolved
        // path, silently denying Bash writes that Write/Edit/NotebookEdit
        // (via hook.rs's normalize_path) would allow for the same entry.
        let resolved = resolve(PathBuf::from(
            "/tmp/wtclaude-test-nonexistent-literal-entry",
        ));
        assert_eq!(
            resolved,
            PathBuf::from("/private/tmp/wtclaude-test-nonexistent-literal-entry")
        );
    }

    #[test]
    fn glob_to_regex_translates_trailing_star_to_anchored_dotstar() {
        // Regression test: `~/.claude.json*` in wtclaude.yml must produce a
        // regex that matches the base file, not just literal-`*` paths that
        // never exist (the bug that made the entry a silent no-op).
        assert_eq!(
            glob_to_regex("/Users/earlye/.claude.json*"),
            r"^/Users/earlye/\.claude\.json.*$"
        );
    }

    #[test]
    fn glob_to_regex_escapes_regex_metacharacters() {
        assert_eq!(glob_to_regex("/a/b.c*"), r"^/a/b\.c.*$");
    }

    #[test]
    fn glob_to_regex_escapes_every_listed_metacharacter() {
        assert_eq!(
            glob_to_regex(r"a.b^c$d+e?f(g)h[i]j{k}l|m\n"),
            r"^a\.b\^c\$d\+e\?f\(g\)h\[i\]j\{k\}l\|m\\n$"
        );
    }

    #[test]
    fn glob_to_regex_handles_leading_and_middle_wildcards() {
        assert_eq!(glob_to_regex("*.log"), r"^.*\.log$");
        assert_eq!(glob_to_regex("/a/foo*bar"), r"^/a/foo.*bar$");
    }

    #[test]
    fn sbpl_string_escape_escapes_backslash_and_quote() {
        assert_eq!(sbpl_string_escape(r#"a\b"c"#), r#"a\\b\"c"#);
    }

    #[test]
    fn partition_allowlist_splits_glob_from_literal_and_expands_tilde() {
        let allowlist = vec![
            "~/.claude.json*".to_string(),
            "~/.cargo".to_string(),
            "/tmp/plain".to_string(),
        ];
        let (glob, literal) = partition_allowlist(&allowlist, "/Users/x");
        assert_eq!(glob, vec!["/Users/x/.claude.json*".to_string()]);
        assert_eq!(
            literal,
            vec!["/Users/x/.cargo".to_string(), "/tmp/plain".to_string()]
        );
    }

    #[test]
    fn git_metadata_dir_returns_dot_git_when_it_is_a_real_directory() {
        let root = std::env::temp_dir().join(format!(
            "wtclaude-test-normal-repo-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".git")).unwrap();

        assert_eq!(git_metadata_dir(&root), root.join(".git"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn git_metadata_dir_follows_gitdir_pointer_for_a_linked_worktree() {
        let root = std::env::temp_dir().join(format!(
            "wtclaude-test-linked-worktree-{}",
            std::process::id()
        ));
        let main_repo_worktree_dir = std::env::temp_dir().join(format!(
            "wtclaude-test-main-repo-worktrees-name-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&main_repo_worktree_dir).unwrap();
        std::fs::write(
            root.join(".git"),
            format!("gitdir: {}\n", main_repo_worktree_dir.display()),
        )
        .unwrap();

        assert_eq!(git_metadata_dir(&root), main_repo_worktree_dir);

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&main_repo_worktree_dir).unwrap();
    }
}

#[cfg(test)]
mod headless_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<HeadlessArgs> {
        parse_headless_args(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn parse_headless_args_joins_interleaved_flags_and_prompt() {
        let parsed = parse(&["--mode", "fast", "do", "the", "thing", "--resume", "abc"]).unwrap();
        assert_eq!(parsed.mode.as_deref(), Some("fast"));
        assert_eq!(parsed.prompt.as_deref(), Some("do the thing"));
        assert_eq!(parsed.resume.as_deref(), Some("abc"));
        assert!(!parsed.show_policy);
    }

    #[test]
    fn parse_headless_args_show_policy_does_not_consume_a_value() {
        let parsed = parse(&["--show-policy", "explain this"]).unwrap();
        assert!(parsed.show_policy);
        assert_eq!(parsed.prompt.as_deref(), Some("explain this"));
    }

    #[test]
    fn parse_headless_args_errors_on_missing_mode_value() {
        assert!(parse(&["--mode"]).is_err());
    }

    #[test]
    fn parse_headless_args_errors_on_missing_resume_value() {
        assert!(parse(&["--resume"]).is_err());
    }

    #[test]
    fn parse_headless_args_errors_on_unknown_flag() {
        assert!(parse(&["--nonsense"]).is_err());
    }

    #[test]
    fn parse_headless_args_defaults_are_empty() {
        let parsed = parse(&[]).unwrap();
        assert!(parsed.mode.is_none());
        assert!(parsed.prompt.is_none());
        assert!(parsed.resume.is_none());
        assert!(!parsed.show_policy);
    }
}
