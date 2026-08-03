use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

use crate::config;

#[derive(Clone, Copy, clap::ValueEnum)]
enum SbplBreakage {
    Hide,
    Missing,
}

// main.rs injects an after_help listing the other subcommands (built live
// from the `Commands` enum) before parsing, so this struct's own `--help`
// documents the full command surface without hand-duplicating it here.
/// Launch a sandboxed Claude session in a worktree for WORKTREE_NAME.
#[derive(Parser)]
#[command(name = "wtclaude")]
pub struct Args {
    /// Operation mode (see wtclaude.yml)
    #[arg(long)]
    mode: Option<String>,
    /// Skip git pull before launch
    #[arg(long)]
    no_pull: bool,
    /// Resume a previous session
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,
    /// Print the generated sandbox policy and pause for Enter before launching
    #[arg(long)]
    show_policy: bool,
    /// Inject sandbox policy breakage for testing
    #[arg(long, value_enum, value_name = "TYPE")]
    test_sbpl_breakage: Option<SbplBreakage>,
    /// Name of the worktree/branch to launch
    #[arg(value_name = "WORKTREE_NAME")]
    name: String,
    /// Initial prompt to hand to claude (use `--` to pass prompt text that
    /// starts with a hyphen)
    #[arg(value_name = "INITIAL_PROMPT")]
    prompt_parts: Vec<String>,
}

impl Args {
    fn prompt(&self) -> Option<String> {
        if self.prompt_parts.is_empty() {
            None
        } else {
            Some(self.prompt_parts.join(" "))
        }
    }
}

