//! The OS-level file-write sandbox, abstracted over its backends.
//!
//! Every backend enforces the same idea — deny all file writes, then allow
//! back a specific set of paths — but they disagree about almost everything
//! else, including whether a profile file exists at all. So the currency
//! here is the [`Plan`]: the platform-neutral set of writable paths. A
//! [`Backend`] turns a `Plan` plus a command into the command line that runs
//! that command confined.
//!
//! See `docs/adr/0001-sandbox-backends-behind-a-trait.md` for why the trait
//! is shaped this way, and `CONTEXT.md` for the vocabulary (Plan, Backend,
//! Profile, Sandbox root, Sandbox notice).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::config;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!(
    "wtclaude's sandbox supports macOS (Seatbelt/sandbox-exec) and Linux (Landlock) only"
);

#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "macos")]
pub mod seatbelt;

#[cfg(target_os = "linux")]
pub use landlock::{SandboxArgs, run_sandbox};

/// The platform-neutral answer to "what may this session write to?", before
/// any backend syntax is involved.
pub struct Plan {
    /// The one directory the session owns.
    pub sandbox_root: PathBuf,
    /// Paths whose entire subtree is writable. Includes `sandbox_root`.
    pub write_subtrees: Vec<PathBuf>,
    /// Allowlist entries containing `*`. Kept as patterns rather than
    /// resolved paths because no real path contains a literal `*`; each
    /// backend decides what, if anything, it can do with them.
    pub write_globs: Vec<String>,
    /// Unix-socket paths from `socket_allowlist`, needing write access and
    /// (on macOS) explicit network-bind/connect rules.
    pub sockets: Vec<PathBuf>,
}

/// Whether a backend can enforce a plan on this machine right now.
pub enum Availability {
    Ready,
    /// Usable, but with a caveat worth surfacing once at launch. Only
    /// backends whose capabilities vary by kernel version construct this, so
    /// it is unused on a macOS build.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Degraded(String),
    Unavailable(String),
}

pub trait Backend {
    /// Stable identifier, as accepted by `sandbox-backend` in wtclaude.yml
    /// and exported as `WTCLAUDE_BACKEND`.
    fn name(&self) -> &'static str;

    fn availability(&self) -> Availability;

    /// Human-readable rendering of what would be enforced, for
    /// `--show-policy`. For Seatbelt this is the SBPL profile verbatim.
    fn describe(&self, plan: &Plan) -> String;

    /// The command line that runs `command` confined by `plan`. May write
    /// files into `session_dir`, which outlives every Bash call.
    fn confine(&self, plan: &Plan, command: &str, session_dir: &Path) -> Result<String>;

    /// The backend-specific half of the sandbox notice appended to the
    /// system prompt: what this backend forbids that a session would
    /// otherwise waste turns discovering.
    fn notice(&self) -> &'static str;

    /// Whether heredocs must be refused outright for this backend's shell.
    /// True only for Seatbelt, whose bash 3.2 miscounts quote nesting for a
    /// heredoc inside `$(...)` inside double quotes.
    fn heredocs_blocked(&self) -> bool;

    /// Whether `sudo` and friends can never succeed under this backend, so
    /// the hook should say why instead of letting the kernel refuse.
    fn privilege_escalation_blocked(&self) -> bool;

    /// Plan entries this backend cannot honor, described for the user. Not
    /// an error: an allowlist shared between machines may legitimately
    /// contain entries only one backend can express.
    fn unhonored(&self, plan: &Plan) -> Vec<String>;
}

