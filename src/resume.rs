//! `wtclaude resume-all`: after a reboot or power loss, reopen every
//! worktree whose Claude session isn't running, each in a tmux window
//! (opened by `src window`) that resumes its newest session, or starts a
//! fresh one when it has no transcript. `wtclaude resume` does the same for
//! the worktrees of the repository it runs in.

use anyhow::{Context, Result, bail};
use clap::Args;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::hook::resolve_on_path;
use crate::launch::{sanitize_name, shell_quote};
use crate::sessions::projects_dir;

/// Arguments shared by `resume-all` and `resume`.
#[derive(Args)]
pub struct ResumeArgs {
    /// Print the report without opening any windows
    #[arg(long)]
    pub dry_run: bool,
}

/// How much of each transcript is read before falling back to all of it.
const TAIL_BYTES: u64 = 64 * 1024;

const WORKTREES_MARKER: &str = "/.claude/worktrees/";

/// `wtclaude resume-all`: every repo with a worktree transcript.
pub fn run_all(args: ResumeArgs) -> Result<i32> {
    run(args, "resume-all", None)
}

/// `wtclaude resume`: only the repo the current directory belongs to.
pub fn run_current(args: ResumeArgs) -> Result<i32> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let repo = current_repo_root(&cwd)?;
    run(args, "resume", Some(&repo))
}

/// Resumes the worktrees of `only`, a canonical repo root, or of every repo
/// when it's None. `command` names the subcommand in errors.
fn run(args: ResumeArgs, command: &str, only: Option<&Path>) -> Result<i32> {
    let path_var = match std::env::var("PATH") {
        Ok(path_var) => path_var,
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(e) => bail!("reading PATH: {e}"),
    };
    let (startish, startish_note) = resolve_tool("startish", "", &path_var, args.dry_run)?;
    let (src, src_note) = resolve_tool(
        "src",
        &format!("; {command} opens windows through `src window`"),
        &path_var,
        args.dry_run,
    )?;

    let projects_dir = projects_dir()?;
    let mut discovery = discover(&projects_dir, TAIL_BYTES)?;
    if let Some(repo) = only {
        keep_repo(&mut discovery, repo);
    }
    let mut report = sweep(&System { src }, discovery, &startish, args.dry_run)?;
    report.notes = [startish_note, src_note].into_iter().flatten().collect();
    print!("{}", report.render(&projects_dir));
    Ok(report.exit_code())
}

/// The main repo root of the git repository containing `dir`, canonical as
/// discovery's repo roots are. Inside a worktree, `--show-toplevel` would
/// give the worktree, so this takes the parent of the common git dir, which
/// is the main repo's `.git` from any of its worktrees.
fn current_repo_root(dir: &Path) -> Result<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let common_dir = output_of(cmd, "git rev-parse")
        .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
    let common_dir = PathBuf::from(common_dir.trim_end_matches('\n'));
    let root = common_dir
        .parent()
        .with_context(|| format!("git dir {} has no parent", common_dir.display()))?;
    Ok(canonical(root))
}

/// Drops every session outside `repo`, a canonical repo root. Compares
/// roots rather than project dir names, since slugs are lossy: `a.b` and
/// `a-b` share one.
fn keep_repo(discovery: &mut Discovery, repo: &Path) {
    discovery.sessions.retain(|s| s.repo_root == repo);
}

/// The absolute path of `word` on PATH, resolved up front so a missing
/// tool stops the run once instead of failing every worktree. `startish`
/// needs it anyway: the window gets the tmux server's environment, whose
/// PATH may not include ~/.cargo/bin. On a dry run, which opens no window,
/// a missing tool falls back to the bare word and adds a note to the
/// report. `why` is appended to the error.
fn resolve_tool(
    word: &str,
    why: &str,
    path_var: &str,
    dry_run: bool,
) -> Result<(PathBuf, Option<String>)> {
    match resolve_on_path(word, path_var) {
        Some(p) => {
            let p = std::path::absolute(&p).with_context(|| format!("resolving {word}"))?;
            Ok((p, None))
        }
        None if dry_run => Ok((
            PathBuf::from(word),
            Some(format!(
                "{word} not found on PATH; a real run would stop here"
            )),
        )),
        None => bail!("{word} not found on PATH{why}"),
    }
}

/// The commands `resume-all` runs, behind a trait so tests can fake them.
/// Each except `src` returns the command's stdout, or an error carrying its
/// exit status and its stderr (or stdout if stderr is empty).
pub(crate) trait External {
    /// `claude agents --json`
    fn claude_agents(&self) -> Result<String>;
    /// `git worktree list --porcelain`, run in `repo`
    fn git_worktree_list(&self, repo: &Path) -> Result<String>;
    /// The number of a merged PR whose head is `branch`, or empty output.
    fn gh_merged_pr(&self, repo: &Path, branch: &str) -> Result<String>;
    /// `src ARGS…`. Err only if `src` couldn't be run at all; a non-zero
    /// exit comes back as an unsuccessful output.
    fn src(&self, args: &[String]) -> Result<SrcOutput>;
}

pub(crate) struct SrcOutput {
    pub success: bool,
    /// The exit status as `ExitStatus` displays it, e.g. `exit status: 1`.
    pub status: String,
    pub stdout: String,
    pub stderr: String,
}

struct System {
    /// `src`, resolved on PATH.
    src: PathBuf,
}

/// Why a command failed: its stderr (or stdout when stderr is empty)
/// followed by the exit status, or just the exit status when both are
/// empty, as when it was killed by a signal.
fn failure_reason(status: &str, stdout: &str, stderr: &str) -> String {
    let text = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|t| !t.is_empty());
    match text {
        Some(text) => format!("{text} ({status})"),
        None => status.to_string(),
    }
}

