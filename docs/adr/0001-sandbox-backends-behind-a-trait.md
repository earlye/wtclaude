---
status: accepted
---

# Sandbox enforcement sits behind a Backend trait, with Landlock on Linux

Enforcement was hardcoded to macOS Seatbelt: a generated SBPL profile plus
`/usr/bin/sandbox-exec` per Bash call. To support Linux we put both platforms
behind one `Backend` trait whose currency is the platform-neutral **Plan**
(the set of writable paths), not a rendered profile — because the three
mechanisms don't agree that a profile file even exists. Linux uses Landlock,
applied in-process by a `wtclaude sandbox` subcommand that re-derives the
Plan and then execs the command.

## Considered options

- **Landlock** (chosen): no external dependency, unprivileged, restricts only
  write-type rights so read/exec/ioctl stay untouched — a near 1:1 match for
  today's deny-all-writes-then-allow-subpaths model. Rulesets also stack, so
  a wrapper run inside an existing sandbox narrows further rather than
  failing, which `sandbox-exec` cannot do at all. (A whole nested *session*
  still cannot launch, on either backend: the launcher writes
  `~/.claude.json` and its scratch dir, both outside the plan by design.)
- **bubblewrap**: kept in reserve behind the same trait. It's a packaged
  runtime dependency and brings a large behavioral surface (/proc, /dev, /run
  remounts, namespace-visible process tables, EXDEV on cross-bind renames).
  Its one real advantage is that `sudo` keeps working.
- **seccomp user-notify**: rejected as disproportionate. It is the only option
  that could reproduce SBPL's path-pattern matching faithfully, but it needs a
  supervisor process, fd passing, and has inherent TOCTOU races — far too much
  machinery given the threat model is accidental writes first.

## Consequences

- **The Plan is re-derived per Bash call**, not cached at launch. It costs
  single-digit milliseconds against a hook that already pays a process spawn,
  and it is close to mandatory on Linux: Landlock rules need a path that
  already exists, so a cache dir created mid-session can only become writable
  if the Plan is recomputed.
- **Only Seatbelt materializes a profile.** `sandbox-exec -f` demands a path
  on disk; Landlock applies rules in-process and bubblewrap puts them in argv.
  The profile file is therefore a Seatbelt implementation detail, not part of
  the trait, and `WTCLAUDE_SBPL` is replaced by `WTCLAUDE_SESSION_DIR` +
  `WTCLAUDE_BACKEND`.
- **Profile files are owned by a session scratch dir with an RAII guard at
  launch.** The PreToolUse hook is a separate short-lived process that exits
  before Claude runs the command, so a guard in the hook would delete the
  profile before `sandbox-exec` could open it. Each call writes a uniquely
  named file into the session dir, because Claude issues Bash calls in
  parallel and a single rewritten path would be truncated mid-read. The dir
  carries an `owner.lock` held open for the session's lifetime, so a later
  launch sweeps orphaned dirs (SIGKILLed sessions) without deleting a live
  session's profile — which judging staleness by age alone would eventually
  do to any long-running session.
- **`sudo` cannot work under Landlock.** The kernel requires `NO_NEW_PRIVS`
  before an unprivileged process may restrict itself, so privilege escalation
  is impossible by construction. The Linux system-prompt notice says so, and
  the hook denies `sudo` in command position with that explanation.
- **Landlock ABI 2 (kernel 5.19) is the floor.** ABI 1 lacks the `REFER`
  right, and the kernel then denies all cross-directory renames outright,
  which breaks git, cargo and npm. ABI 2 warns once that `truncate(2)` on a
  path is unrestricted; ABI 3+ (kernel 6.2) closes that.
- **Tests that apply Landlock must spawn a subprocess.** `restrict_self` is
  irreversible for the process and `cargo test` runs tests as threads in one
  binary.