/// Package manager cache/data dirs, relative to `$HOME` — caching only, not
/// installers (no homebrew etc.).
const PKG_CACHE_DIRS: &[&str] = &[
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

/// Tool state dirs, relative to `$HOME`, that a session must be able to
/// write for ordinary work to succeed.
///
/// `.gnupg` is here because `commit.gpgsign` makes signing part of
/// committing, and gpg takes a dotlock (`.#lk<addr>` hardlinked to
/// `pubring.kbx.lock`) before it will sign — so it needs create+unlink in
/// the keyring *directory*, not write on any single file. It writes nothing
/// to `trustdb.gpg` in the process; the lock is pessimism against a
/// concurrent gpg rewriting the keybox mid-read.
const TOOL_STATE_DIRS: &[&str] = &[".gnupg"];

#[cfg(target_os = "macos")]
const PLATFORM_WRITE_SUBTREES: &[&str] = &["/private/tmp", "/var/folders", "/private/var/folders"];
/// `Library/Keychains` is here so tools like saml2aws can store tokens.
#[cfg(target_os = "macos")]
const PLATFORM_HOME_DIRS: &[&str] = &["Library/Caches/cargo-xwin", "Library/Keychains"];

/// `/dev` is granted wholesale rather than enumerated. Ordinary filesystem
/// permissions still stop a non-root session from writing real devices, and
/// the enumeration that would otherwise be needed is long and easy to leave
/// a gap in: `/dev/null` alone breaks git, mktemp and shell redirection when
/// missing, and `/dev/{tty,pts,shm,zero,full,random,urandom}` all have
/// callers too.
#[cfg(target_os = "linux")]
const PLATFORM_WRITE_SUBTREES: &[&str] = &["/var/tmp", "/dev"];
#[cfg(target_os = "linux")]
const PLATFORM_HOME_DIRS: &[&str] = &[".cache/go-build"];

/// Backends in order of preference for `auto`, most preferred first.
fn candidates() -> Vec<Box<dyn Backend>> {
    #[cfg(target_os = "macos")]
    return vec![Box::new(seatbelt::Seatbelt)];
    #[cfg(target_os = "linux")]
    return vec![Box::new(landlock::Landlock)];
}

/// Backend names recognized on this platform but not implemented yet, so
/// asking for one gets a straight answer instead of "unknown backend".
#[cfg(target_os = "linux")]
const RESERVED: &[(&str, &str)] = &[(
    "bubblewrap",
    "the bubblewrap backend is not implemented yet; \
     it is reserved for the case where `sudo` must work inside sandboxed Bash \
     (see docs/adr/0001-sandbox-backends-behind-a-trait.md). Use `landlock` or `auto`.",
)];
#[cfg(target_os = "macos")]
const RESERVED: &[(&str, &str)] = &[];

/// Resolves `preference` (`auto`, or a backend name) to a usable backend.
///
/// Fails closed: if nothing on this machine can enforce a plan, that's an
/// error rather than an unsandboxed session, matching how
/// `config::validate_allowlist` and `launch::is_sandbox_nullifying_root`
/// refuse to start rather than silently under-protect.
pub fn select(preference: &str) -> Result<(Box<dyn Backend>, Option<String>)> {
    if let Some((_, why)) = RESERVED.iter().find(|(name, _)| *name == preference) {
        bail!("{why}");
    }

    let candidates = candidates();
    if preference != "auto" {
        let backend = candidates
            .into_iter()
            .find(|b| b.name() == preference)
            .with_context(|| {
                format!(
                    "unknown sandbox-backend {:?}; available on this platform: {}",
                    preference,
                    candidates_names().join(", ")
                )
            })?;
        return match backend.availability() {
            Availability::Ready => Ok((backend, None)),
            Availability::Degraded(caveat) => Ok((backend, Some(caveat))),
            Availability::Unavailable(why) => bail!(
                "sandbox-backend {:?} is configured but unusable: {why}",
                preference
            ),
        };
    }

    let mut refusals = Vec::new();
    for backend in candidates {
        match backend.availability() {
            Availability::Ready => return Ok((backend, None)),
            Availability::Degraded(caveat) => return Ok((backend, Some(caveat))),
            Availability::Unavailable(why) => refusals.push(format!("{}: {why}", backend.name())),
        }
    }
    bail!(
        "no usable sandbox backend on this machine, so every file write would be \
         unrestricted — refusing to start. Tried: {}",
        refusals.join("; ")
    )
}

fn candidates_names() -> Vec<&'static str> {
    candidates().iter().map(|b| b.name()).collect()
}