/// Runs `cmd`, returning its stdout, or why it failed as the error.
fn output_of(mut cmd: Command, what: &str) -> Result<String> {
    let output = cmd.output().with_context(|| format!("running {what}"))?;
    if !output.status.success() {
        bail!(
            "{what} failed: {}",
            failure_reason(
                &output.status.to_string(),
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
            )
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl External for System {
    fn claude_agents(&self) -> Result<String> {
        let mut cmd = Command::new("claude");
        cmd.args(["agents", "--json"]);
        output_of(cmd, "claude agents --json")
    }

    fn git_worktree_list(&self, repo: &Path) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(repo)
            .args(["worktree", "list", "--porcelain"]);
        output_of(cmd, "git worktree list")
    }

    fn gh_merged_pr(&self, repo: &Path, branch: &str) -> Result<String> {
        // Run in the repo so gh picks its remote.
        let mut cmd = Command::new("gh");
        cmd.current_dir(repo).args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "merged",
            "--json",
            "number",
            "--jq",
            ".[0].number // empty",
        ]);
        output_of(cmd, "gh pr list")
    }

    fn src(&self, args: &[String]) -> Result<SrcOutput> {
        let output = Command::new(&self.src)
            .args(args)
            .output()
            .context("running src")?;
        Ok(SrcOutput {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Where a transcript's newest usable line says its session was.
#[derive(Debug, PartialEq)]
struct LastLine {
    /// Inside a worktree.
    cwd: PathBuf,
    /// `gitBranch`, kept only to report a mismatch with git's view.
    branch: Option<String>,
    session_id: String,
}

/// The newest session on one worktree.
#[derive(Debug)]
struct Session {
    repo_root: PathBuf,
    worktree: PathBuf,
    transcript_branch: Option<String>,
    session_id: String,
    /// Older sessions on the same worktree, newest first; not resumed.
    older: Vec<String>,
}

#[derive(Debug, Default)]
struct Discovery {
    scanned: usize,
    skipped: Vec<(PathBuf, String)>,
    sessions: Vec<Session>,
}

/// Splits a `cwd` inside `<repo>/.claude/worktrees/<dir>` into the repo root
/// and `<dir>`, dropping anything below the worktree. It splits at
/// the last marker, so for a worktree nested inside another, the outer
/// worktree is the repo root: wtclaude run inside a worktree nests
/// `.claude/worktrees/` under it, so the outer worktree is where that
/// branch's directory lives and where `wtclaude <branch>` would look.
fn split_worktree_cwd(cwd: &str) -> Option<(PathBuf, String)> {
    let at = cwd.rfind(WORKTREES_MARKER)?;
    let repo_root = &cwd[..at];
    let dir = cwd[at + WORKTREES_MARKER.len()..].split('/').next()?;
    if repo_root.is_empty() || dir.is_empty() {
        return None;
    }
    Some((PathBuf::from(repo_root), dir.to_string()))
}

fn worktree_path(repo_root: &Path, dir: &str) -> PathBuf {
    repo_root.join(".claude").join("worktrees").join(dir)
}

/// The canonical repo root and worktree of a path inside a worktree. Split
/// before canonicalizing, so a symlinked `.claude/worktrees/<dir>` keeps
/// its place under the repo; the worktree is derived from the canonical
/// root, so two spellings of the repo give one key. Transcripts, `claude
/// agents` and `git worktree list` all go through this, so their paths
/// compare equal.
fn worktree_key(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let (repo_root, dir) = split_worktree_cwd(&path.to_string_lossy())?;
    let repo_root = canonical(&repo_root);
    let worktree = worktree_path(&repo_root, &dir);
    Some((repo_root, worktree))
}

/// The newest line of `data` that is a JSON object with a `cwd` inside a
/// worktree. A missing `sessionId` falls back to `stem`, the transcript's
/// file name, which is the session id.
fn newest_usable_line(data: &[u8], stem: &str) -> Option<LastLine> {
    String::from_utf8_lossy(data)
        .lines()
        .rev()
        .find_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let text = |key: &str| {
                value
                    .get(key)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            };
            let cwd = text("cwd")?;
            split_worktree_cwd(cwd)?;
            Some(LastLine {
                cwd: PathBuf::from(cwd),
                branch: text("gitBranch").map(String::from),
                session_id: text("sessionId").unwrap_or(stem).to_string(),
            })
        })
}

/// The newest usable line of the transcript at `path`, looked for in its
/// last `tail` bytes, then in the whole file if the tail has none.
fn read_transcript(path: &Path, tail: u64) -> Result<Option<LastLine>> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut data = Vec::new();
    file.seek(SeekFrom::Start(size.saturating_sub(tail)))?;
    file.read_to_end(&mut data)?;
    if let Some(line) = newest_usable_line(&data, &stem) {
        return Ok(Some(line));
    }
    if size <= tail {
        return Ok(None);
    }
    data.clear();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut data)?;
    Ok(newest_usable_line(&data, &stem))
}

/// Reads `<projects_dir>/*--claude-worktrees-*/*.jsonl`. Only top-level
/// files, which leaves out subagent transcripts in subdirectories. Repo
/// roots are canonicalized and each worktree path is derived from its
/// root, so two spellings that differ only by symlinks group together.
/// Bind mounts are not unified.
fn discover(projects_dir: &Path, tail: u64) -> Result<Discovery> {
    let mut discovery = Discovery::default();
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(discovery),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", projects_dir.display()));
        }
    };
    let mut found: Vec<(SystemTime, LastLine)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                discovery
                    .skipped
                    .push((projects_dir.to_path_buf(), e.to_string()));
                continue;
            }
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .contains("--claude-worktrees-")
        {
            continue;
        }
        // Follows symlinks, as the transcripts' own paths do.
        match std::fs::metadata(entry.path()) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => continue,
            Err(e) => {
                discovery.skipped.push((entry.path(), e.to_string()));
                continue;
            }
        }
        let transcripts = match std::fs::read_dir(entry.path()) {
            Ok(transcripts) => transcripts,
            Err(e) => {
                discovery.skipped.push((entry.path(), e.to_string()));
                continue;
            }
        };
        for transcript in transcripts {
            let path = match transcript {
                Ok(transcript) => transcript.path(),
                Err(e) => {
                    discovery.skipped.push((entry.path(), e.to_string()));
                    continue;
                }
            };
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let metadata = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => m,
                Ok(_) => continue,
                // Deleted since the listing, or a dangling symlink.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    discovery.skipped.push((path, e.to_string()));
                    continue;
                }
            };
            discovery.scanned += 1;
            let mtime = match metadata.modified() {
                Ok(mtime) => mtime,
                Err(e) => {
                    discovery.skipped.push((path, e.to_string()));
                    continue;
                }
            };
            match read_transcript(&path, tail) {
                Ok(Some(line)) => found.push((mtime, line)),
                Ok(None) => discovery
                    .skipped
                    .push((path, "no line with a worktree cwd".to_string())),
                Err(e) => discovery.skipped.push((path, format!("{e:#}"))),
            }
        }
    }

    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut by_worktree: BTreeMap<PathBuf, Session> = BTreeMap::new();
    for (_, line) in found {
        let Some((repo_root, worktree)) = worktree_key(&line.cwd) else {
            continue;
        };
        match by_worktree.get_mut(&worktree) {
            Some(newer) => newer.older.push(line.session_id),
            None => {
                by_worktree.insert(
                    worktree.clone(),
                    Session {
                        repo_root,
                        worktree,
                        transcript_branch: line.branch,
                        session_id: line.session_id,
                        older: Vec::new(),
                    },
                );
            }
        }
    }
    discovery.sessions = by_worktree.into_values().collect();
    Ok(discovery)
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, PartialEq)]
struct WorktreeEntry {
    path: PathBuf,
    /// Without `refs/heads/`; None when detached.
    branch: Option<String>,
    prunable: bool,
}

fn parse_porcelain(output: &str) -> Vec<WorktreeEntry> {
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            entries.push(WorktreeEntry {
                path: PathBuf::from(path),
                branch: None,
                prunable: false,
            });
        } else if let Some(entry) = entries.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
                entry.branch = Some(branch.to_string());
            } else if line == "prunable" || line.starts_with("prunable ") {
                entry.prunable = true;
            }
        }
    }
    entries
}

/// The running sessions, per `claude agents --json`.
#[derive(Debug, Default, PartialEq)]
struct Live {
    /// Each cwd's worktree, by the rule discovery uses, or the cwd itself
    /// when it isn't in one.
    worktrees: HashSet<PathBuf>,
    session_ids: HashSet<String>,
}

impl Live {
    /// Whether a Claude runs in `worktree`, or runs `session` wherever it
    /// was started.
    fn covers(&self, worktree: &Path, session: Option<&Session>) -> bool {
        self.worktrees.contains(worktree)
            || session.is_some_and(|s| self.session_ids.contains(&s.session_id))
    }
}

/// Reads `claude agents --json`. An entry without a string `cwd` is an
/// error: dropping it would make its session look dead. `sessionId` is
/// optional.
fn parse_live(json: &str) -> Result<Live> {
    let agents: Vec<serde_json::Value> =
        serde_json::from_str(json).context("parsing claude agents --json")?;
    let mut live = Live::default();
    for a in &agents {
        let Some(cwd) = a.get("cwd").and_then(|c| c.as_str()) else {
            bail!("claude agents --json: an entry has no string cwd: {a}");
        };
        // The cwd as given first, so a symlinked worktree directory keeps
        // its place; canonical only for a cwd that reaches a worktree
        // through a symlink outside one.
        let cwd = Path::new(cwd);
        let canonical_cwd = canonical(cwd);
        let worktree = worktree_key(cwd)
            .or_else(|| worktree_key(&canonical_cwd))
            .map_or(canonical_cwd, |(_, worktree)| worktree);
        live.worktrees.insert(worktree);
        if let Some(id) = a.get("sessionId").and_then(|i| i.as_str()) {
            live.session_ids.insert(id.to_string());
        }
    }
    Ok(live)
}

