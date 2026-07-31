# Add a subcommand to run sandboxed in the current directory/branch, without creating a worktree

## Context

Today `wtclaude <name>` always creates (or reuses) a git worktree at
`.claude/worktrees/<name>` before launching Claude Code, and the sandbox
write-boundary is scoped to that worktree directory. The user wants a mode
that skips worktree creation entirely — running Claude directly in the
current working directory, on whatever branch is currently checked out —
while still applying the same sandbox-exec write-restriction machinery
(PreToolUse hook, SBPL policy, Bash command rewriting) that worktree mode
uses today.

This is a feature request for `wtclaude` itself (the CLI in this repo), not
an issue in a project `wtclaude` manages.

## Relevant files

- `src/launch.rs` — main worktree-creation + sandbox-launch flow (both the
  interactive `run` path and the headless variant). A new "in-place" path
  would need to parallel this while skipping the `git worktree add` step.
- `src/config.rs` — `Mode` / `Config` structs backing `wtclaude.yml` modes
  (`default-mode`, `modes.<name>.claude-flags`). Could be the extension
  point, or this could instead be a new top-level subcommand alongside the
  existing `Hook`, `Sessions`, `Worktrees`, `Modes`, `Headless`,
  `SessionWorktree` variants in `src/main.rs`.
- `src/hook.rs` — the PreToolUse hook that enforces the write boundary;
  currently scoped to the worktree path, so it needs a boundary concept for
  "current directory" or "repo root" when no worktree exists.
- `README.md` — documents the current worktree-based flow and `--mode`
  flag; will need updating once this lands.

## Next steps

- Decide whether this is a new CLI subcommand (e.g. `wtclaude here`) or a
  `--mode`/flag variant of the existing launch path.
- Determine what the sandbox write-boundary should be when there's no
  worktree: current directory, repo root, or something configurable.
- Clarify interaction with the existing worktree-removal prompt at session
  exit (likely N/A here, since no worktree is created) and with `--resume`.
