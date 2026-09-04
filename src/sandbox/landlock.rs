//! Linux Landlock backend.
//!
//! Unlike Seatbelt, there is no profile to compile and no external helper to
//! exec: the kernel takes rules directly. So each Bash command is wrapped in
//! `wtclaude sandbox`, which re-derives the plan, applies it to itself with
//! `landlock_restrict_self`, and then execs the command. Restrictions are
//! inherited across exec and by children, and they stack — so nested
//! wtclaude sessions compose, which `sandbox-exec` cannot do at all.

use anyhow::{Context, Result, bail};
use clap::Parser;
use landlock::{
    ABI, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus,
};
use std::path::{Path, PathBuf};

use super::{Availability, Backend, Plan, shell_single_quote};

pub struct Landlock;

/// The write rights we ask the kernel to govern.
///
/// Deliberately ABI V4's write set rather than V5's: V5 adds `IoctlDev`, and
/// handling it would restrict ioctls on device files — terminal ioctls
/// included — which macOS's `file-write*` never did. Read, execute and ioctl
/// rights are left unhandled, and therefore unrestricted, which is what makes
/// this a faithful port of "deny file writes, allow these paths".
const WRITE_RIGHTS_ABI: ABI = ABI::V4;

fn write_rights() -> BitFlags<AccessFs> {
    AccessFs::from_write(WRITE_RIGHTS_ABI)
}

/// The shell the wrapper execs. Claude's Bash tool emits bash, so `/bin/sh`
/// would break on distros where that's dash.
const SHELL: &str = "/bin/bash";