/// Whether the worktree directory is the one wtclaude would derive from
/// `branch`. If not, `wtclaude <branch>` would look elsewhere and its
/// `git worktree add` would fail with "already checked out".
fn dir_matches_branch(worktree: &Path, branch: &str) -> bool {
    worktree
        .file_name()
        .is_some_and(|name| name.to_string_lossy() == sanitize_name(branch))
}

/// The arguments to `src` that open a window in the repo's session running
/// `wtclaude --no-pull [--resume SESSION_ID] BRANCH` under `startish`. The
/// window starts in the repo root: started inside a worktree, wtclaude
/// would nest `.claude/worktrees/`. `wtclaude` stays a bare word because it
/// runs in the user's interactive shell, after the rc file has set PATH and
/// defined the wtclaude wrapper. The `--history` entry omits `--resume`;
/// the wrapper pushes the resume line itself on exit.
fn src_window_args(
    repo_root: &Path,
    branch: &str,
    session_id: Option<&str>,
    startish: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "window".into(),
        repo_root.to_string_lossy().into_owned(),
        "-n".into(),
        branch.into(),
        "--".into(),
        startish.to_string_lossy().into_owned(),
        "--history".into(),
        format!("wtclaude --no-pull {}", shell_quote(branch)),
        "--".into(),
        "wtclaude".into(),
        "--no-pull".into(),
    ];
    if let Some(session_id) = session_id {
        args.extend(["--resume".into(), session_id.into()]);
    }
    args.push(branch.into());
    args
}

/// Splits a successful `src window`'s stderr into the warnings it prints
/// for each existing window that already has the new window's name
/// (`Warning: session S already has a window named N (@id)`, without the
/// `Warning: ` prefix), and every other non-empty line.
fn split_src_stderr(stderr: &str) -> (Vec<String>, Vec<String>) {
    let mut stale = Vec::new();
    let mut other = Vec::new();
    for line in stderr.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line.strip_prefix("Warning: ") {
            Some(w) if w.starts_with("session ") && w.contains(" already has a window named ") => {
                stale.push(w.to_string())
            }
            _ => other.push(line.to_string()),
        }
    }
    (stale, other)
}

/// Resolves symlinks where the path exists, so paths from transcripts, git
/// and `claude agents` compare equal.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One line of the report, within a repo: a worktree, named by its branch,
/// or by its path when it's detached or its directory doesn't match the
/// branch, or `(whole repo)` when `git worktree list` failed. `detail` says
/// whatever else is worth saying.
#[derive(Debug, PartialEq)]
struct Entry {
    repo: PathBuf,
    what: String,
    detail: String,
}

impl Entry {
    fn new(repo: &Path, what: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            repo: repo.to_path_buf(),
            what: what.into(),
            detail: detail.into(),
        }
    }

    /// Appends `note` to the detail, if there is one.
    fn with_note(mut self, note: Option<&str>) -> Self {
        if let Some(note) = note {
            if !self.detail.is_empty() {
                self.detail.push_str("; ");
            }
            self.detail.push_str(note);
        }
        self
    }
}

#[derive(Debug, Default)]
struct Report {
    dry_run: bool,
    /// Set when a dry run couldn't find `startish` or `src`.
    notes: Vec<String>,
    resumed: Vec<Entry>,
    fresh: Vec<Entry>,
    failed: Vec<Entry>,
    live: Vec<Entry>,
    merged: Vec<Entry>,
    /// Branches whose merge check failed and were treated as unmerged.
    merge_unchecked: Vec<Entry>,
    stale_windows: Vec<Entry>,
    left_alone: Vec<Entry>,
    scanned: usize,
    /// Transcripts and project directories that couldn't be read or used.
    skipped: Vec<(PathBuf, String)>,
}

/// Filters its worktrees (those with transcripts, plus any under
/// `.claude/worktrees/` git lists without one) and launches (or, on a dry
/// run, only lists) a window for each survivor.
fn sweep(
    ext: &dyn External,
    discovery: Discovery,
    startish: &Path,
    dry_run: bool,
) -> Result<Report> {
    let mut report = Report {
        dry_run,
        scanned: discovery.scanned,
        skipped: discovery.skipped,
        ..Report::default()
    };
    if discovery.sessions.is_empty() {
        return Ok(report);
    }

    // Without a live list every running session would look dead and get a
    // second Claude, so a failure here stops everything.
    let live = ext
        .claude_agents()
        .and_then(|json| parse_live(&json))
        .context("cannot tell which worktrees are live")?;

    // Discovery canonicalized each repo root, so each repo has one bucket.
    let mut repos: BTreeMap<&Path, Vec<&Session>> = BTreeMap::new();
    for session in &discovery.sessions {
        repos.entry(&session.repo_root).or_default().push(session);
    }

    for (repo_root, sessions) in repos {
        // A missing repo root is skipped silently, like a gone worktree; one
        // that can't be checked (say, an unmounted network mount) fails.
        match std::fs::metadata(repo_root) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                report
                    .failed
                    .push(Entry::new(repo_root, "(whole repo)", e.to_string()));
                continue;
            }
        }
        let porcelain = match ext.git_worktree_list(repo_root) {
            Ok(out) => out,
            Err(e) => {
                report
                    .failed
                    .push(Entry::new(repo_root, "(whole repo)", format!("{e:#}")));
                continue;
            }
        };
        // Keyed as discovery keys worktrees; the root checkout has no key
        // and is never a target.
        let entries: Vec<(PathBuf, PathBuf, WorktreeEntry)> = parse_porcelain(&porcelain)
            .into_iter()
            .filter_map(|e| {
                let (root, worktree) = worktree_key(&e.path)?;
                Some((root, worktree, e))
            })
            .collect();

        // Every worktree with a transcript, plus those under this repo's
        // .claude/worktrees without one, which get a fresh session.
        let mut targets: BTreeMap<&Path, Option<&Session>> = BTreeMap::new();
        for (root, worktree, _) in &entries {
            if root == repo_root {
                targets.insert(worktree, None);
            }
        }
        for session in sessions {
            targets.insert(&session.worktree, Some(session));
        }

        for (worktree, session) in targets {
            let entry = entries
                .iter()
                .find(|(_, w, _)| w == worktree)
                .map(|(_, _, e)| e);
            // Gone, skipped silently (as is a missing repo root, above). A
            // listed worktree whose directory is missing isn't launched,
            // because wtclaude would recreate it.
            let Some(entry) = entry.filter(|e| !e.prunable) else {
                continue;
            };
            let worktree_label = worktree.to_string_lossy().into_owned();
            match std::fs::metadata(worktree) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    let what = entry.branch.as_deref().unwrap_or(&worktree_label);
                    report
                        .failed
                        .push(Entry::new(repo_root, what, e.to_string()));
                    continue;
                }
            }
            let note = match (
                session.and_then(|s| s.transcript_branch.as_deref()),
                &entry.branch,
            ) {
                (Some(theirs), Some(branch)) if theirs != branch => {
                    Some(format!("transcript was on {theirs}"))
                }
                _ => None,
            };
            let note = note.as_deref();
            if live.covers(worktree, session) {
                let what = entry.branch.as_deref().unwrap_or(&worktree_label);
                report
                    .live
                    .push(Entry::new(repo_root, what, "").with_note(note));
                continue;
            }
            let Some(branch) = &entry.branch else {
                report
                    .left_alone
                    .push(Entry::new(repo_root, worktree_label, "detached"));
                continue;
            };
            if !dir_matches_branch(worktree, branch) {
                report.left_alone.push(
                    Entry::new(
                        repo_root,
                        worktree_label,
                        format!("directory name doesn't match branch {branch}"),
                    )
                    .with_note(note),
                );
                continue;
            }
            match ext.gh_merged_pr(repo_root, branch) {
                Ok(pr) if !pr.trim().is_empty() => {
                    report.merged.push(
                        Entry::new(repo_root, branch, format!("PR #{}", pr.trim())).with_note(note),
                    );
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    report
                        .merge_unchecked
                        .push(Entry::new(repo_root, branch, format!("{e:#}")))
                }
            }
            launch(ext, &mut report, repo_root, session, branch, startish, note);
        }
    }
    Ok(report)
}