/// Computes the writable path set for a session. `repo_root` is `None` for
/// `wtclaude path` against a directory outside any git repo.
///
/// Derived fresh per Bash call rather than cached at launch: it costs single
/// digit milliseconds against a hook that already pays a process spawn, and
/// Landlock rules need a path that already exists, so a cache dir created
/// mid-session can only become writable if the plan is recomputed.
pub fn plan(sandbox_root: &Path, repo_root: Option<&Path>) -> Result<Plan> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());

    let user_config = config::load_user()?;
    let (write_globs, literal_allow) = partition_allowlist(&user_config.allowlist, &home);

    let raw: Vec<PathBuf> = [sandbox_root.to_path_buf(), PathBuf::from("/tmp")]
        .into_iter()
        .chain(PLATFORM_WRITE_SUBTREES.iter().map(PathBuf::from))
        .chain(std::iter::once(PathBuf::from(&tmpdir)))
        .chain(agent_socket_dir())
        .chain(repo_root.map(crate::launch::git_dirs).unwrap_or_default())
        .chain(
            PKG_CACHE_DIRS
                .iter()
                .chain(PLATFORM_HOME_DIRS.iter())
                .chain(TOOL_STATE_DIRS.iter())
                .map(|d| PathBuf::from(&home).join(d)),
        )
        .chain(literal_allow.into_iter().map(PathBuf::from))
        .collect();

    let mut seen = std::collections::HashSet::new();
    let write_subtrees: Vec<PathBuf> = raw
        .into_iter()
        .map(resolve)
        .filter(|p| seen.insert(p.clone()))
        .collect();

    let sockets: Vec<PathBuf> = user_config
        .socket_allowlist
        .iter()
        .map(|p| resolve(PathBuf::from(p.replace('~', &home))))
        .collect();

    Ok(Plan {
        sandbox_root: resolve(sandbox_root.to_path_buf()),
        write_subtrees,
        write_globs,
        sockets,
    })
}

/// `$XDG_RUNTIME_DIR` on Linux — where ssh-agent and gpg-agent sockets live,
/// so an agent that pushes over SSH or signs via `gpg.format=ssh` can reach
/// them. The macOS equivalent is configured per-machine via
/// `socket_allowlist`, so there's nothing to add there.
fn agent_socket_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    return std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from);
    #[cfg(not(target_os = "linux"))]
    return None;
}

/// The generic half of the sandbox notice, true of every backend. Each
/// backend's [`Backend::notice`] is appended to this.
pub fn sandbox_warning_common() -> &'static str {
    "Do not attempt to create new worktrees (e.g. via `git worktree add`, `wtclaude new`, \
     or an EnterWorktree/spawn-agent-in-worktree tool) from within this sandbox: creating a \
     worktree requires writing to the main repository's `.git` directory, which is outside \
     this sandbox and will be rejected."
}

/// Resolve a literal allowlist/sandbox path for the subtree rule it becomes.
/// Delegates to `config::resolve_existing_prefix`, which is a strict superset
/// of a plain `canonicalize().unwrap_or(p)`: identical result when `p` exists
/// in full, but also walks up to the longest existing ancestor and
/// canonicalizes *that* when it doesn't — e.g. a literal entry for a
/// not-yet-created directory under `/tmp` (symlinked to `/private/tmp` on
/// macOS) still resolves through the symlink instead of compiling into a rule
/// that can never match the kernel-resolved path. Keeps literal-entry
/// resolution consistent with `resolve_glob_prefix` and `hook.rs`'s
/// `normalize_path`.
fn resolve(p: PathBuf) -> PathBuf {
    config::resolve_existing_prefix(&p)
}

/// Splits `allowlist` into entries containing a `*` glob and plain literal
/// entries, expanding `~` to `home` in both. Pure and parameterized by `home`
/// (rather than reading `$HOME` itself) so it's directly unit testable
/// without mutating global process state.
fn partition_allowlist(allowlist: &[String], home: &str) -> (Vec<String>, Vec<String>) {
    allowlist
        .iter()
        .map(|p| p.replace('~', home))
        .partition(|p| p.contains('*'))
}

