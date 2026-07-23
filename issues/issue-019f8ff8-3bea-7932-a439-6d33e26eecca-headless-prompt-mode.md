# Headless (`-p`-style) mode for wtclaude

## Context

User wants a headless mode analogous to Claude Code's `-p` flag: give
wtclaude a prompt, it drives Claude to execute that prompt to completion
(or failure), and then reports the result back to the caller
non-interactively.

Unlike wtclaude's current normal invocation, this mode does not create a
git branch or worktree at all — but it does still sandbox the run.
Design resolved via grill (see Grill Log for full reasoning):

- **New subcommand**: `wtclaude headless <PROMPT...>`, alongside `hook`,
  `sessions`, `worktrees`, etc. in `main.rs` — not a flag on the existing
  worktree-launching invocation.
- **No worktree, ever.** Sandbox target is the invoking **cwd**, not
  `repo_root()` — this matters for monorepos where multiple headless
  agents may run concurrently in different subdirectories and must not
  be able to write into each other's areas. `repo_root()` (via `git
  rev-parse --show-toplevel`) is still resolved separately, for `.git`
  access and `update_trust` — just not as the sandbox boundary.
- **Prompt input**: trailing positional args (joined by space, same
  convention as today's `Args.prompt`), falling back to stdin when no
  positional prompt is given (mirrors `claude -p`). `--resume
  SESSION_ID` carries over unchanged.
- **Output**: pure passthrough. Headless mode always appends `--print`
  to the `claude` invocation, runs it via the same `Command::status()`
  (inherited stdio) as today, and returns `claude`'s own exit code
  as-is. wtclaude does not parse or wrap the result — a caller wanting
  `--output-format json` etc. just passes it through.
- **Interactive bits removed/bypassed**: `offer_git_init`'s stdin
  prompt is replaced with a hard failure if `repo_root()` fails
  (expected precondition: repo already exists, e.g. just cloned by
  whatever launched wtclaude headless) — no prompting, since stdin may
  carry the prompt itself. `--show-policy` keeps printing the policy but
  drops the "press enter" pause. `post_exit_menu` is simply never called
  (no worktree lifecycle to manage). `TmuxWindowName::rename` is not
  called — tmux window naming is the caller's concern in this mode.
- **Permission mode**: no special-casing. Headless mode accepts
  `--mode` and falls back to the config's `default-mode` exactly like
  the interactive path (currently `safe`). The sandbox is what bounds
  the risk regardless of mode; a user who wants headless runs to
  default to permissive can set their own `default-mode: dangerous` in
  their user config — that's a per-user choice, not something wtclaude
  should silently do for everyone.

## Relevant files

- `src/main.rs:61` — flat subcommand dispatch (`hook`, `sessions`,
  `worktrees`, `modes`, `session-worktree`); a `headless` subcommand
  arm belongs here, separate from the `launch::parse_args`/`launch::run`
  fallthrough.
- `src/launch.rs:20` (`struct Args`) / `src/launch.rs:30`
  (`parse_args`) — existing worktree-flow arg parsing; headless mode
  needs its own `Args`-equivalent (prompt, resume, mode, show-policy)
  without a required `WORKTREE_NAME` positional.
- `src/launch.rs:97` (`run`) — for reference only; headless mode's
  entry point reuses `generate_sbpl_policy` (`src/launch.rs:640`),
  `write_sbpl_policy` (`src/launch.rs:729`), `write_hook_settings`
  (`src/launch.rs:736`), `repo_root()` (`src/launch.rs:279`), and
  `update_trust` (`src/launch.rs:612`) directly, without going through
  `ensure_worktree`, `offer_git_init`, `post_exit_menu`, or
  `TmuxWindowName`.
- `src/config.rs` / `default_config.yml` — existing `--mode` /
  `default-mode` mechanism, reused unchanged for permission handling.

## Next steps

- Implement the `headless` subcommand: new arg parsing (prompt via
  positional-or-stdin, `--resume`, `--mode`, `--show-policy`), sandbox
  setup against cwd (not `repo_root()`), hard failure on non-git-repo
  (no `offer_git_init` prompt), `--print` always appended to the
  underlying `claude` invocation, exit code passed through unchanged.
- Note (not blocking, flagged during design): concurrent headless
  agents in the same repo still share one `.git` — sandbox isolation is
  per-cwd for file writes, but git-level races (e.g. simultaneous
  commits) between concurrent agents are not addressed by this design
  and are out of scope for this issue.

## Grill Log

### 2026-07-23

- Q: New subcommand (e.g. `wtclaude headless <PROMPT>`) vs. a `-p`/`--print`
  flag bolted onto the existing default (worktree-launching) invocation? —
  A: New subcommand. Keeps `launch.rs`'s worktree-required assumptions
  (`WORKTREE_NAME` as a required positional in `parse_args`) untouched, and
  matches the existing flat-dispatch pattern in `main.rs` (`hook`,
  `sessions`, `worktrees`, etc. are already single-word subcommands with
  their own entry points).
- Q: Sandbox scope when there's no worktree — the whole `repo_root()`
  (generalizing `in_place`), or just the invoking cwd? — A: The starting
  (invoking) directory, not `repo_root`. Reason: in a monorepo, multiple
  headless agents may run concurrently in different subdirectories: if
  they all sandbox against the shared `repo_root`, they can each write
  anywhere in the repo and stomp on each other's areas. Note:
  `generate_sbpl_policy` (`src/launch.rs:640`) already allow-lists
  `repo_root.join(".git")` as a path independent of `sandbox`, so scoping
  the writable sandbox to cwd still leaves git commands (commit, etc.)
  working — `repo_root` is still resolved via `git rev-parse
  --show-toplevel` for that purpose and for `update_trust`, it's just no
  longer the sandbox boundary itself.
- Q: How is the prompt supplied, and does `--resume` carry over? — A: Yes
  to both — keep the existing trailing-positional-prompt convention
  (`wtclaude headless <PROMPT...>`, same join-by-space style as
  `Args.prompt` today), additionally fall back to reading the prompt from
  stdin when no positional prompt is given (mirroring `claude -p` itself),
  and carry `--resume SESSION_ID` over unchanged so a headless run can
  continue a prior session.
- Q: What does "reports the result back to the caller" mean — pure
  passthrough of `claude`'s own stdout/exit code, or should wtclaude parse
  and wrap the result itself? — A: Pure passthrough, no wrapping. Headless
  mode always appends `--print` to the underlying `claude` invocation,
  runs it via the same `Command::status()` (inherited stdio) as today, and
  returns `claude`'s own exit code as wtclaude's exit code. wtclaude does
  not parse or envelope the result; a caller wanting structured output
  (e.g. `--output-format json`) passes that through like any other
  `claude` flag.
- Q: How to handle the currently-interactive bits — `offer_git_init`'s
  stdin prompt, `--show-policy`'s "press enter" pause, `post_exit_menu`,
  `TmuxWindowName::rename`? — A: `offer_git_init` — do not auto-run
  `git init` either; just bail loudly if `repo_root()` fails. Target
  scenario for headless mode is "repo was just cloned, and you're being
  launched by something else" — the repo is expected to already exist, so
  there's no interactive-vs-auto-init tradeoff to make, it's simply an
  error case. `--show-policy` — keep it, drop the pause (agreed).
  `post_exit_menu` — falls out naturally, never called for headless
  (agreed). `TmuxWindowName::rename` — do not call it; tmux window naming
  is the caller's concern in this mode, not wtclaude's.
- Q: With nobody present to approve tool-use prompts, how does headless
  mode avoid hanging/refusing under `--mode safe`? Should it default to a
  permissive mode (e.g. `dangerous`) instead? — A: No special-casing —
  headless mode uses the existing `--mode`/config mechanism completely
  unchanged, same default (`default-mode` from config, currently `safe`)
  as the interactive path. The sandbox bounds the risk regardless of
  mode. Anyone wanting headless runs to default to permissive can already
  set their own `default-mode: dangerous` in their user config — that's a
  per-user choice, not something wtclaude should silently do for
  everyone.