/// Opens one window through `src window` (or, on a dry run, records the
/// command) and files the outcome in `report`, with `note` appended.
fn launch(
    ext: &dyn External,
    report: &mut Report,
    repo_root: &Path,
    session: Option<&Session>,
    branch: &str,
    startish: &Path,
    note: Option<&str>,
) {
    let session_id = session.map(|s| s.session_id.as_str());
    let args = src_window_args(repo_root, branch, session_id, startish);

    let mut details: Vec<String> = Vec::new();
    if let Some(session_id) = session_id {
        details.push(format!("session {session_id}"));
    }
    if report.dry_run {
        let command: Vec<String> = std::iter::once("src")
            .chain(args.iter().map(String::as_str))
            .map(shell_quote)
            .collect();
        details.push(format!("would run: {}", command.join(" ")));
    } else {
        let failure = match ext.src(&args) {
            Err(e) => Some(format!("{e:#}")),
            Ok(output) if !output.success => Some(failure_reason(
                &output.status,
                &output.stdout,
                &output.stderr,
            )),
            Ok(output) if output.stdout.trim().is_empty() => {
                let mut reason = "src window succeeded but printed no window id".to_string();
                if !output.stderr.trim().is_empty() {
                    reason.push_str(&format!(": {}", output.stderr.trim()));
                }
                Some(reason)
            }
            Ok(output) => {
                let (stale, other) = split_src_stderr(&output.stderr);
                for warning in stale {
                    report
                        .stale_windows
                        .push(Entry::new(repo_root, branch, warning));
                }
                details.push(format!("window {}", output.stdout.trim()));
                details.extend(other);
                None
            }
        };
        if let Some(failure) = failure {
            report
                .failed
                .push(Entry::new(repo_root, branch, failure).with_note(note));
            return;
        }
    }
    if let Some(session) = session.filter(|s| !s.older.is_empty()) {
        details.push(format!(
            "older sessions ignored: {}",
            session.older.join(", ")
        ));
    }

    let entry = Entry::new(repo_root, branch, details.join("; ")).with_note(note);
    if session.is_some() {
        report.resumed.push(entry);
    } else {
        report.fresh.push(entry);
    }
}

impl Report {
    /// 1 when anything failed, else 0.
    fn exit_code(&self) -> i32 {
        if self.failed.is_empty() { 0 } else { 1 }
    }

    fn render(&self, projects_dir: &Path) -> String {
        let mut out = String::new();
        if self.dry_run {
            out.push_str("Dry run: no windows were opened.\n");
            for note in &self.notes {
                out.push_str(&format!("Note: {note}.\n"));
            }
            out.push('\n');
        }
        if self.scanned == 0 {
            // List skipped entries even when nothing was scanned, so
            // unreadable directories don't look like there are no
            // transcripts.
            out.push_str(&format!(
                "No transcripts found under {}/*--claude-worktrees-*/, so no worktrees were checked.\n",
                projects_dir.display()
            ));
            self.render_skipped(&mut out);
            return out;
        }

        let (resumed, fresh) = if self.dry_run {
            ("Would resume", "Would start fresh")
        } else {
            ("Resumed", "Started fresh")
        };
        let sections: [(&str, &[Entry]); 8] = [
            (resumed, &self.resumed),
            (fresh, &self.fresh),
            ("Failed", &self.failed),
            ("Skipped, live", &self.live),
            ("Skipped, merged", &self.merged),
            (
                "Merge check failed, treated as unmerged",
                &self.merge_unchecked,
            ),
            ("Stale windows, left open", &self.stale_windows),
            (
                "Detached or mismatched worktrees, left alone",
                &self.left_alone,
            ),
        ];
        let mut anything = false;
        for (title, entries) in sections.iter().filter(|(_, e)| !e.is_empty()) {
            anything = true;
            out.push_str(&format!("{title} ({}):\n", entries.len()));
            for entry in entries.iter() {
                out.push_str(&format!("  {}  {}", entry.repo.display(), entry.what));
                if !entry.detail.is_empty() {
                    out.push_str(&format!("  ({})", entry.detail));
                }
                out.push('\n');
            }
            out.push('\n');
        }
        if !anything {
            out.push_str("Nothing to resume.\n\n");
        }

        out.push_str(&format!("Transcripts scanned: {}\n", self.scanned));
        self.render_skipped(&mut out);
        out
    }