/// Wraps `s` in single quotes for embedding in a shell command line.
///
/// This is the only barrier between an arbitrary Bash command and the second
/// shell parse that every backend's wrapper performs, so it must reconstruct
/// the original bytes exactly.
pub fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// A per-session scratch directory, owned by the launcher for the lifetime of
/// the session.
///
/// Backends that must materialize a profile write it here. The `PreToolUse`
/// hook is a separate short-lived process that exits before Claude runs the
/// command it rewrote, so a guard in the hook would delete the profile before
/// the wrapper could open it — the guard has to live at session scope.
///
/// Deliberately under `~/.local/state`, which is *not* in the plan: `/tmp` is
/// writable by the session, so a profile kept there could be rewritten by the
/// session it constrains.
pub struct SessionDir {
    path: PathBuf,
    /// Held open for the whole session. The kernel drops this lock when the
    /// process exits, however it exits, which is what lets a later launch
    /// tell an orphaned scratch dir from one still in use.
    _lock: std::fs::File,
}

/// Name of the lock file inside each session dir.
const LOCK_FILE: &str = "owner.lock";

/// Fallback age limit for a session dir with no lock file — one left by an
/// older wtclaude, or a partially created one. Dirs that *do* have a lock
/// file are judged by the lock, not by age.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

impl SessionDir {
    pub fn create() -> Result<Self> {
        let base = Self::base()?;
        std::fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
        sweep_orphaned(&base);

        let path = base.join(std::process::id().to_string());
        // A recycled pid could collide with an orphaned dir; start clean.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;

        let lock_path = path.join(LOCK_FILE);
        let lock = std::fs::File::create(&lock_path)
            .with_context(|| format!("creating {}", lock_path.display()))?;
        lock.try_lock()
            .with_context(|| format!("locking {}", lock_path.display()))?;
        Ok(Self { path, _lock: lock })
    }