pub fn run(args: Args) -> Result<i32> {
    let prompt = args.prompt();
    let config = config::load()?;
    let mode = args
        .mode
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
    let _sbpl_policy = write_sbpl_policy(&canonical, Some(&repo_root), &sanitize_name(&args.name))?;

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
    if let Some(p) = prompt {
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

// Note: when used as the `Commands::Headless` subcommand variant in
// main.rs, that variant's own doc comment overrides this struct's `name`/
// `about` (i.e. this struct's doc comment is inert there) — but other
// #[command(...)] attributes on this struct, if added, would still apply
// via clap's augment_args.
#[derive(Parser)]
pub struct HeadlessArgs {
    /// Operation mode (see wtclaude.yml)
    #[arg(long)]
    mode: Option<String>,
    /// Resume a previous session
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,
    /// Print the generated sandbox policy to stderr before launching
    #[arg(long)]
    show_policy: bool,
    /// Output format to pass through to `claude --print`
    #[arg(long, value_name = "FORMAT")]
    output_format: Option<String>,
    /// Pass --include-partial-messages through to `claude --print`
    #[arg(long)]
    include_partial_messages: bool,
    /// Pass --verbose through to `claude --print`
    #[arg(long)]
    verbose: bool,
    /// Prompt to hand to claude (read from stdin if omitted; use `--` to
    /// pass prompt text that starts with a hyphen)
    #[arg(value_name = "PROMPT")]
    prompt_parts: Vec<String>,
}

impl HeadlessArgs {
    fn prompt(&self) -> Option<String> {
        if self.prompt_parts.is_empty() {
            None
        } else {
            Some(self.prompt_parts.join(" "))
        }
    }
}

pub fn run_headless(args: HeadlessArgs) -> Result<i32> {
    let prompt = args.prompt();
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
    let _sbpl_policy = write_sbpl_policy(&canonical, Some(&repo_root), "headless")?;

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
    if let Some(output_format) = args.output_format {
        cmd.arg("--output-format").arg(output_format);
    }
    if args.include_partial_messages {
        cmd.arg("--include-partial-messages");
    }
    if args.verbose {
        cmd.arg("--verbose");
    }
    cmd.arg("--settings").arg(&_settings.0);
    cmd.arg("--append-system-prompt").arg(&sandbox_notice);
    cmd.env("WTCLAUDE_SANDBOX", &canonical);
    cmd.env("WTCLAUDE_REPO_ROOT", &repo_root);
    cmd.env("WTCLAUDE_SBPL", &_sbpl_policy.0);
    if let Some(p) = prompt {
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

// Note: when used as the `Commands::Path` subcommand variant in main.rs,
// that variant's own doc comment overrides this struct's `name`/`about`
// (i.e. this struct's doc comment is inert there) — but other
// #[command(...)] attributes on this struct, if added, would still apply
// via clap's augment_args.
#[derive(Parser)]
pub struct PathArgs {
    /// Operation mode (see wtclaude.yml)
    #[arg(long)]
    mode: Option<String>,
    /// Skip git pull before launch (only applies when DIRECTORY is inside a git repo)
    #[arg(long)]
    no_pull: bool,
    /// Resume a previous session
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,
    /// Print the generated sandbox policy and pause for Enter before launching
    #[arg(long)]
    show_policy: bool,
    /// Inject sandbox policy breakage for testing
    #[arg(long, value_enum, value_name = "TYPE")]
    test_sbpl_breakage: Option<SbplBreakage>,
    /// Directory to sandbox against and launch claude in (use `.` for the current directory)
    #[arg(value_name = "DIRECTORY")]
    directory: PathBuf,
    /// Initial prompt to hand to claude (use `--` to pass prompt text that
    /// starts with a hyphen)
    #[arg(value_name = "INITIAL_PROMPT")]
    prompt_parts: Vec<String>,
}

impl PathArgs {
    fn prompt(&self) -> Option<String> {
        if self.prompt_parts.is_empty() {
            None
        } else {
            Some(self.prompt_parts.join(" "))
        }
    }
}

/// Interactive, sandboxed launch against an arbitrary directory — no
/// worktree, no branch checkout. Unlike `run()`'s in-place mode (which
/// requires an existing git repo and sandboxes the whole repo_root) or
/// `run_headless()` (non-interactive, hardcoded to cwd), this works with or
/// without a surrounding git repo and sandboxes exactly `args.directory`.
pub fn run_path(args: PathArgs) -> Result<i32> {
    let prompt = args.prompt();
    let config = config::load()?;
    let mode = args
        .mode
        .or_else(|| std::env::var("WTCLAUDE_DEFAULT_MODE").ok())
        .unwrap_or_else(|| config.default_mode.clone());
    let mode_config = config
        .modes
        .get(&mode)
        .with_context(|| format!("unknown mode: {}", mode))?;

    match std::fs::metadata(&args.directory) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => bail!(
            "{} is not a directory (it's a file)",
            args.directory.display()
        ),
        Err(_) => bail!("{} does not exist", args.directory.display()),
    }
    let canonical = args
        .directory
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", args.directory.display()))?;

    if is_sandbox_nullifying_root(&canonical) {
        bail!(
            "refusing to sandbox against {}: this would allow writes to (almost) \
             any path, defeating the sandbox entirely. Pass a more specific directory.",
            canonical.display()
        );
    }

    let repo_root = match repo_root_at(&canonical) {
        Ok(root) => Some(root),
        Err(e) if e.to_string().contains("not a git repository") => None,
        Err(e) => return Err(e).context("resolving repo root for DIRECTORY"),
    };

    if let Some(repo_root) = &repo_root
        && !args.no_pull
    {
        git_pull(repo_root)?;
    }

    let _window_name = TmuxWindowName::rename(&directory_label(&canonical));
    update_trust(repo_root.as_deref().unwrap_or(&canonical))?;

    let binary_path = std::env::current_exe().context("resolving binary path")?;
    let _settings = write_hook_settings(&binary_path)?;
    let _sbpl_policy = write_sbpl_policy(&canonical, repo_root.as_deref(), "path")?;

    if args.show_policy {
        let policy = std::fs::read_to_string(&_sbpl_policy.0).context("reading sbpl policy")?;
        println!("{}", policy);
        print!("Press Enter to continue...");
        use std::io::{self, BufRead, Write};
        io::stdout().flush()?;
        let _ = io::stdin().lock().lines().next();
    }

    let sandbox_notice = format!(
        "You are running in a wtclaude directory sandbox rooted at: {}. You may only write \
         files under that directory (plus a few package-manager cache dirs{}). {}",
        canonical.display(),
        if repo_root.is_some() {
            ", and the repo's .git"
        } else {
            ""
        },
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
    // Explicitly cleared (not just left unset) when there's no repo: this
    // process may itself be running inside another wtclaude sandbox, whose
    // own WTCLAUDE_REPO_ROOT would otherwise leak through to the child via
    // ambient env inheritance.
    match &repo_root {
        Some(repo_root) => cmd.env("WTCLAUDE_REPO_ROOT", repo_root),
        None => cmd.env_remove("WTCLAUDE_REPO_ROOT"),
    };
    match args.test_sbpl_breakage {
        None => {
            cmd.env("WTCLAUDE_SBPL", &_sbpl_policy.0);
        }
        // Cleared, not just left unset, for the same ambient-leak reason as
        // WTCLAUDE_REPO_ROOT above: this simulates "policy unavailable",
        // which an inherited WTCLAUDE_SBPL from an outer wtclaude sandbox
        // would otherwise quietly defeat.
        Some(SbplBreakage::Hide) => {
            cmd.env_remove("WTCLAUDE_SBPL");
        }
        Some(SbplBreakage::Missing) => {
            cmd.env(
                "WTCLAUDE_SBPL",
                format!("/tmp/wtclaude-sbpl-missing-{}.sb", std::process::id()),
            );
        }
    }
    if let Some(p) = prompt {
        cmd.arg(p);
    }

    let status = cmd.status().context("launching claude")?;
    Ok(status.code().unwrap_or(1))
}

/// Rejects a canonicalized DIRECTORY that would make the sandbox a no-op:
/// the filesystem root, or the user's home directory (or any ancestor of
/// it, e.g. `~/..`) — matching `config::validate_allowlist`'s precedent for
/// refusing sandbox-nullifying input rather than silently allow-writing
/// almost everything. Compares filesystem identity (device + inode) rather
/// than path strings: a string comparison — even a canonicalized one — is
/// still fooled by macOS APFS's case-insensitive-but-case-preserving
/// default (`/users/x` and `/Users/x` canonicalize to different strings but
/// are the same file), which `home.ancestors()` walks past `/` itself, so
/// the explicit filesystem-root check falls out of this for free.
fn is_sandbox_nullifying_root(canonical: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(target) = std::fs::metadata(canonical) else {
        return false;
    };
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let Ok(home) = Path::new(&home).canonicalize() else {
        // HOME is set but can't be resolved (e.g. an unmounted network
        // home) — fail closed rather than silently trust DIRECTORY.
        return true;
    };
    home.ancestors().any(|ancestor| {
        std::fs::metadata(ancestor)
            .map(|m| m.dev() == target.dev() && m.ino() == target.ino())
            .unwrap_or(false)
    })
}

/// The tmux window title for `path` mode: the canonicalized directory's
/// basename, not whatever string the user actually typed (e.g. `path .`
/// labels the window with the real directory name, not `.`).
fn directory_label(canonical: &Path) -> String {
    canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| canonical.to_string_lossy().to_string())
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

/// Like `repo_root()`, but resolved relative to an arbitrary directory
/// rather than the process's own cwd — used by `path` mode, whose DIRECTORY
/// argument may not be where `wtclaude` itself was invoked from.
fn repo_root_at(dir: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        // run_path's caller pattern-matches this error's text for "not a
        // git repository" to distinguish a real failure from "just not a
        // repo" — git translates that message under a non-C locale, which
        // would silently break repo-less `path` mode for non-English
        // users. Force English output so the match is locale-independent.
        .env("LC_ALL", "C")
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

    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("is already checked out at") || stderr.contains("already used by worktree") {
        let existing = find_worktree_for_branch(name, repo_root)?;
        match existing {
            Some(path) => bail!(
                "branch '{name}' is already checked out in another worktree at {}\n\
                 cd there instead, or pick a different branch name.",
                path.display()
            ),
            None => bail!(
                "branch '{name}' is already checked out in another worktree, \
                 but its location could not be determined: {}",
                stderr.trim()
            ),
        }
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

fn find_worktree_for_branch(name: &str, repo_root: &Path) -> Result<Option<PathBuf>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .context("git worktree list")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let target_ref = format!("refs/heads/{name}");

    let mut current_path: Option<&str> = None;
    for line in text.lines() {
        if let Some(wt_path) = line.strip_prefix("worktree ") {
            current_path = Some(wt_path);
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            if branch_ref == target_ref {
                if let Some(wt_path) = current_path {
                    let canonical = PathBuf::from(wt_path)
                        .canonicalize()
                        .unwrap_or_else(|_| PathBuf::from(wt_path));
                    return Ok(Some(canonical));
                }
            }
        }
    }
    Ok(None)
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

/// Resolves the git directories that need to be writable for sandbox-policy
/// purposes: the per-worktree git-dir (`HEAD`, `index`, `FETCH_HEAD`,
/// `MERGE_HEAD`, ...) and the git-common-dir shared across worktrees
/// (`objects`, `packed-refs`, `config`, ...). For a normal checkout these
/// are both `<repo_root>/.git`. For a linked worktree (`git worktree add`),
/// `<repo_root>/.git` is instead a pointer file, and the two real
/// directories live under the main repository's `.git` and
/// `.git/worktrees/<name>/` — asking git itself (rather than re-parsing the
/// `gitdir:`/`commondir` indirection by hand) follows both correctly.
fn git_dirs(repo_root: &Path) -> Vec<PathBuf> {
    let fallback = || vec![repo_root.join(".git")];
    let Ok(output) = Command::new("git")
        .args(["rev-parse", "--git-dir", "--git-common-dir"])
        .current_dir(repo_root)
        .output()
    else {
        return fallback();
    };
    if !output.status.success() {
        return fallback();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let p = PathBuf::from(line.trim());
            if p.is_absolute() {
                p
            } else {
                repo_root.join(p)
            }
        })
        .collect()
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

pub(crate) fn generate_sbpl_policy(sandbox: &Path, repo_root: Option<&Path>) -> Result<String> {
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
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
        PathBuf::from(&tmpdir),
    ]
    .into_iter()
    .chain(repo_root.map(git_dirs).unwrap_or_default())
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

fn write_sbpl_policy(
    sandbox: &Path,
    repo_root: Option<&Path>,
    _worktree_name: &str,
) -> Result<TempFile> {
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

    // git canonicalizes symlinked ancestors (e.g. macOS's /tmp -> /private/tmp)
    // in its own `rev-parse` output, so test dirs must be canonicalized too —
    // otherwise comparisons against git_dirs()'s output spuriously mismatch.
    fn unique_test_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wtclaude-test-{}-{}", std::process::id(), label));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn git_dirs_returns_dot_git_for_a_normal_repo() {
        let root = unique_test_dir("git-dirs-normal-repo");
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );

        assert!(git_dirs(&root).iter().all(|d| *d == root.join(".git")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn git_dirs_returns_both_the_worktree_dir_and_the_shared_common_dir() {
        let main_repo = unique_test_dir("git-dirs-main-repo");
        let worktree = unique_test_dir("git-dirs-linked-worktree");
        std::fs::remove_dir_all(&worktree).unwrap(); // `git worktree add` must create this itself

        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&main_repo)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        };
        run(&["init", "-q"]);
        std::fs::write(main_repo.join("f.txt"), "x").unwrap();
        run(&["add", "f.txt"]);
        run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        run(&[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "git-dirs-test-branch",
        ]);

        let dirs = git_dirs(&worktree);

        assert_eq!(dirs.len(), 2);
        assert!(dirs.contains(&main_repo.join(".git")));
        assert!(
            dirs.iter()
                .any(|p| p.starts_with(main_repo.join(".git").join("worktrees")))
        );

        run(&["worktree", "remove", "-f", worktree.to_str().unwrap()]);
        std::fs::remove_dir_all(&main_repo).unwrap();
    }

    #[test]
    fn ensure_worktree_reports_the_existing_path_when_branch_is_checked_out_elsewhere() {
        // Regression test: requesting a worktree under a *new* directory name
        // for a branch that's already checked out somewhere else used to
        // surface git's confusing final-fallback error ("a branch named 'X'
        // already exists") instead of pointing at where the branch actually
        // lives.
        let main_repo = unique_test_dir("ensure-worktree-main-repo");
        let existing_worktree = unique_test_dir("ensure-worktree-existing");
        let new_worktree = unique_test_dir("ensure-worktree-new");
        std::fs::remove_dir_all(&existing_worktree).unwrap();
        std::fs::remove_dir_all(&new_worktree).unwrap();

        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&main_repo)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        };
        run(&["init", "-q"]);
        std::fs::write(main_repo.join("f.txt"), "x").unwrap();
        run(&["add", "f.txt"]);
        run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        run(&[
            "worktree",
            "add",
            "-q",
            existing_worktree.to_str().unwrap(),
            "-b",
            "the-branch",
        ]);

        let err =
            ensure_worktree(&new_worktree, "the-branch", &main_repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&existing_worktree.to_string_lossy().to_string()),
            "error should point at the existing worktree path, got: {msg}"
        );
        assert!(!new_worktree.exists());

        run(&[
            "worktree",
            "remove",
            "-f",
            existing_worktree.to_str().unwrap(),
        ]);
        std::fs::remove_dir_all(&main_repo).unwrap();
    }

    #[test]
    fn generate_sbpl_policy_omits_repo_git_dirs_when_repo_root_is_none() {
        let repo_root = unique_test_dir("generate-sbpl-no-repo-root");
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo_root)
                .status()
                .unwrap()
                .success()
        );
        let sandbox = unique_test_dir("generate-sbpl-no-repo-root-sandbox");

        let with_repo = generate_sbpl_policy(&sandbox, Some(&repo_root)).unwrap();
        let without_repo = generate_sbpl_policy(&sandbox, None).unwrap();

        let git_dir = repo_root.join(".git").to_string_lossy().to_string();
        assert!(
            with_repo.contains(&git_dir),
            "expected repo .git dir to be allowlisted when repo_root is Some"
        );
        assert!(
            !without_repo.contains(&git_dir),
            "expected repo .git dir to be absent when repo_root is None"
        );

        std::fs::remove_dir_all(&repo_root).unwrap();
        std::fs::remove_dir_all(&sandbox).unwrap();
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Args, clap::Error> {
        let mut full = vec!["wtclaude"];
        full.extend_from_slice(args);
        Args::try_parse_from(full)
    }

    #[test]
    fn parse_args_defaults_are_empty() {
        let parsed = parse(&["myworktree"]).unwrap();
        assert_eq!(parsed.name, "myworktree");
        assert!(parsed.mode.is_none());
        assert!(!parsed.no_pull);
        assert!(parsed.resume.is_none());
        assert!(!parsed.show_policy);
        assert!(parsed.test_sbpl_breakage.is_none());
        assert!(parsed.prompt().is_none());
    }

    #[test]
    fn parse_args_errors_on_missing_worktree_name() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn parse_args_captures_no_pull() {
        let parsed = parse(&["--no-pull", "myworktree"]).unwrap();
        assert!(parsed.no_pull);
    }

    #[test]
    fn parse_args_joins_interleaved_flags_and_prompt() {
        let parsed = parse(&[
            "--mode",
            "fast",
            "myworktree",
            "do",
            "the",
            "thing",
            "--resume",
            "abc",
        ])
        .unwrap();
        assert_eq!(parsed.mode.as_deref(), Some("fast"));
        assert_eq!(parsed.name, "myworktree");
        assert_eq!(parsed.prompt().as_deref(), Some("do the thing"));
        assert_eq!(parsed.resume.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_args_accepts_hyphen_leading_prompt_text_after_dash_dash() {
        let parsed = parse(&["myworktree", "--", "-1 fix this"]).unwrap();
        assert_eq!(parsed.prompt().as_deref(), Some("-1 fix this"));
    }

    #[test]
    fn parse_args_accepts_test_sbpl_breakage_values() {
        let hide = parse(&["--test-sbpl-breakage", "hide", "myworktree"]).unwrap();
        assert!(matches!(hide.test_sbpl_breakage, Some(SbplBreakage::Hide)));

        let missing = parse(&["--test-sbpl-breakage", "missing", "myworktree"]).unwrap();
        assert!(matches!(
            missing.test_sbpl_breakage,
            Some(SbplBreakage::Missing)
        ));
    }

    #[test]
    fn parse_args_rejects_invalid_test_sbpl_breakage_value() {
        assert!(parse(&["--test-sbpl-breakage", "bogus", "myworktree"]).is_err());
    }

    #[test]
    fn parse_args_errors_on_unknown_flag() {
        assert!(parse(&["--nonsense", "myworktree"]).is_err());
    }
}

#[cfg(test)]
mod headless_tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<HeadlessArgs, clap::Error> {
        let mut full = vec!["wtclaude-headless"];
        full.extend_from_slice(args);
        HeadlessArgs::try_parse_from(full)
    }

    #[test]
    fn parse_headless_args_joins_interleaved_flags_and_prompt() {
        let parsed = parse(&["--mode", "fast", "do", "the", "thing", "--resume", "abc"]).unwrap();
        assert_eq!(parsed.mode.as_deref(), Some("fast"));
        assert_eq!(parsed.prompt().as_deref(), Some("do the thing"));
        assert_eq!(parsed.resume.as_deref(), Some("abc"));
        assert!(!parsed.show_policy);
    }

    #[test]
    fn parse_headless_args_accepts_hyphen_leading_prompt_text_after_dash_dash() {
        // A prompt that itself starts with a hyphen (e.g. "-1 fix this")
        // looks like an unknown flag to clap; per clap convention (and its
        // own error tip), `--` escapes the rest of argv as literal values.
        let parsed = parse(&["--mode", "fast", "--", "-1 fix this"]).unwrap();
        assert_eq!(parsed.mode.as_deref(), Some("fast"));
        assert_eq!(parsed.prompt().as_deref(), Some("-1 fix this"));
    }

    #[test]
    fn parse_headless_args_show_policy_does_not_consume_a_value() {
        let parsed = parse(&["--show-policy", "explain this"]).unwrap();
        assert!(parsed.show_policy);
        assert_eq!(parsed.prompt().as_deref(), Some("explain this"));
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
        assert!(parsed.prompt().is_none());
        assert!(parsed.resume.is_none());
        assert!(!parsed.show_policy);
        assert!(parsed.output_format.is_none());
        assert!(!parsed.include_partial_messages);
        assert!(!parsed.verbose);
    }

    #[test]
    fn parse_headless_args_captures_output_format() {
        let parsed = parse(&["--output-format", "json", "hello"]).unwrap();
        assert_eq!(parsed.output_format.as_deref(), Some("json"));
        assert_eq!(parsed.prompt().as_deref(), Some("hello"));
    }

    #[test]
    fn parse_headless_args_errors_on_missing_output_format_value() {
        assert!(parse(&["--output-format"]).is_err());
    }

    #[test]
    fn parse_headless_args_captures_include_partial_messages() {
        let parsed = parse(&["--include-partial-messages", "hello"]).unwrap();
        assert!(parsed.include_partial_messages);
        assert_eq!(parsed.prompt().as_deref(), Some("hello"));
    }

    #[test]
    fn parse_headless_args_captures_verbose() {
        let parsed = parse(&["--verbose", "hello"]).unwrap();
        assert!(parsed.verbose);
        assert_eq!(parsed.prompt().as_deref(), Some("hello"));
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<PathArgs, clap::Error> {
        let mut full = vec!["wtclaude-path"];
        full.extend_from_slice(args);
        PathArgs::try_parse_from(full)
    }

    #[test]
    fn parse_path_args_defaults_are_empty() {
        let parsed = parse(&["."]).unwrap();
        assert_eq!(parsed.directory, PathBuf::from("."));
        assert!(parsed.mode.is_none());
        assert!(!parsed.no_pull);
        assert!(parsed.resume.is_none());
        assert!(!parsed.show_policy);
        assert!(parsed.test_sbpl_breakage.is_none());
        assert!(parsed.prompt().is_none());
    }

    #[test]
    fn parse_path_args_errors_on_missing_directory() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn parse_path_args_captures_no_pull() {
        let parsed = parse(&["--no-pull", "."]).unwrap();
        assert!(parsed.no_pull);
    }

    #[test]
    fn parse_path_args_joins_interleaved_flags_and_prompt() {
        let parsed = parse(&[
            "--mode", "fast", ".", "do", "the", "thing", "--resume", "abc",
        ])
        .unwrap();
        assert_eq!(parsed.mode.as_deref(), Some("fast"));
        assert_eq!(parsed.directory, PathBuf::from("."));
        assert_eq!(parsed.prompt().as_deref(), Some("do the thing"));
        assert_eq!(parsed.resume.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_path_args_accepts_hyphen_leading_prompt_text_after_dash_dash() {
        let parsed = parse(&[".", "--", "-1 fix this"]).unwrap();
        assert_eq!(parsed.prompt().as_deref(), Some("-1 fix this"));
    }

    #[test]
    fn parse_path_args_accepts_test_sbpl_breakage_values() {
        let hide = parse(&["--test-sbpl-breakage", "hide", "."]).unwrap();
        assert!(matches!(hide.test_sbpl_breakage, Some(SbplBreakage::Hide)));

        let missing = parse(&["--test-sbpl-breakage", "missing", "."]).unwrap();
        assert!(matches!(
            missing.test_sbpl_breakage,
            Some(SbplBreakage::Missing)
        ));
    }

    #[test]
    fn parse_path_args_errors_on_unknown_flag() {
        assert!(parse(&["--nonsense", "."]).is_err());
    }

    #[test]
    fn run_path_bails_on_nonexistent_directory() {
        let missing = std::env::temp_dir().join(format!(
            "wtclaude-path-test-nonexistent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing);
        let args = parse(&[missing.to_str().unwrap()]).unwrap();
        let err = run_path(args).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "error: {err}");
    }

    #[test]
    fn run_path_bails_on_a_regular_file() {
        let file = std::env::temp_dir().join(format!(
            "wtclaude-path-test-regular-file-{}",
            std::process::id()
        ));
        std::fs::write(&file, "not a directory").unwrap();
        let args = parse(&[file.to_str().unwrap()]).unwrap();
        let err = run_path(args).unwrap_err();
        assert!(
            err.to_string().contains("is not a directory"),
            "error: {err}"
        );
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn run_path_bails_on_filesystem_root() {
        let args = parse(&["/"]).unwrap();
        let err = run_path(args).unwrap_err();
        assert!(
            err.to_string().contains("refusing to sandbox"),
            "error: {err}"
        );
    }

    #[test]
    fn is_sandbox_nullifying_root_rejects_filesystem_root() {
        assert!(is_sandbox_nullifying_root(Path::new("/")));
    }

    #[test]
    fn is_sandbox_nullifying_root_rejects_home_directory_exactly() {
        let home = std::env::var("HOME").unwrap();
        assert!(is_sandbox_nullifying_root(Path::new(&home)));
    }

    #[test]
    fn is_sandbox_nullifying_root_allows_a_normal_subdirectory() {
        let home = std::env::var("HOME").unwrap();
        let sub = PathBuf::from(home).join("some-project");
        assert!(!is_sandbox_nullifying_root(&sub));
    }

    #[test]
    fn is_sandbox_nullifying_root_rejects_an_ancestor_of_home() {
        // `~/..` canonicalizes to HOME's parent, e.g. `/Users` — still broad
        // enough to nullify the sandbox for every user on the machine, not
        // just the current one, so this must be rejected too.
        let home = std::env::var("HOME").unwrap();
        let home_path = Path::new(&home).canonicalize().unwrap();
        let parent = home_path.parent().expect("HOME should have a parent");
        assert!(is_sandbox_nullifying_root(parent));
    }

    #[test]
    fn is_sandbox_nullifying_root_does_not_reject_a_sibling_of_home() {
        let home = std::env::var("HOME").unwrap();
        let home_path = Path::new(&home).canonicalize().unwrap();
        let parent = home_path.parent().expect("HOME should have a parent");
        let sibling = parent.join("wtclaude-test-definitely-not-a-real-user-dir");
        assert!(!is_sandbox_nullifying_root(&sibling));
    }

    #[test]
    fn is_sandbox_nullifying_root_rejects_a_case_variant_of_home() {
        // Regression test: a string comparison (even canonicalized) is
        // fooled by macOS APFS's default case-insensitive-but-case-
        // preserving behavior — `/users/x` and `/Users/x` canonicalize to
        // different strings yet name the identical file. Only meaningful
        // when the filesystem actually behaves that way; skipped as a
        // harmless no-op otherwise rather than failing on an unusual setup.
        let home = std::env::var("HOME").unwrap();
        let home_path = Path::new(&home).canonicalize().unwrap();
        let flipped: String = home_path
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else if c.is_ascii_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        let flipped_path = PathBuf::from(&flipped);
        if flipped_path != home_path && std::fs::metadata(&flipped_path).is_ok() {
            assert!(is_sandbox_nullifying_root(&flipped_path));
        }
    }

    #[test]
    fn directory_label_uses_basename_not_literal_dot() {
        let dir =
            std::env::temp_dir().join(format!("wtclaude-path-test-label-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.canonicalize().unwrap();
        let expected = canonical.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(directory_label(&canonical), expected);
        assert_ne!(expected, ".");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn directory_label_falls_back_to_full_path_when_no_file_name() {
        assert_eq!(directory_label(Path::new("/")), "/");
    }
}
