//! `wtclaude resume-all`: after a reboot or power loss, reopen every
//! worktree whose Claude session isn't running, each in a tmux window
//! (opened by `src window`) that resumes its newest session.

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

#[derive(Args)]
pub struct ResumeAllArgs {
    /// Print the report without opening any windows
    #[arg(long)]
    pub dry_run: bool,
}

/// How much of each transcript is read before falling back to all of it.
const TAIL_BYTES: u64 = 64 * 1024;

const WORKTREES_MARKER: &str = "/.claude/worktrees/";

pub fn run(args: ResumeAllArgs) -> Result<i32> {
    // The window gets the tmux server's environment, whose PATH may not
    // include ~/.cargo/bin, so startish goes in by absolute path.
    let path_var = std::env::var("PATH").unwrap_or_default();
    let startish = resolve_on_path("startish", &path_var)
        .context("startish not found on PATH")
        .and_then(|p| std::path::absolute(&p).context("resolving startish"))?;

    let projects_dir = projects_dir()?;
    let discovery = discover(&projects_dir, TAIL_BYTES)?;
    let report = sweep(&System, discovery, &startish, args.dry_run)?;
    print!("{}", report.render(&projects_dir));
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// The commands `resume-all` runs, behind a trait so tests can fake them.
/// Each returns the command's stdout, or an error carrying its stderr.
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
    pub stdout: String,
    pub stderr: String,
}

struct System;

