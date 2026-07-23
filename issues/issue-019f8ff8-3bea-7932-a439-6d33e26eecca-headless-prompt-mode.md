# Headless (`-p`-style) mode for wtclaude

## Context

User wants a headless mode analogous to Claude Code's `-p` flag: give
wtclaude a prompt, it drives Claude to execute that prompt to completion
(or failure), and then reports the result back to the caller
non-interactively.

Unlike wtclaude's current normal invocation, this mode does not
necessarily need to create a git branch or worktree — but it does still
need to sandbox the run. Today, sandboxing setup
(`generate_sbpl_policy` / `write_sbpl_policy` / `write_hook_settings`)
is wired up alongside worktree creation inside `launch::run`, so the
sandbox and the worktree/branch machinery are currently coupled and
will likely need to be pulled apart for this to work.

## Relevant files

- `src/main.rs:61` — CLI falls through to `launch::parse_args` /
  `launch::run` for the default (non-subcommand) invocation; a
  headless/`-p` mode would need a new entry point or flag here.
- `src/launch.rs:20` (`struct Args`) and `src/launch.rs:30`
  (`parse_args`) — current argument parsing; `WORKTREE_NAME` is a
  required positional today.
- `src/launch.rs:97` (`run`) — orchestrates worktree creation
  (`ensure_worktree`, `src/launch.rs:323`) and sandbox policy
  generation/hook wiring (`generate_sbpl_policy`, `src/launch.rs:640`;
  `write_sbpl_policy`, `src/launch.rs:729`; `write_hook_settings`,
  `src/launch.rs:736`) as one flow — these will need to be
  decoupled so sandboxing can apply without a worktree/branch.

## Next steps

- Design how sandbox setup can run independent of worktree creation
  (e.g. sandbox against the current working directory / repo root
  directly).
- Decide how the headless invocation is triggered (new subcommand vs.
  a `-p`/`--print`-style flag) and how the prompt and final
  result/exit status are passed in and reported back to the caller.

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
