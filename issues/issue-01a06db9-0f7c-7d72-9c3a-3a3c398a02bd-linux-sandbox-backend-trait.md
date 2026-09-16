# Linux sandbox support via a Backend trait

## Context

`wtclaude`'s write sandbox is hardcoded to macOS Seatbelt — a generated SBPL
profile plus `/usr/bin/sandbox-exec` per Bash call — so the tool does not work
on Linux at all. The goal is to run hands-off sessions (up to
`--dangerously-skip-permissions`) on a Linux box with the same containment
guarantee.

The design was grilled to completion before any implementation. Rather than
restate it, see:

- [`CONTEXT.md`](../CONTEXT.md) — vocabulary (**Plan**, **Backend**,
  **Profile**, **Sandbox root**, **Sandbox notice**, **Atomic-write dance**)
  and the ranked threat model
- [`docs/adr/0001-sandbox-backends-behind-a-trait.md`](../docs/adr/0001-sandbox-backends-behind-a-trait.md)
  — the trait, Landlock as the Linux backend, and the consequences that drove
  the shape (per-call Plan re-derivation, profile lifetime, `sudo`, ABI floor)
- [`docs/adr/0002-glob-allowlist-entries-are-seatbelt-only.md`](../docs/adr/0002-glob-allowlist-entries-are-seatbelt-only.md)
  — why `~/.claude.json*` cannot work on Linux

## Findings from the grill

Verified on Amazon Linux 2023, kernel 6.18, Landlock ABI 7:

- **Landlock unions overlapping rules.** A read-only rule on a subdirectory of
  a fully-granted directory does *not* reduce access, so "writable directory
  except X" and glob patterns are both inexpressible. This is what kills the
  `~/.claude.json*` **atomic-write dance** on Linux: replacing a file needs
  create+unlink on its parent, and here that parent is `$HOME`.
- **`/dev/null` must be granted explicitly** or almost nothing works — `git`,
  `mktemp` and `stty` all fail with `Permission denied` on `/dev/null`. macOS
  has an explicit `(allow file-write* (literal "/dev/null"))` rule with no
  Linux counterpart.
- **Signed commits need `~/.gnupg` writable.** `commit.gpgsign` is `true`
  globally, and gpg takes a dotlock (`.#lk0x…` → `pubring.kbx.lock`) before
  signing. `strace` confirms it writes *nothing* to `trustdb.gpg` — mtime and
  size are unchanged — so the requirement is create+unlink in the directory,
  not write on any file. `gpg --lock-never` signs fine with the keyring
  read-only; granting the directory was chosen anyway so key import and
  trustdb checks keep working. This gap exists on macOS too.
- **`sudo` cannot work under Landlock.** The kernel requires `NO_NEW_PRIVS`
  before an unprivileged process may restrict itself:
  `sudo: The "no new privileges" flag is set, which prevents sudo from running as root.`
- **Heredocs are fine on Linux.** The exact shape that breaks Apple's bash 3.2
  (`$(cat <<'EOF' … EOF)` with an embedded apostrophe) parses correctly under
  Linux bash, so the ban is Seatbelt-only.
- **`CLAUDE_CONFIG_DIR` relocates `.claude.json`** and loses auth, which a
  copied `.credentials.json` restores. Recorded as the rejected alternative in
  ADR-0002.
- **Re-deriving the Plan is cheap** — `git rev-parse --git-dir
  --git-common-dir` costs ~0.9ms, negligible against the hook's own process
  spawn.

## Relevant files

- `src/launch.rs:1130` — `generate_sbpl_policy`, the Plan derivation and SBPL
  rendering to be split apart
- `src/launch.rs:1236` — `write_sbpl_policy` and the `TempFile` RAII guard at
  `src/launch.rs:114`, to become the session scratch dir
- `src/launch.rs:1042` — `resolve`, `glob_to_regex`, `sbpl_string_escape`,
  `partition_allowlist`
- `src/launch.rs:177` — `sandbox_warning_common`, to become a per-Backend
  **Sandbox notice**
- `src/hook.rs:103` — `wrap_bash_in_sandbox`, the `sandbox-exec` rewrite, and
  `HEREDOC_DENY_MESSAGE` (the precedent for the new `sudo` reminder)
- `src/hook.rs:159` — `regenerate_sbpl_policy`, superseded by per-call Plan
  re-derivation
- `src/config.rs:47` — `validate_allowlist` and `resolve_glob_prefix`
- `src/main.rs:17` — `Commands`, gaining a Linux-only `sandbox` subcommand;
  the zsh completion block at `src/main.rs:220` mentions
  `--test-sbpl-breakage`
- `tests/path.rs:467` — asserts SBPL text directly; needs per-Backend
  expectations

## Next steps

Scope agreed with the user: implement directly from the ADRs, no initiative.

1. Extract `Plan` derivation and the `Backend` trait; move Seatbelt behind it
   with no behavior change.
2. Add the Landlock backend plus the `wtclaude sandbox` subcommand and
   `sandbox-backend` config key with auto-detection.
3. Do the rename: `WTCLAUDE_SBPL` → `WTCLAUDE_SESSION_DIR` +
   `WTCLAUDE_BACKEND`, `--test-sbpl-breakage` → `--test-sandbox-breakage`.
4. Session scratch dir under `~/.local/state/wtclaude/<pid>/` — deliberately
   outside the Plan, so a session cannot rewrite its own **Profile**.
5. Per-Backend **Sandbox notice**; `sudo` denial in command position only
   (also `doas`/`pkexec`/`su`), so `git commit -m "document sudo"` is not
   caught.
6. Tests: cross-platform Plan tests, cfg-gated render tests, and subprocess
   enforcement tests on Linux (`restrict_self` is irreversible, and
   `cargo test` runs tests as threads in one binary).
7. Update `README.md` — it currently says macOS-only and describes the goal as
   preventing *accidental* modification without noting the injection
   motivation.

## Deferred

- The **Profile** was previously written to `/tmp`, which is itself in the
  Plan, and its path was exported as `$WTCLAUDE_SBPL` — so a session could
  rewrite its own profile. Step 4 above fixes this incidentally. No other
  self-protection work is in scope; the threat model ranks accidental writes
  first.
- bubblewrap as a second Linux Backend, if `sudo` inside sandboxed Bash ever
  becomes necessary.