impl Backend for Landlock {
    fn name(&self) -> &'static str {
        "landlock"
    }

    fn availability(&self) -> Availability {
        if !supports(write_rights()) {
            return Availability::Unavailable(
                "the kernel has no usable Landlock support. It needs Linux 5.13+ built with \
                 CONFIG_SECURITY_LANDLOCK and landlock enabled in the active LSM list — check \
                 /sys/kernel/security/lsm and the kernel's lsm= boot parameter."
                    .to_string(),
            );
        }
        if !supports(AccessFs::Refer) {
            return Availability::Unavailable(
                "this kernel's Landlock is ABI 1, which has no REFER right. The kernel then \
                 denies every cross-directory rename once a ruleset is enforced, which breaks \
                 git, cargo and npm. Needs Linux 5.19+ (ABI 2)."
                    .to_string(),
            );
        }
        if !supports(AccessFs::Truncate) {
            return Availability::Degraded(
                "this kernel's Landlock predates the TRUNCATE right (Linux 6.2 / ABI 3), so \
                 truncate(2) on a path outside the sandbox is not restricted. Writes through \
                 shell redirection and open(O_TRUNC) are still covered."
                    .to_string(),
            );
        }
        Availability::Ready
    }

    fn describe(&self, plan: &Plan) -> String {
        let mut out = String::from(
            "# wtclaude sandbox plan (landlock)\n\
             # All file writes are denied except beneath the paths listed below.\n\
             # Reads, execs and ioctls are not restricted.\n",
        );
        out.push_str(&format!(
            "# kernel supports: refer={} truncate={}\n\
             # sandbox root: {}\n\n",
            yes_no(supports(AccessFs::Refer)),
            yes_no(supports(AccessFs::Truncate)),
            plan.sandbox_root.display(),
        ));

        let (present, missing): (Vec<_>, Vec<_>) = plan
            .write_subtrees
            .iter()
            .chain(plan.sockets.iter())
            .partition(|p| p.exists());
        for p in present {
            out.push_str(&format!("write-subtree {}\n", p.display()));
        }
        if !missing.is_empty() {
            out.push_str(
                "\n# Skipped: a Landlock rule needs a path that already exists. These are\n\
                 # re-checked on every Bash command, so they become writable as soon as\n\
                 # something outside the sandbox creates them.\n",
            );
            for p in missing {
                out.push_str(&format!("# missing {}\n", p.display()));
            }
        }
        for warning in self.unhonored(plan) {
            out.push_str(&format!("\n# NOT HONORED: {warning}\n"));
        }
        out
    }

    /// `plan` is unused: the wrapper re-derives it from the environment after
    /// this hook process has exited, so that the process which applies the
    /// rules is the one that computed them.
    fn confine(&self, _plan: &Plan, command: &str, session_dir: &Path) -> Result<String> {
        if !session_dir.is_dir() {
            bail!(
                "session dir {} does not exist; the launching wtclaude process is gone",
                session_dir.display()
            );
        }
        let exe = std::env::current_exe().context("resolving wtclaude binary path")?;
        Ok(format!(
            "{} sandbox -- {SHELL} -c {}",
            shell_single_quote(&exe.to_string_lossy()),
            shell_single_quote(command)
        ))
    }

    fn notice(&self) -> &'static str {
        "Privilege escalation is impossible in this session, by construction: the kernel \
         requires the NO_NEW_PRIVS flag before an unprivileged process may sandbox itself, \
         and that flag also prevents setuid binaries from elevating. `sudo`, `doas`, \
         `pkexec` and `su` therefore cannot work no matter how they are invoked, and \
         attempts will be rejected before they run. Do not try to install system packages \
         or otherwise act as root — ask the user to do it outside the sandbox instead. \
         Writes outside the sandbox fail with 'Permission denied' (EACCES) at the kernel \
         level; reads are not restricted."
    }

    fn heredocs_blocked(&self) -> bool {
        // The heredoc ban exists for Apple's bash 3.2, which miscounts quote
        // nesting for a heredoc inside `$(...)` inside double quotes. Linux
        // bash parses that shape correctly.
        false
    }

    fn privilege_escalation_blocked(&self) -> bool {
        true
    }

    fn unhonored(&self, plan: &Plan) -> Vec<String> {
        plan.write_globs
            .iter()
            .map(|pattern| {
                format!(
                    "allowlist entry {pattern:?} is a glob, and Landlock has no path-pattern \
                     matching — it grants rights per directory or file only, and unions \
                     overlapping rules, so it cannot express \"this directory but only these \
                     names\". Nothing is granted for this entry. If it exists for an \
                     atomic-write dance (write {{file}}.{{tmpid}}, rename over {{file}}), that \
                     needs create+unlink on the parent directory, which for a file directly in \
                     $HOME would mean granting all of $HOME. See \
                     docs/adr/0002-glob-allowlist-entries-are-seatbelt-only.md."
                )
            })
            .collect()
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Asks whether this kernel can govern `access`.
///
/// The landlock crate deliberately exposes no "current ABI" accessor —
/// runtime ABI detection invites sandboxes that differ between runs — so
/// "does the kernel support X" is asked as "would demanding X fail". The
/// ruleset built here is created and immediately dropped; nothing is enforced
/// because `restrict_self` is never called.
fn supports<T: Into<BitFlags<AccessFs>>>(access: T) -> bool {
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(access.into())
        .and_then(|r| r.create())
        .is_ok()
}

/// Applies `plan` to the calling process. Irreversible, and inherited by
/// every child and across exec.
fn restrict(plan: &Plan) -> Result<()> {
    let mut ruleset = Ruleset::default()
        .handle_access(write_rights())
        .context("building landlock ruleset")?
        .create()
        .context("creating landlock ruleset")?;

    for path in plan.write_subtrees.iter().chain(plan.sockets.iter()) {
        // A rule needs an open fd, so a path that doesn't exist yet cannot be
        // granted. Skipping is correct rather than fatal: writing *into* a
        // missing directory is impossible anyway while its parent is
        // ungranted, and the plan is re-derived per Bash command, so the
        // grant appears as soon as the path does. Silent by design — this
        // runs on every Bash call and its stderr lands in the agent's
        // transcript.
        let Ok(fd) = PathFd::new(path) else { continue };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, write_rights()))
            .with_context(|| format!("adding landlock rule for {}", path.display()))?;
    }

    let status = ruleset
        .restrict_self()
        .context("applying landlock ruleset")?;
    if status.ruleset == RulesetStatus::NotEnforced {
        bail!(
            "the kernel reported that no Landlock restrictions were applied, so file writes \
             would be unrestricted"
        );
    }
    Ok(())
}