/// Runs `cmd`, returning its stdout, or its stderr as the error.
fn output_of(mut cmd: Command, what: &str) -> Result<String> {
    let output = cmd.output().with_context(|| format!("running {what}"))?;
    if !output.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
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
        let output = Command::new("src")
            .args(args)
            .output()
            .context("running src")?;
        Ok(SrcOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Where a transcript's newest usable line says its session was.
#[derive(Debug, PartialEq)]
struct LastLine {
    repo_root: PathBuf,
    worktree: PathBuf,
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
/// and the worktree path, dropping anything below the worktree.
fn split_worktree_cwd(cwd: &str) -> Option<(PathBuf, PathBuf)> {
    let at = cwd.rfind(WORKTREES_MARKER)?;
    let repo_root = &cwd[..at];
    let dir = cwd[at + WORKTREES_MARKER.len()..].split('/').next()?;
    if repo_root.is_empty() || dir.is_empty() {
        return None;
    }
    let repo_root = PathBuf::from(repo_root);
    let worktree = repo_root.join(".claude").join("worktrees").join(dir);
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
            let (repo_root, worktree) = split_worktree_cwd(text("cwd")?)?;
            Some(LastLine {
                repo_root,
                worktree,
                branch: text("gitBranch").map(String::from),
                session_id: text("sessionId").unwrap_or(stem).to_string(),
            })
        })
}

/// Reads the last `tail` bytes of the transcript at `path`, and the whole
/// file if they hold no usable line (one huge final line).
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
/// files, which leaves out subagent transcripts in subdirectories.
fn discover(projects_dir: &Path, tail: u64) -> Result<Discovery> {
    let mut discovery = Discovery::default();
    if !projects_dir.is_dir() {
        return Ok(discovery);
    }
    let mut found: Vec<(SystemTime, LastLine)> = Vec::new();
    let entries = std::fs::read_dir(projects_dir)
        .with_context(|| format!("reading {}", projects_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", projects_dir.display()))?;
        if !entry
            .file_name()
            .to_string_lossy()
            .contains("--claude-worktrees-")
            || !entry.path().is_dir()
        {
            continue;
        }
        let transcripts = std::fs::read_dir(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;
        for transcript in transcripts {
            let path = transcript?.path();
            if path.extension().is_none_or(|e| e != "jsonl") || !path.is_file() {
                continue;
            }
            discovery.scanned += 1;
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
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
        match by_worktree.get_mut(&line.worktree) {
            Some(newer) => newer.older.push(line.session_id),
            None => {
                by_worktree.insert(
                    line.worktree.clone(),
                    Session {
                        repo_root: line.repo_root,
                        worktree: line.worktree,
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

/// The `cwd` of every running session in `claude agents --json`.
fn parse_live_cwds(json: &str) -> Result<HashSet<PathBuf>> {
    let agents: Vec<serde_json::Value> =
        serde_json::from_str(json).context("parsing claude agents --json")?;
    Ok(agents
        .iter()
        .filter_map(|a| a.get("cwd")?.as_str())
        .map(|cwd| canonical(Path::new(cwd)))
        .collect())
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
/// would nest `.claude/worktrees/`. `wtclaude` stays a bare word, so the
/// window's shell finds it, and its history wrapper, after the rc file ran.
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

/// The `Warning: …` lines `src window` prints for each existing window that
/// already has the new window's name, without the `Warning: ` prefix.
fn stale_window_warnings(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("Warning: "))
        .filter(|l| l.starts_with("session ") && l.contains(" already has a window named "))
        .map(String::from)
        .collect()
}

/// Resolves symlinks where the path exists, so paths from transcripts, git
/// and `claude agents` compare equal.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One line of the report: a worktree (by branch, or by path when it has
/// no usable branch) in a repo, plus whatever is worth saying about it.
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
}

#[derive(Debug, Default)]
struct Report {
    dry_run: bool,
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
    skipped_transcripts: Vec<(PathBuf, String)>,
}

/// A worktree that survived discovery and is a candidate for a window.
struct Target<'a> {
    repo_root: &'a Path,
    worktree: PathBuf,
    session: Option<&'a Session>,
}

/// Filters the discovered worktrees and launches (or, on a dry run, only
/// lists) a window for each survivor.
fn sweep(
    ext: &dyn External,
    discovery: Discovery,
    startish: &Path,
    dry_run: bool,
) -> Result<Report> {
    let mut report = Report {
        dry_run,
        scanned: discovery.scanned,
        skipped_transcripts: discovery.skipped,
        ..Report::default()
    };
    if discovery.sessions.is_empty() {
        return Ok(report);
    }

    // Without a live list every running session would look dead and get a
    // second Claude, so a failure here stops everything.
    let live = ext
        .claude_agents()
        .and_then(|json| parse_live_cwds(&json))
        .context("cannot tell which worktrees are live")?;

    let mut repos: BTreeMap<&Path, Vec<&Session>> = BTreeMap::new();
    for session in &discovery.sessions {
        repos.entry(&session.repo_root).or_default().push(session);
    }

    for (repo_root, sessions) in repos {
        if !repo_root.is_dir() {
            continue;
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
        let entries = parse_porcelain(&porcelain);
        let worktrees_dir = canonical(repo_root).join(".claude").join("worktrees");

        // Every worktree with a transcript, plus those under .claude/worktrees
        // without one, which get a fresh session.
        let mut targets: BTreeMap<PathBuf, Target> = BTreeMap::new();
        for entry in &entries {
            let path = canonical(&entry.path);
            if path.parent() == Some(worktrees_dir.as_path()) {
                targets.insert(
                    path.clone(),
                    Target {
                        repo_root,
                        worktree: path,
                        session: None,
                    },
                );
            }
        }
        for session in sessions {
            let path = canonical(&session.worktree);
            targets.insert(
                path.clone(),
                Target {
                    repo_root,
                    worktree: path,
                    session: Some(session),
                },
            );
        }

        for target in targets.into_values() {
            let entry = entries
                .iter()
                .find(|e| canonical(&e.path) == target.worktree);
            // Gone: the only silent skip. A missing directory would be
            // recreated by wtclaude.
            let Some(entry) = entry.filter(|e| !e.prunable && target.worktree.is_dir()) else {
                continue;
            };
            let worktree_label = target.worktree.to_string_lossy().into_owned();
            let Some(branch) = &entry.branch else {
                report
                    .left_alone
                    .push(Entry::new(repo_root, worktree_label, "detached"));
                continue;
            };
            if !dir_matches_branch(&target.worktree, branch) {
                report.left_alone.push(Entry::new(
                    repo_root,
                    worktree_label,
                    format!("directory name doesn't match branch {branch}"),
                ));
                continue;
            }
            if live.contains(&target.worktree) {
                report.live.push(Entry::new(repo_root, branch, ""));
                continue;
            }
            match ext.gh_merged_pr(repo_root, branch) {
                Ok(pr) if !pr.trim().is_empty() => {
                    report
                        .merged
                        .push(Entry::new(repo_root, branch, format!("PR #{}", pr.trim())));
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    report
                        .merge_unchecked
                        .push(Entry::new(repo_root, branch, format!("{e:#}")))
                }
            }
            launch(ext, &mut report, &target, branch, startish);
        }
    }
    Ok(report)
}

/// Opens one window through `src window` (or, on a dry run, records the
/// command) and files the outcome in `report`.
fn launch(ext: &dyn External, report: &mut Report, target: &Target, branch: &str, startish: &Path) {
    let repo_root = target.repo_root;
    let session_id = target.session.map(|s| s.session_id.as_str());
    let args = src_window_args(repo_root, branch, session_id, startish);

    let mut details: Vec<String> = Vec::new();
    if let Some(session) = target.session {
        details.push(format!("session {}", session.session_id));
    }
    if report.dry_run {
        let command: Vec<String> = std::iter::once("src")
            .chain(args.iter().map(String::as_str))
            .map(shell_quote)
            .collect();
        details.push(format!("would run: {}", command.join(" ")));
    } else {
        let output = match ext.src(&args) {
            Ok(output) => output,
            Err(e) => {
                report
                    .failed
                    .push(Entry::new(repo_root, branch, format!("{e:#}")));
                return;
            }
        };
        if !output.success {
            report
                .failed
                .push(Entry::new(repo_root, branch, output.stderr.trim()));
            return;
        }
        for warning in stale_window_warnings(&output.stderr) {
            report
                .stale_windows
                .push(Entry::new(repo_root, branch, warning));
        }
        details.push(format!("window {}", output.stdout.trim()));
    }
    if let Some(session) = target.session {
        if !session.older.is_empty() {
            details.push(format!(
                "older sessions ignored: {}",
                session.older.join(", ")
            ));
        }
        if let Some(theirs) = session
            .transcript_branch
            .as_deref()
            .filter(|b| *b != branch)
        {
            details.push(format!("transcript was on {theirs}"));
        }
    }

    let entry = Entry::new(repo_root, branch, details.join("; "));
    if target.session.is_some() {
        report.resumed.push(entry);
    } else {
        report.fresh.push(entry);
    }
}

impl Report {
    fn render(&self, projects_dir: &Path) -> String {
        let mut out = String::new();
        if self.scanned == 0 {
            out.push_str(&format!(
                "No transcripts found under {}/*--claude-worktrees-*/, so no worktrees were checked.\n",
                projects_dir.display()
            ));
            return out;
        }
        if self.dry_run {
            out.push_str("Dry run: no windows were opened.\n\n");
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

        out.push_str(&format!(
            "Transcripts scanned: {}, skipped: {}\n",
            self.scanned,
            self.skipped_transcripts.len()
        ));
        for (path, why) in &self.skipped_transcripts {
            out.push_str(&format!("  {}: {why}\n", path.display()));
        }
        out
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
            Some((
                PathBuf::from("/home/me/repo"),
                PathBuf::from("/home/me/repo/.claude/worktrees/feat+x")
            ))
        );
        assert_eq!(
            split_worktree_cwd("/home/me/repo/.claude/worktrees/feat+x"),
            Some((
                PathBuf::from("/home/me/repo"),
                PathBuf::from("/home/me/repo/.claude/worktrees/feat+x")
            ))
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
        assert_eq!(line.repo_root, PathBuf::from("/r"));
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
    fn stale_window_warnings_keeps_only_the_warning_lines() {
        let stderr = "something else\n\
                      Warning: session r already has a window named a/b (@3)\n";
        assert_eq!(
            stale_window_warnings(stderr),
            ["session r already has a window named a/b (@3)"]
        );
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
            let (success, stdout, stderr) =
                self.src.get(&args[3]).cloned().context("src not found")?;
            Ok(SrcOutput {
                success,
                stdout,
                stderr,
            })
        }
    }

    /// A repo with worktrees under .claude/worktrees, plus a projects dir
    /// holding a transcript for each of `with_transcripts`.
    struct Setup {
        base: PathBuf,
        repo: PathBuf,
        projects: PathBuf,
    }

    impl Setup {
        fn new(label: &str, dirs: &[&str]) -> Self {
            let base = temp_dir(label);
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
            let project = self.projects.join(format!("x--claude-worktrees-{dir}"));
            std::fs::create_dir_all(&project).unwrap();
            let line = serde_json::json!({
                "cwd": self.worktree(dir).join("src"),
                "gitBranch": branch,
                "sessionId": session_id,
            });
            std::fs::write(
                project.join(format!("{session_id}.jsonl")),
                format!("{line}\n"),
            )
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
                "e+resume", "e+fresh", "e+live", "e+merged", "e+nogh", "detached", "misnamed",
            ],
        );
        s.transcript("e+resume", "sid-resume", "HEAD");
        s.transcript("e+live", "sid-live", "e/live");
        s.transcript("e+merged", "sid-merged", "e/merged");
        s.transcript("e+nogh", "sid-nogh", "e/nogh");
        s.transcript("detached", "sid-detached", "HEAD");
        s.transcript("e+gone", "sid-gone", "e/gone");

        let mut fake = Fake {
            agents: Some(
                serde_json::json!([{ "cwd": s.worktree("e+live") }, { "cwd": "/elsewhere" }])
                    .to_string(),
            ),
            ..Fake::default()
        };
        fake.worktrees.insert(
            s.repo.clone(),
            s.porcelain(&[
                ("e+resume", Some("e/resume")),
                ("e+fresh", Some("e/fresh")),
                ("e+live", Some("e/live")),
                ("e+merged", Some("e/merged")),
                ("e+nogh", Some("e/nogh")),
                ("detached", None),
                ("misnamed", Some("other")),
            ]),
        );
        for branch in ["e/resume", "e/fresh", "e/live"] {
            fake.merged.insert(branch.into(), String::new());
        }
        fake.merged.insert("e/merged".into(), "42\n".into());

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        assert_eq!(discovery.scanned, 6);
        let report = sweep(&fake, discovery, Path::new(STARTISH), true).unwrap();

        assert!(fake.src_calls.borrow().is_empty());
        // Live is filtered before the merge check.
        assert!(!fake.gh_calls.borrow().contains(&"e/live".to_string()));

        let repo = s.repo.to_string_lossy();
        let rendered = report.render(&s.projects);
        let expected = format!(
            "Dry run: no windows were opened.\n\n\
             Would resume (2):\n\
             \x20 {repo}  e/nogh  (session sid-nogh; would run: src window {repo} -n e/nogh -- /bin/startish --history 'wtclaude --no-pull e/nogh' -- wtclaude --no-pull --resume sid-nogh e/nogh)\n\
             \x20 {repo}  e/resume  (session sid-resume; would run: src window {repo} -n e/resume -- /bin/startish --history 'wtclaude --no-pull e/resume' -- wtclaude --no-pull --resume sid-resume e/resume; transcript was on HEAD)\n\n\
             Would start fresh (1):\n\
             \x20 {repo}  e/fresh  (would run: src window {repo} -n e/fresh -- /bin/startish --history 'wtclaude --no-pull e/fresh' -- wtclaude --no-pull e/fresh)\n\n\
             Skipped, live (1):\n\
             \x20 {repo}  e/live\n\n\
             Skipped, merged (1):\n\
             \x20 {repo}  e/merged  (PR #42)\n\n\
             Merge check failed, treated as unmerged (1):\n\
             \x20 {repo}  e/nogh  (gh: no auth)\n\n\
             Detached or mismatched worktrees, left alone (2):\n\
             \x20 {repo}  {detached}  (detached)\n\
             \x20 {repo}  {misnamed}  (directory name doesn't match branch other)\n\n\
             Transcripts scanned: 6, skipped: 0\n",
            detached = s.worktree("detached").display(),
            misnamed = s.worktree("misnamed").display(),
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn launch_files_failures_stale_windows_and_window_ids() {
        let s = Setup::new("launch", &["ok", "stale", "bad"]);
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
            ]),
        );
        for branch in ["ok", "stale", "bad"] {
            fake.merged.insert(branch.into(), String::new());
        }
        fake.src
            .insert("ok".into(), (true, "@1\n".into(), String::new()));
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

        let discovery = discover(&s.projects, TAIL_BYTES).unwrap();
        let report = sweep(&fake, discovery, Path::new(STARTISH), false).unwrap();

        let entry = |what: &str, detail: &str| Entry::new(&s.repo, what, detail);
        assert_eq!(report.resumed, [entry("ok", "session sid-ok; window @1")]);
        assert_eq!(report.fresh, [entry("stale", "window @2")]);
        assert_eq!(report.failed, [entry("bad", "no such session")]);
        assert_eq!(
            report.stale_windows,
            [entry(
                "stale",
                "session repo already has a window named stale (@0)"
            )]
        );
        assert_eq!(fake.src_calls.borrow().len(), 3);
    }

    #[test]
    fn keeps_the_newest_session_per_worktree() {
        let s = Setup::new("newest", &["w"]);
        s.transcript("w", "old", "w");
        let old = s.projects.join("x--claude-worktrees-w").join("old.jsonl");
        let earlier = SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(earlier)
            .unwrap();
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
        let report = sweep(&Fake::default(), discovery, Path::new(STARTISH), true).unwrap();
        assert!(
            report
                .render(&s.projects)
                .starts_with("No transcripts found under")
        );
    }
}