    fn render_skipped(&self, out: &mut String) {
        if self.skipped.is_empty() {
            return;
        }
        out.push_str(&format!(
            "Transcripts and directories skipped ({}):\n",
            self.skipped.len()
        ));
        for (path, why) in &self.skipped {
            out.push_str(&format!("  {}: {why}\n", path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[test]
    fn split_worktree_cwd_strips_the_worktree_and_below() {
        assert_eq!(
            split_worktree_cwd("/home/me/repo/.claude/worktrees/feat+x/src/deep"),
            Some((PathBuf::from("/home/me/repo"), "feat+x".to_string()))
        );
        assert_eq!(
            split_worktree_cwd("/home/me/repo/.claude/worktrees/feat+x"),
            Some((PathBuf::from("/home/me/repo"), "feat+x".to_string()))
        );
    }

    #[test]
    fn split_worktree_cwd_splits_a_nested_worktree_at_the_last_marker() {
        assert_eq!(
            split_worktree_cwd("/r/.claude/worktrees/a/.claude/worktrees/b/src"),
            Some((PathBuf::from("/r/.claude/worktrees/a"), "b".to_string()))
        );
    }

    #[test]
    fn split_worktree_cwd_rejects_paths_outside_a_worktree() {
        assert_eq!(split_worktree_cwd("/home/me/repo"), None);
        assert_eq!(split_worktree_cwd("/home/me/repo/.claude/worktrees/"), None);
        assert_eq!(split_worktree_cwd("/.claude/worktrees/x"), None);
    }

    const CWD: &str = "/r/.claude/worktrees/a+b";

    #[test]
    fn newest_usable_line_prefers_the_last_line() {
        let data = format!(
            "{{\"cwd\":\"{CWD}\",\"gitBranch\":\"old\",\"sessionId\":\"s1\"}}\n\
             {{\"cwd\":\"{CWD}\",\"gitBranch\":\"a/b\",\"sessionId\":\"s2\"}}\n\
             {{\"type\":\"summary\"}}\n\
             not json\n"
        );
        let line = newest_usable_line(data.as_bytes(), "stem").unwrap();
        assert_eq!(line.branch.as_deref(), Some("a/b"));
        assert_eq!(line.session_id, "s2");
        assert_eq!(line.cwd, PathBuf::from(CWD));
    }

    #[test]
    fn newest_usable_line_falls_back_to_the_stem_for_the_session_id() {
        let data = format!("{{\"cwd\":\"{CWD}\"}}\n");
        let line = newest_usable_line(data.as_bytes(), "stem").unwrap();
        assert_eq!(line.session_id, "stem");
        assert_eq!(line.branch, None);
    }

    #[test]
    fn newest_usable_line_skips_lines_outside_a_worktree() {
        assert_eq!(
            newest_usable_line(b"{\"cwd\":\"/r\",\"sessionId\":\"s\"}\n", "x"),
            None
        );
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wtclaude-resume-test-{}-{}",
            std::process::id(),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_transcript_reads_the_whole_file_when_the_tail_has_no_usable_line() {
        let dir = temp_dir("tail");
        let path = dir.join("sid.jsonl");
        let usable = format!("{{\"cwd\":\"{CWD}\",\"sessionId\":\"s1\"}}\n");
        let huge = format!("{{\"padding\":\"{}\"}}\n", "x".repeat(200));
        std::fs::write(&path, format!("{usable}{huge}")).unwrap();

        // The tail is entirely inside the huge last line.
        let line = read_transcript(&path, 64).unwrap().unwrap();
        assert_eq!(line.session_id, "s1");
        // A tail that covers both lines finds it directly.
        assert!(read_transcript(&path, 4096).unwrap().is_some());

        std::fs::write(&path, &huge).unwrap();
        assert_eq!(read_transcript(&path, 64).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_porcelain_reads_branches_detached_and_prunable() {
        let out = "worktree /r\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /r/.claude/worktrees/a+b\nHEAD def\nbranch refs/heads/a/b\n\n\
                   worktree /r/.claude/worktrees/d\nHEAD 123\ndetached\n\n\
                   worktree /r/.claude/worktrees/p\nHEAD 456\nbranch refs/heads/p\nprunable gitdir file points to non-existent location\n";
        assert_eq!(
            parse_porcelain(out),
            vec![
                WorktreeEntry {
                    path: "/r".into(),
                    branch: Some("main".into()),
                    prunable: false
                },
                WorktreeEntry {
                    path: "/r/.claude/worktrees/a+b".into(),
                    branch: Some("a/b".into()),
                    prunable: false
                },
                WorktreeEntry {
                    path: "/r/.claude/worktrees/d".into(),
                    branch: None,
                    prunable: false
                },
                WorktreeEntry {
                    path: "/r/.claude/worktrees/p".into(),
                    branch: Some("p".into()),
                    prunable: true
                },
            ]
        );
    }

    #[test]
    fn dir_matches_branch_turns_slashes_into_plus() {
        assert!(dir_matches_branch(
            Path::new("/r/.claude/worktrees/a+b+c"),
            "a/b/c"
        ));
        assert!(!dir_matches_branch(
            Path::new("/r/.claude/worktrees/a-b"),
            "a/b"
        ));
        assert!(!dir_matches_branch(
            Path::new("/r/.claude/worktrees/old"),
            "new"
        ));
    }

    #[test]
    fn src_window_args_resume_and_fresh() {
        let startish = Path::new("/home/me/.cargo/bin/startish");
        assert_eq!(
            src_window_args(Path::new("/r"), "a/b", Some("sid"), startish),
            [
                "window",
                "/r",
                "-n",
                "a/b",
                "--",
                "/home/me/.cargo/bin/startish",
                "--history",
                "wtclaude --no-pull a/b",
                "--",
                "wtclaude",
                "--no-pull",
                "--resume",
                "sid",
                "a/b"
            ]
        );
        let fresh = src_window_args(Path::new("/r"), "x", None, startish);
        assert_eq!(fresh[fresh.len() - 3..], ["wtclaude", "--no-pull", "x"]);
    }

    #[test]
    fn split_src_stderr_separates_stale_window_warnings() {
        let stderr = "Note: something else\n\n\
                      Warning: session r already has a window named a/b (@3)\n\
                      Warning: unrelated\n";
        let (stale, other) = split_src_stderr(stderr);
        assert_eq!(stale, ["session r already has a window named a/b (@3)"]);
        assert_eq!(other, ["Note: something else", "Warning: unrelated"]);
    }

    #[test]
    fn failure_reason_falls_back_to_stdout_then_the_status() {
        assert_eq!(
            failure_reason("exit status: 1", "out", "err\n"),
            "err (exit status: 1)"
        );
        assert_eq!(
            failure_reason("exit status: 1", "out\n", " \n"),
            "out (exit status: 1)"
        );
        assert_eq!(
            failure_reason("signal: 9 (SIGKILL)", "", ""),
            "signal: 9 (SIGKILL)"
        );
    }

    #[test]
    fn parse_live_rejects_an_entry_without_a_cwd() {
        assert_eq!(
            parse_live(r#"[{"cwd":"/nonexistent/a","sessionId":"s1"},{"cwd":"/nonexistent/b"}]"#)
                .unwrap(),
            Live {
                worktrees: HashSet::from(["/nonexistent/a".into(), "/nonexistent/b".into()]),
                session_ids: HashSet::from(["s1".into()]),
            }
        );
        assert!(parse_live(r#"[{"cwd":"/a"},{"directory":"/b"}]"#).is_err());
        assert!(parse_live(r#"[{"cwd":null}]"#).is_err());
        assert!(parse_live(r#"[{"sessionId":"s1"}]"#).is_err());
    }

    #[test]
    fn resolve_tool_falls_back_only_on_a_dry_run() {
        let err = resolve_tool(
            "src",
            "; resume-all opens windows through `src window`",
            "",
            false,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "src not found on PATH; resume-all opens windows through `src window`"
        );
        let (startish, startish_note) = resolve_tool("startish", "", "", true).unwrap();
        assert_eq!(startish, PathBuf::from("startish"));
        let (_, src_note) = resolve_tool("src", "", "", true).unwrap();

        let report = Report {
            dry_run: true,
            notes: [startish_note, src_note].into_iter().flatten().collect(),
            scanned: 1,
            ..Report::default()
        };
        assert!(report.render(Path::new("/p")).starts_with(
            "Dry run: no windows were opened.\n\
             Note: startish not found on PATH; a real run would stop here.\n\
             Note: src not found on PATH; a real run would stop here.\n"
        ));
    }

    /// Canned answers for every external command. A missing entry for a
    /// repo or branch is an error, as the real command's failure would be.
    #[derive(Default)]
    struct Fake {
        agents: Option<String>,
        worktrees: HashMap<PathBuf, String>,
        merged: HashMap<String, String>,
        src: HashMap<String, (bool, String, String)>,
        src_calls: RefCell<Vec<Vec<String>>>,
        gh_calls: RefCell<Vec<String>>,
    }

    impl External for Fake {
        fn claude_agents(&self) -> Result<String> {
            self.agents.clone().context("claude agents failed")
        }
        fn git_worktree_list(&self, repo: &Path) -> Result<String> {
            self.worktrees.get(repo).cloned().context("not a git repo")
        }
        fn gh_merged_pr(&self, _repo: &Path, branch: &str) -> Result<String> {
            self.gh_calls.borrow_mut().push(branch.to_string());
            self.merged.get(branch).cloned().context("gh: no auth")
        }
        fn src(&self, args: &[String]) -> Result<SrcOutput> {
            self.src_calls.borrow_mut().push(args.to_vec());
            let name = args
                .iter()
                .skip_while(|a| *a != "-n")
                .nth(1)
                .context("no -n")?;
            let (success, stdout, stderr) = self.src.get(name).cloned().context("src not found")?;
            Ok(SrcOutput {
                success,
                status: format!("exit status: {}", if success { 0 } else { 1 }),
                stdout,
                stderr,
            })
        }
    }

    /// A repo with the given directories under .claude/worktrees, plus an
    /// empty projects dir; add transcripts with `transcript()`.
    struct Setup {
        base: PathBuf,
        repo: PathBuf,
        projects: PathBuf,
    }

    impl Setup {
        fn new(label: &str, dirs: &[&str]) -> Self {
            // Canonical, as discovery's paths are.
            let base = canonical(&temp_dir(label));
            let repo = base.join("repo");
            for dir in dirs {
                std::fs::create_dir_all(repo.join(".claude/worktrees").join(dir)).unwrap();
            }
            let projects = base.join("projects");
            std::fs::create_dir_all(&projects).unwrap();
            Setup {
                base,
                repo,
                projects,
            }
        }

        fn worktree(&self, dir: &str) -> PathBuf {
            self.repo.join(".claude/worktrees").join(dir)
        }

        fn transcript(&self, dir: &str, session_id: &str, branch: &str) {
            self.transcript_via(&self.repo, dir, session_id, branch);
        }

        /// A transcript whose cwd spells the repo as `repo`.
        fn transcript_via(&self, repo: &Path, dir: &str, session_id: &str, branch: &str) {
            let project = self.projects.join(format!("x--claude-worktrees-{dir}"));
            std::fs::create_dir_all(&project).unwrap();
            let line = serde_json::json!({
                "cwd": repo.join(".claude/worktrees").join(dir).join("src"),
                "gitBranch": branch,
                "sessionId": session_id,
            });
            std::fs::write(
                project.join(format!("{session_id}.jsonl")),
                format!("{line}\n"),
            )
            .unwrap();
        }

        /// Backdates a transcript by an hour, making it older than the rest.
        fn age(&self, dir: &str, session_id: &str) {
            let path = self
                .projects
                .join(format!("x--claude-worktrees-{dir}"))
                .join(format!("{session_id}.jsonl"));
            let earlier = SystemTime::now() - std::time::Duration::from_secs(3600);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(earlier)
                .unwrap();
        }

        fn porcelain(&self, worktrees: &[(&str, Option<&str>)]) -> String {
            let mut out = format!(
                "worktree {}\nHEAD 0\nbranch refs/heads/main\n\n",
                self.repo.display()
            );
            for (dir, branch) in worktrees {
                out.push_str(&format!(
                    "worktree {}\nHEAD 0\n",
                    self.worktree(dir).display()
                ));
                match branch {
                    Some(b) => out.push_str(&format!("branch refs/heads/{b}\n\n")),
                    None => out.push_str("detached\n\n"),
                }
            }
            out
        }
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    const STARTISH: &str = "/bin/startish";

    #[test]
    fn dry_run_reports_every_outcome_without_calling_src() {
        let s = Setup::new(
            "dry-run",
            &[
                "e+resume",
                "e+fresh",
                "e+live",
                "e+merged",
                "e+nogh",
                "detached",
                "misnamed",
                "live-detached",
                "e+byid",
                "e+prunable",
            ],
        );
        s.transcript("e+resume", "sid-resume-old", "e/resume");
        s.age("e+resume", "sid-resume-old");
        s.transcript("e+resume", "sid-resume", "HEAD");
        // A subagent transcript in a subdirectory isn't read.
        let subagents = s
            .projects
            .join("x--claude-worktrees-e+resume/sid-resume/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::copy(
            s.projects
                .join("x--claude-worktrees-e+resume/sid-resume.jsonl"),
            subagents.join("x.jsonl"),
        )
        .unwrap();
        s.transcript("e+byid", "sid-byid", "e/byid");
        s.transcript("e+prunable", "sid-prunable", "e/prunable");
        // Listed by git, but its directory is missing.
        s.transcript("e+nodir", "sid-nodir", "e/nodir");
        let unusable = s.projects.join("x--claude-worktrees-junk/junk.jsonl");
        std::fs::create_dir_all(unusable.parent().unwrap()).unwrap();
        std::fs::write(&unusable, "{\"cwd\":\"/r\"}\n").unwrap();
        s.transcript("e+live", "sid-live", "old/live");
        s.transcript("e+merged", "sid-merged", "old/merged");
        s.transcript("e+nogh", "sid-nogh", "e/nogh");
        s.transcript("detached", "sid-detached", "HEAD");
        s.transcript("e+gone", "sid-gone", "e/gone");

        let mut fake = Fake {
            agents: Some(
                // A Claude below the worktree counts as live.
                serde_json::json!([
                    { "cwd": s.worktree("e+live").join("src") },
                    { "cwd": s.worktree("live-detached") },
                    { "cwd": "/elsewhere" },
                    // Started at the repo root, so only its session id
                    // ties it to its worktree.
                    { "cwd": s.repo, "sessionId": "sid-byid" },
                ])
                .to_string(),
            ),
            ..Fake::default()
        };
        let mut porcelain = s.porcelain(&[
            ("e+resume", Some("e/resume")),
            ("e+fresh", Some("e/fresh")),
            ("e+live", Some("e/live")),
            ("e+merged", Some("e/merged")),
            ("e+nogh", Some("e/nogh")),
            ("detached", None),
            ("misnamed", Some("other")),
            ("live-detached", None),
            ("e+byid", Some("e/byid")),
            ("e+nodir", Some("e/nodir")),
            ("e+prunable", Some("e/prunable")),
        ]);
        porcelain.push_str("prunable gitdir file points to non-existent location\n");
        fake.worktrees.insert(s.repo.clone(), porcelain);
        for branch in ["e/resume", "e/fresh", "e/live"] {
            fake.merged.insert(branch.into(), String::new());
        }
        fake.merged.insert("e/merged".into(), "42\n".into());

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert_eq!(discovery.scanned, 11);
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();

        assert!(fake.src_calls.borrow().is_empty());
        // Gone and live are filtered before the merge check.
        for branch in ["e/live", "e/byid", "e/nodir", "e/prunable"] {
            assert!(!fake.gh_calls.borrow().contains(&branch.to_string()));
        }

        let repo = s.repo.to_string_lossy();
        let rendered = report.render(&s.projects);
        let expected = format!(
            "Dry run: no windows were opened.\n\n\
             Would resume (2):\n\
             \x20 {repo}  e/nogh  (session sid-nogh; would run: src window {repo} -n e/nogh -- /bin/startish --history 'wtclaude --no-pull e/nogh' -- wtclaude --no-pull --resume sid-nogh e/nogh)\n\
             \x20 {repo}  e/resume  (session sid-resume; would run: src window {repo} -n e/resume -- /bin/startish --history 'wtclaude --no-pull e/resume' -- wtclaude --no-pull --resume sid-resume e/resume; older sessions ignored: sid-resume-old; transcript was on HEAD)\n\n\
             Would start fresh (1):\n\
             \x20 {repo}  e/fresh  (would run: src window {repo} -n e/fresh -- /bin/startish --history 'wtclaude --no-pull e/fresh' -- wtclaude --no-pull e/fresh)\n\n\
             Skipped, live (3):\n\
             \x20 {repo}  e/byid\n\
             \x20 {repo}  e/live  (transcript was on old/live)\n\
             \x20 {repo}  {live_detached}\n\n\
             Skipped, merged (1):\n\
             \x20 {repo}  e/merged  (PR #42; transcript was on old/merged)\n\n\
             Merge check failed, treated as unmerged (1):\n\
             \x20 {repo}  e/nogh  (gh: no auth)\n\n\
             Detached or mismatched worktrees, left alone (2):\n\
             \x20 {repo}  {detached}  (detached)\n\
             \x20 {repo}  {misnamed}  (directory name doesn't match branch other)\n\n\
             Transcripts scanned: 11\n\
             Transcripts and directories skipped (1):\n\
             \x20 {unusable}: no line with a worktree cwd\n",
            unusable = unusable.display(),
            detached = s.worktree("detached").display(),
            misnamed = s.worktree("misnamed").display(),
            live_detached = s.worktree("live-detached").display(),
        );
        assert_eq!(rendered, expected);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn launch_files_failures_stale_windows_and_window_ids() {
        let s = Setup::new("launch", &["ok", "stale", "bad", "noid"]);
        s.transcript("ok", "sid-ok", "ok");
        let mut fake = Fake {
            agents: Some("[]".into()),
            ..Fake::default()
        };
        fake.worktrees.insert(
            s.repo.clone(),
            s.porcelain(&[
                ("ok", Some("ok")),
                ("stale", Some("stale")),
                ("bad", Some("bad")),
                ("noid", Some("noid")),
            ]),
        );
        for branch in ["ok", "stale", "bad", "noid"] {
            fake.merged.insert(branch.into(), String::new());
        }
        fake.src.insert(
            "ok".into(),
            (true, "@1\n".into(), "Note: created session repo\n".into()),
        );
        fake.src.insert(
            "stale".into(),
            (
                true,
                "@2\n".into(),
                "Warning: session repo already has a window named stale (@0)\n".into(),
            ),
        );
        fake.src.insert(
            "bad".into(),
            (false, String::new(), "no such session\n".into()),
        );
        fake.src
            .insert("noid".into(), (true, "\n".into(), "odd\n".into()));

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), false).unwrap();

        let entry = |what: &str, detail: &str| Entry::new(&s.repo, what, detail);
        // Other stderr lines go into the detail.
        assert_eq!(
            report.resumed,
            [entry(
                "ok",
                "session sid-ok; window @1; Note: created session repo"
            )]
        );
        assert_eq!(report.fresh, [entry("stale", "window @2")]);
        assert_eq!(
            report.failed,
            [
                entry("bad", "no such session (exit status: 1)"),
                entry("noid", "src window succeeded but printed no window id: odd"),
            ]
        );
        assert_eq!(
            report.stale_windows,
            [entry(
                "stale",
                "session repo already has a window named stale (@0)"
            )]
        );
        assert_eq!(fake.src_calls.borrow().len(), 4);
        assert_eq!(report.exit_code(), 1);

        let rendered = report.render(&s.projects);
        let headings: Vec<&str> = rendered
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with(' '))
            .collect();
        assert_eq!(
            headings,
            [
                "Resumed (1):",
                "Started fresh (1):",
                "Failed (2):",
                "Stale windows, left open (1):",
                "Transcripts scanned: 1",
            ]
        );
    }

    #[test]
    fn a_failed_repo_or_src_call_does_not_stop_the_sweep() {
        let a = Setup::new("two-a", &["a1", "a2"]);
        let b = Setup::new("two-b", &["b1"]);
        a.transcript("a1", "sid-a1", "a1");
        a.transcript_via(&b.repo, "b1", "sid-b1", "b1");
        // A repo that no longer exists is skipped silently.
        let missing = a.base.join("missing");
        a.transcript_via(&missing, "m1", "sid-m1", "m1");
        let mut fake = Fake {
            agents: Some("[]".into()),
            ..Fake::default()
        };
        // b's worktree list fails; a2's src call can't run.
        fake.worktrees.insert(
            a.repo.clone(),
            a.porcelain(&[("a1", Some("a1")), ("a2", Some("a2"))]),
        );
        for branch in ["a1", "a2"] {
            fake.merged.insert(branch.into(), String::new());
        }
        fake.src
            .insert("a1".into(), (true, "@1\n".into(), String::new()));

        let discovery = discover(&a.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), false).unwrap();

        assert_eq!(
            report.resumed,
            [Entry::new(&a.repo, "a1", "session sid-a1; window @1")]
        );
        assert_eq!(
            report.failed,
            [
                Entry::new(&a.repo, "a2", "src not found"),
                Entry::new(&b.repo, "(whole repo)", "not a git repo"),
            ]
        );
        assert_eq!(report.exit_code(), 1);
        let rendered = report.render(&a.projects);
        assert!(!rendered.contains(&*missing.to_string_lossy()));
        assert!(rendered.contains("Transcripts scanned: 3\n"));
    }

    #[test]
    fn a_live_nested_worktree_leaves_its_parent_dead() {
        let s = Setup::new("nested", &["a", "a/.claude/worktrees/b"]);
        s.transcript("a", "sid-a", "a");
        let mut fake = Fake {
            agents: Some(
                serde_json::json!([{ "cwd": s.worktree("a").join(".claude/worktrees/b") }])
                    .to_string(),
            ),
            ..Fake::default()
        };
        fake.worktrees
            .insert(s.repo.clone(), s.porcelain(&[("a", Some("a"))]));
        fake.merged.insert("a".into(), String::new());

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();
        assert!(report.live.is_empty());
        assert_eq!(report.resumed.len(), 1);
        assert_eq!(report.resumed[0].what, "a");
    }

    #[test]
    fn two_spellings_of_one_repo_group_together() {
        let s = Setup::new("symlink", &["w"]);
        let link = s.base.join("link");
        std::os::unix::fs::symlink(&s.repo, &link).unwrap();
        s.transcript("w", "old", "w");
        s.age("w", "old");
        s.transcript_via(&link, "w", "new", "w");

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert_eq!(discovery.sessions.len(), 1);
        assert_eq!(discovery.sessions[0].repo_root, s.repo);
        assert_eq!(discovery.sessions[0].session_id, "new");
        assert_eq!(discovery.sessions[0].older, ["old"]);

        let mut fake = Fake {
            agents: Some("[]".into()),
            ..Fake::default()
        };
        fake.worktrees
            .insert(s.repo.clone(), s.porcelain(&[("w", Some("w"))]));
        fake.merged.insert("w".into(), String::new());
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();
        assert_eq!(report.resumed.len(), 1);
    }

    #[test]
    fn keep_repo_drops_other_repos_even_when_their_slugs_match() {
        // `a.b` and `a-b` share a project dir slug; only the root tells
        // them apart.
        let s = Setup::new("keep-repo", &[]);
        let here = s.base.join("a.b");
        let other = s.base.join("a-b");
        for repo in [&here, &other] {
            std::fs::create_dir_all(repo.join(".claude/worktrees/w")).unwrap();
        }
        let link = s.base.join("link");
        std::os::unix::fs::symlink(&here, &link).unwrap();
        s.transcript_via(&link, "w", "here", "w");
        s.transcript_via(&other, "w", "other", "w");

        let mut discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert_eq!(discovery.sessions.len(), 2);
        keep_repo(&mut discovery, &here);
        assert_eq!(discovery.sessions.len(), 1);
        assert_eq!(discovery.sessions[0].repo_root, here);
        assert_eq!(discovery.sessions[0].session_id, "here");

        // The fake knows only `a.b`, so a sweep that reached `a-b` would
        // report a failure.
        let mut fake = Fake {
            agents: Some("[]".into()),
            ..Fake::default()
        };
        let porcelain = format!(
            "worktree {}\nHEAD 0\nbranch refs/heads/main\n\n\
             worktree {}\nHEAD 0\nbranch refs/heads/w\n\n",
            here.display(),
            here.join(".claude/worktrees/w").display()
        );
        fake.worktrees.insert(here.clone(), porcelain);
        fake.merged.insert("w".into(), String::new());
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();
        assert_eq!(report.resumed.len(), 1);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn current_repo_root_is_the_main_repo_from_the_root_and_a_worktree() {
        let s = Setup::new("current-repo", &[]);
        std::fs::create_dir_all(&s.repo).unwrap();
        git(&s.repo, &["init", "-q"]);
        git(
            &s.repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(
            &s.repo,
            &["worktree", "add", "-q", "-b", "w", ".claude/worktrees/w"],
        );
        std::fs::create_dir_all(s.worktree("w").join("src")).unwrap();

        assert_eq!(current_repo_root(&s.repo).unwrap(), s.repo);
        assert_eq!(current_repo_root(&s.worktree("w")).unwrap(), s.repo);
        assert_eq!(
            current_repo_root(&s.worktree("w").join("src")).unwrap(),
            s.repo
        );
    }

    #[test]
    fn a_live_cwd_through_a_symlink_counts_as_live() {
        let s = Setup::new("symlink-live", &["w"]);
        let link = s.base.join("link");
        std::os::unix::fs::symlink(&s.repo, &link).unwrap();
        s.transcript("w", "sid", "w");
        // canonicalize needs the cwd to exist.
        std::fs::create_dir_all(s.worktree("w").join("src")).unwrap();
        let mut fake = Fake {
            agents: Some(
                serde_json::json!([{ "cwd": link.join(".claude/worktrees/w/src") }]).to_string(),
            ),
            ..Fake::default()
        };
        fake.worktrees
            .insert(s.repo.clone(), s.porcelain(&[("w", Some("w"))]));
        fake.merged.insert("w".into(), String::new());

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), false).unwrap();
        assert_eq!(report.live, [Entry::new(&s.repo, "w", "")]);
        assert!(report.resumed.is_empty());
        assert!(fake.src_calls.borrow().is_empty());
    }

    #[test]
    fn all_gone_says_nothing_to_resume() {
        let s = Setup::new("all-gone", &[]);
        s.transcript("w", "sid", "w");
        let mut fake = Fake {
            agents: Some("[]".into()),
            ..Fake::default()
        };
        fake.worktrees.insert(s.repo.clone(), s.porcelain(&[]));
        std::fs::create_dir_all(&s.repo).unwrap();

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), false).unwrap();
        assert_eq!(
            report.render(&s.projects),
            "Nothing to resume.\n\nTranscripts scanned: 1\n"
        );
    }

    #[test]
    fn keeps_the_newest_session_per_worktree() {
        let s = Setup::new("newest", &["w"]);
        s.transcript("w", "old", "w");
        s.age("w", "old");
        s.transcript("w", "new", "w");

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert_eq!(discovery.sessions.len(), 1);
        assert_eq!(discovery.sessions[0].session_id, "new");
        assert_eq!(discovery.sessions[0].older, ["old"]);
    }

    #[test]
    fn a_failed_live_check_aborts() {
        let s = Setup::new("no-agents", &["w"]);
        s.transcript("w", "sid", "w");
        let fake = Fake::default();
        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert!(sweep(&fake, discovery, Path::new(STARTISH), true).is_err());
        assert!(fake.gh_calls.borrow().is_empty());
    }

    #[test]
    fn no_transcripts_says_so() {
        let s = Setup::new("empty", &[]);
        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let mut report = sweep(&Fake::default(), discovery, Path::new(STARTISH), true).unwrap();
        // A dry run's notes still show, since a real run would stop first.
        report.notes = vec!["src not found on PATH; a real run would stop here".into()];
        assert!(report.render(&s.projects).starts_with(
            "Dry run: no windows were opened.\n\
             Note: src not found on PATH; a real run would stop here.\n\n\
             No transcripts found under"
        ));
    }

    #[test]
    fn discover_records_unreadable_project_dirs_and_transcripts() {
        let s = Setup::new("unreadable", &[]);
        let dangling = s.projects.join("x--claude-worktrees-dangling");
        std::os::unix::fs::symlink(s.base.join("nowhere"), &dangling).unwrap();
        let project = s.projects.join("x--claude-worktrees-w");
        std::fs::create_dir_all(&project).unwrap();
        // A symlink to itself can't be stat'ed, but isn't missing either.
        let looping = project.join("loop.jsonl");
        std::os::unix::fs::symlink(&looping, &looping).unwrap();
        // A transcript that's gone is skipped silently.
        std::os::unix::fs::symlink(s.base.join("nowhere"), project.join("gone.jsonl")).unwrap();

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert_eq!(discovery.scanned, 0);
        let skipped: Vec<&Path> = discovery.skipped.iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(skipped.len(), 2);
        assert!(skipped.contains(&dangling.as_path()));
        assert!(skipped.contains(&looping.as_path()));
    }

    #[test]
    fn an_unstatable_repo_root_or_worktree_fails() {
        let s = Setup::new("unstatable", &[]);
        // Symlinks to themselves: stat fails with something other than
        // "not found", as on an unmounted network mount.
        let repo_loop = s.base.join("repo-loop");
        std::os::unix::fs::symlink(&repo_loop, &repo_loop).unwrap();
        s.transcript_via(&repo_loop, "r", "sid-r", "r");
        std::fs::create_dir_all(s.repo.join(".claude/worktrees")).unwrap();
        let worktree_loop = s.worktree("w");
        std::os::unix::fs::symlink(&worktree_loop, &worktree_loop).unwrap();
        s.transcript("w", "sid-w", "w");
        let mut fake = Fake {
            agents: Some("[]".into()),
            ..Fake::default()
        };
        fake.worktrees
            .insert(s.repo.clone(), s.porcelain(&[("w", Some("w"))]));

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();
        let failed: Vec<(&Path, &str)> = report
            .failed
            .iter()
            .map(|e| (e.repo.as_path(), e.what.as_str()))
            .collect();
        assert_eq!(
            failed,
            [
                (s.repo.as_path(), "w"),
                (repo_loop.as_path(), "(whole repo)")
            ]
        );
        assert!(fake.gh_calls.borrow().is_empty());
    }

    #[test]
    fn a_symlinked_worktree_directory_is_found() {
        let s = Setup::new("symlinked-worktree", &[]);
        let target = s.base.join("elsewhere/w");
        std::fs::create_dir_all(target.join("src")).unwrap();
        std::fs::create_dir_all(s.repo.join(".claude/worktrees")).unwrap();
        std::os::unix::fs::symlink(&target, s.worktree("w")).unwrap();
        std::os::unix::fs::symlink(&target, s.worktree("live")).unwrap();
        s.transcript("w", "sid-w", "w");
        let mut fake = Fake {
            agents: Some(
                serde_json::json!([{ "cwd": s.worktree("live").join("src") }]).to_string(),
            ),
            ..Fake::default()
        };
        fake.worktrees.insert(
            s.repo.clone(),
            s.porcelain(&[("w", Some("w")), ("live", Some("live"))]),
        );
        fake.merged.insert("w".into(), String::new());

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();
        assert_eq!(report.resumed.len(), 1);
        assert_eq!(report.resumed[0].what, "w");
        assert_eq!(report.live, [Entry::new(&s.repo, "live", "")]);
    }

    #[test]
    fn no_transcripts_still_lists_unreadable_directories() {
        let report = Report {
            skipped: vec![(
                "/p/x--claude-worktrees-w".into(),
                "Permission denied".into(),
            )],
            ..Report::default()
        };
        assert_eq!(
            report.render(Path::new("/p")),
            "No transcripts found under /p/*--claude-worktrees-*/, so no worktrees were checked.\n\
             Transcripts and directories skipped (1):\n\
             \x20 /p/x--claude-worktrees-w: Permission denied\n"
        );
    }
}