/// Runs a command under the sandbox described by the current environment.
///
/// Invoked by the `PreToolUse` hook as
/// `wtclaude sandbox -- /bin/bash -c '<command>'`. Reads the sandbox root and
/// repo root from `WTCLAUDE_SANDBOX` / `WTCLAUDE_REPO_ROOT` rather than argv,
/// since those are already exported to every child of the session.
#[derive(Parser)]
pub struct SandboxArgs {
    /// Print the rules that would be applied and exit without running anything
    #[arg(long)]
    describe: bool,
    /// Program and arguments to run inside the sandbox
    #[arg(
        value_name = "COMMAND",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

pub fn run_sandbox(args: SandboxArgs) -> Result<i32> {
    let sandbox_root = std::env::var("WTCLAUDE_SANDBOX")
        .ok()
        .filter(|s| !s.is_empty())
        .context(
            "WTCLAUDE_SANDBOX is not set, so there is no sandbox root to confine to. \
             `wtclaude sandbox` is invoked by wtclaude's PreToolUse hook, not directly.",
        )?;
    let repo_root = std::env::var("WTCLAUDE_REPO_ROOT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let plan = super::plan(Path::new(&sandbox_root), repo_root.as_deref())?;

    if args.describe {
        print!("{}", Landlock.describe(&plan));
        return Ok(0);
    }
    if args.command.is_empty() {
        bail!("nothing to run: expected `wtclaude sandbox -- <program> [args...]`");
    }

    restrict(&plan)?;

    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .exec();
    Err(err).with_context(|| format!("running {} under the sandbox", args.command[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_is_ready_on_a_modern_kernel() {
        // Guards the probe itself: if `supports` were broken, every backend
        // would report unavailable and wtclaude would refuse to launch.
        match Landlock.availability() {
            Availability::Ready => {}
            Availability::Degraded(caveat) => {
                assert!(caveat.contains("TRUNCATE"), "unexpected caveat: {caveat}")
            }
            Availability::Unavailable(why) => {
                panic!("landlock should be usable on this kernel: {why}")
            }
        }
    }

    #[test]
    fn write_rights_exclude_ioctl_dev_so_terminal_ioctls_stay_unrestricted() {
        // Regression test: bumping WRITE_RIGHTS_ABI to V5 silently starts
        // governing IoctlDev, which would make ioctls on tty devices fail
        // for anything not granted /dev — a restriction macOS's file-write*
        // never imposed.
        assert!(!write_rights().contains(AccessFs::IoctlDev));
        assert!(write_rights().contains(AccessFs::WriteFile));
        assert!(write_rights().contains(AccessFs::Refer));
        assert!(write_rights().contains(AccessFs::Truncate));
    }

    #[test]
    fn confine_re_enters_this_binary_with_a_quoted_command() {
        let dir =
            std::env::temp_dir().join(format!("wtclaude-test-confine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = super::super::plan(&dir, None).unwrap();

        let wrapped = Landlock.confine(&plan, "echo it's fine", &dir).unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            wrapped.contains(" sandbox -- /bin/bash -c "),
            "got: {wrapped}"
        );
        assert!(
            wrapped.ends_with(r#"'echo it'\''s fine'"#),
            "the command must be single-quoted with embedded quotes escaped: {wrapped}"
        );
    }

    #[test]
    fn confine_refuses_when_the_session_dir_is_gone() {
        let dir = std::env::temp_dir().join("wtclaude-test-confine-absent-session-dir");
        let _ = std::fs::remove_dir_all(&dir);
        let plan = super::super::plan(Path::new("/tmp"), None).unwrap();

        let err = Landlock.confine(&plan, "echo hi", &dir).unwrap_err();
        assert!(err.to_string().contains("session dir"), "error: {err}");
    }

    #[test]
    fn unhonored_reports_every_glob_entry() {
        let plan = Plan {
            sandbox_root: PathBuf::from("/home/x/wt"),
            write_subtrees: vec![PathBuf::from("/home/x/wt")],
            write_globs: vec!["/home/x/.claude.json*".to_string()],
            sockets: vec![],
        };
        let warnings = Landlock.unhonored(&plan);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(".claude.json*"));
        assert!(warnings[0].contains("Nothing is granted"));
    }
}