    fn base() -> Result<PathBuf> {
        match std::env::var("XDG_STATE_HOME") {
            Ok(state) if !state.is_empty() => Ok(PathBuf::from(state).join("wtclaude/sessions")),
            _ => {
                let home = std::env::var("HOME").context("HOME not set")?;
                Ok(PathBuf::from(home).join(".local/state/wtclaude/sessions"))
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Removes scratch dirs whose owning session is gone.
///
/// Liveness is decided by whether the dir's lock can be acquired, not by age:
/// a session that runs for weeks keeps its lock, while one killed with
/// SIGKILL loses it immediately. An age check on its own would eventually
/// delete a long-running session's profile out from under it, which shows up
/// as Bash being denied mid-session.
fn sweep_orphaned(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let orphaned = match std::fs::File::open(entry.path().join(LOCK_FILE)) {
            // Acquiring the lock means nobody holds it, so the owner exited.
            // The lock is released again when this handle drops, just after
            // the directory it belongs to has been removed.
            Ok(lock) => lock.try_lock().is_ok(),
            Err(_) => entry
                .metadata()
                .and_then(|m| m.modified())
                .and_then(|t| now.duration_since(t).map_err(std::io::Error::other))
                .map(|age| age > STALE_AFTER)
                .unwrap_or(false),
        };
        if orphaned {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

// `pub(crate)` because `run_via_outer_shell` is shared with hook.rs's
// Seatbelt round-trip test: both exercise the same double shell parse, and
// duplicating its diagnostics in two test modules is worse than exporting it.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn resolve_resolves_a_symlinked_existing_ancestor_for_a_nonexistent_literal_entry() {
        // Regression test: a literal (non-glob) allowlist entry for a path
        // that doesn't exist yet under /tmp must still resolve the same way
        // the kernel will (through the /private/tmp symlink on macOS; /tmp is
        // already real on Linux) — otherwise the emitted rule can never match
        // the kernel-resolved path, silently denying Bash writes that
        // Write/Edit/NotebookEdit (via hook.rs's normalize_path) would allow
        // for the same entry.
        let tmp = std::fs::canonicalize("/tmp").unwrap();
        let resolved = resolve(PathBuf::from(
            "/tmp/wtclaude-test-nonexistent-literal-entry",
        ));
        assert_eq!(
            resolved,
            tmp.join("wtclaude-test-nonexistent-literal-entry")
        );
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

    fn unique_test_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wtclaude-test-{}-{}", std::process::id(), label));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn plan_includes_the_sandbox_root_and_the_temp_dirs() {
        let sandbox = unique_test_dir("plan-sandbox-root");
        let plan = plan(&sandbox, None).unwrap();
        assert_eq!(plan.sandbox_root, sandbox);
        assert!(plan.write_subtrees.contains(&sandbox));
        assert!(
            plan.write_subtrees
                .contains(&std::fs::canonicalize("/tmp").unwrap())
        );
        std::fs::remove_dir_all(&sandbox).unwrap();
    }

    #[test]
    fn plan_includes_the_gnupg_dir_so_signed_commits_work() {
        // Regression test: `commit.gpgsign` makes signing part of committing,
        // and gpg needs create+unlink in the keyring dir for its dotlock. A
        // plan without this makes `git commit` fail with "failed to write
        // commit object", which points nowhere near the sandbox.
        let sandbox = unique_test_dir("plan-gnupg");
        let plan = plan(&sandbox, None).unwrap();
        let home = std::env::var("HOME").unwrap();
        let gnupg = config::resolve_existing_prefix(&PathBuf::from(&home).join(".gnupg"));
        assert!(
            plan.write_subtrees.contains(&gnupg),
            "expected {} in {:?}",
            gnupg.display(),
            plan.write_subtrees
        );
        std::fs::remove_dir_all(&sandbox).unwrap();
    }

    #[test]
    fn plan_omits_repo_git_dirs_when_repo_root_is_none() {
        let repo_root = unique_test_dir("plan-no-repo-root");
        let sandbox = unique_test_dir("plan-no-repo-root-sandbox");
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo_root)
                .status()
                .unwrap()
                .success()
        );

        let with_repo = plan(&sandbox, Some(&repo_root)).unwrap();
        let without_repo = plan(&sandbox, None).unwrap();

        std::fs::remove_dir_all(&repo_root).unwrap();
        std::fs::remove_dir_all(&sandbox).unwrap();

        assert!(with_repo.write_subtrees.contains(&repo_root.join(".git")));
        assert!(
            !without_repo
                .write_subtrees
                .contains(&repo_root.join(".git"))
        );
    }

    #[test]
    fn select_auto_resolves_to_this_platforms_backend() {
        let (backend, _caveat) = select("auto").expect("a usable backend on the test machine");
        #[cfg(target_os = "macos")]
        assert_eq!(backend.name(), "seatbelt");
        #[cfg(target_os = "linux")]
        assert_eq!(backend.name(), "landlock");
    }

    #[test]
    fn select_rejects_an_unknown_backend_name() {
        let err = select("nonesuch")
            .err()
            .expect("an unknown name must be rejected");
        assert!(
            err.to_string().contains("unknown sandbox-backend"),
            "error: {err}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn select_explains_that_bubblewrap_is_reserved_rather_than_unknown() {
        let err = select("bubblewrap")
            .err()
            .expect("a reserved name must be rejected");
        assert!(
            err.to_string().contains("not implemented yet"),
            "error: {err}"
        );
    }

    #[test]
    fn session_dir_is_outside_the_plan_and_removed_on_drop() {
        // The profile a session runs under must not be writable by that
        // session, so the scratch dir deliberately lives outside every
        // granted subtree.
        let sandbox = unique_test_dir("session-dir-plan");
        let plan = plan(&sandbox, None).unwrap();
        let session = SessionDir::create().unwrap();
        let path = session.path().to_path_buf();

        assert!(path.is_dir());
        for granted in &plan.write_subtrees {
            assert!(
                !path.starts_with(granted),
                "session dir {} must not sit under granted subtree {}",
                path.display(),
                granted.display()
            );
        }

        drop(session);
        assert!(!path.exists());
        std::fs::remove_dir_all(&sandbox).unwrap();
    }

    #[test]
    fn sweeping_spares_a_live_session_and_removes_an_orphaned_one() {
        // Regression test: judging staleness by age alone would eventually
        // delete a long-running session's scratch dir while it was still in
        // use, which surfaces as Bash being denied mid-session.
        let session = SessionDir::create().unwrap();
        let live = session.path().to_path_buf();
        let base = live.parent().expect("base dir").to_path_buf();

        // An orphan: same shape, but with no process holding its lock.
        let orphan = base.join("orphan-not-a-live-pid");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::File::create(orphan.join(LOCK_FILE)).unwrap();

        sweep_orphaned(&base);

        assert!(live.is_dir(), "a locked session dir must survive the sweep");
        assert!(!orphan.exists(), "an unlocked session dir must be swept");
        drop(session);
    }

    // Runs `command` the way the sandbox wrapper's output is ultimately run: as
    // the text of an outer `sh -c`. This reproduces the double shell-parse from
    // production (`sandbox-exec ... sh -c '<escaped>'` invoked as one command
    // line by the harness), not just a single argv-level exec.
    pub(crate) fn run_via_outer_shell(command: &str) -> String {
        eprintln!("--- running via sh -c ---\n{command}\n--- end command ---");
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("failed to spawn sh");
        eprintln!(
            "--- exit status: {:?} ---\n--- stdout ---\n{}--- stderr ---\n{}--- end output ---",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "command failed (status {:?}):\ncommand: {command}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn shell_single_quote_literal_output_for_known_inputs() {
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_single_quote("''"), "''\\'''\\'''");
    }

    #[test]
    fn shell_single_quote_reconstructs_adversarial_bodies_byte_for_byte() {
        // shell_single_quote() is the only barrier between an arbitrary Bash
        // command and a second `sh -c` re-parse inside sandbox-exec. Rather
        // than executing adversarial-looking content directly (risky if a
        // real escaping bug let something run), assert that the outer
        // `sh -c '<escaped>'` reconstructs the original bytes exactly via
        // `printf '%s'`. Exact byte-for-byte reconstruction is what
        // guarantees the wrapped command behaves identically to running the
        // original directly — i.e. nothing can "escape" the outer quoting.
        let adversarial_bodies = [
            "already just plain text",
            "leading quote: 'text",
            "trailing quote: text'",
            "adjacent quotes: ''",
            "looks like a breakout attempt: '; echo INJECTED; echo '",
            "odd count: it's a 'test' of 'quoting",
        ];

        for original in adversarial_bodies {
            let escaped = shell_single_quote(original);
            let probe = format!("printf '%s' {escaped}");
            let reconstructed = run_via_outer_shell(&probe);
            assert_eq!(
                reconstructed, original,
                "failed to reconstruct: {original:?}"
            );
        }
    }

    #[test]
    fn shell_single_quote_survives_quoted_heredoc() {
        // Minimal repro: a heredoc whose quoted delimiter and body both
        // contain literal single quotes for shell_single_quote() to escape.
        let original = "cat <<'EOF'\nHello 'World'\nFrom Heredoc\nEOF";
        let escaped = shell_single_quote(original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, "Hello 'World'\nFrom Heredoc\n");
    }

    #[test]
    fn shell_single_quote_survives_real_world_commit_command() {
        // Body copied verbatim from the original bug report. Keep the
        // apostrophe in "run()'s" — it's the one embedded single quote that
        // makes this test exercise shell_single_quote()'s escaping at all.
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Moves run()'s pty/reader/writer/emulator/cols/rows locals into an App\n\
struct, with App::new() owning setup and run_loop() holding the event\n\
loop, so future state (e.g. a session tree) has somewhere to live.\n\
Also adds a term_emulator::new_emulator() factory so app.rs no longer\n\
names the concrete AlacrittyTerminalEmulator type directly.\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>\n";
        let original = format!("cat <<'EOF'\n{body}EOF");

        let escaped = shell_single_quote(&original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, body);
    }

    #[test]
    fn shell_single_quote_survives_command_substitution_around_heredoc() {
        // Structural shape matches the real failing command — a double-quoted
        // command substitution wrapping a heredoc with a single-quoted
        // delimiter: git commit -m "$(cat <<'EOF' ... EOF)". The body
        // intentionally omits the apostrophe (see
        // shell_single_quote_survives_real_world_commit_command) that
        // triggers the separate, unrelated bash 3.2 heredoc-in-$()-in-""
        // parsing bug (see the issue file) — that bug is not fixable here,
        // so this test isolates shell_single_quote()'s own correctness from
        // it. Swap `git commit -m` for `printf '%s\n'` so this runs
        // standalone without needing a git repo/staged changes.
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>";
        let original = format!("printf '%s\\n' \"$(cat <<'EOF'\n{body}\nEOF\n)\"");

        let escaped = shell_single_quote(&original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, format!("{body}\n"));
    }
}
