# Add a `wtclaude path` subcommand: interactive, sandboxed, no worktree

## Context

Original ask: a subcommand that runs sandboxed in the current directory
without creating a worktree. Turns out most of this already exists:

- `src/launch.rs:86-91` (added in #10, "Add user allowlist config and
  in-place (current-branch) mode") already skips `git worktree add` and
  sandboxes against `repo_root` when the `WORKTREE_NAME` argument matches
  the branch you're already on.
- `wtclaude headless` (`src/launch.rs:236-324`) always runs non-interactively
  against cwd, no worktree, no branch-name argument at all.

The actual gap, confirmed with the user: there's no ergonomic **interactive**
way to launch sandboxed against an arbitrary directory (not necessarily tied
to a git branch, or even to a git repo at all) without either retyping your
current branch name to trigger the existing name-match trick, or using
non-interactive `headless`.

Decision: add a new interactive subcommand, `wtclaude path`, that takes a
directory instead of a worktree/branch name.

## Design decisions (resolved via grill)

- **CLI shape**: `wtclaude path <DIRECTORY> [PROMPT]`. `DIRECTORY` is
  required (no implicit cwd default) — pass `.` explicitly to mean "here".
  `PROMPT` is optional, same seed-the-session behavior as the existing
  bare `wtclaude WORKTREE_NAME [PROMPT]` form.
- **Interactive, not headless**: opens a normal interactive claude session
  (like the main launch path), not `claude --print`. Supports the same
  flags as the existing `Args`: `--mode`, `--resume`, `--show-policy`,
  `--test-sbpl-breakage`. (`--no-pull` still applies when DIRECTORY is
  inside a repo; see below.)
- **Never creates a worktree.** This is the whole point — no
  `git worktree add`, and no post-exit worktree-removal menu (nothing to
  remove).
- **Sandbox write-boundary is `DIRECTORY` itself** (canonicalized), not the
  containing repo root — narrower and more predictable than today's
  branch-name-match in-place mode. Same `.git`/tmp/package-cache-dir
  allowlist entries as today's `generate_sbpl_policy`, when applicable (see
  below).
- **DIRECTORY does not need to be inside a git repository.** If it isn't:
  skip `git pull`, skip `.git`-dir allowlisting (nothing to allow), and skip
  the trust-dialog bypass keyed off `repo_root` (key it off `DIRECTORY`
  itself instead) — "just run claude" against that directory with a bare
  sandbox (DIRECTORY + tmp + package caches).
- **If DIRECTORY is inside a git repo**: still run `git pull` on `repo_root`
  (respecting `--no-pull`), same as the main launch path, and allowlist the
  repo's git dirs (`git_dirs(repo_root)`).
- **tmux window rename**: always happens (repo or not), named after
  `DIRECTORY`'s canonicalized basename — not the literal string typed (so
  `wtclaude path .` renames the window to the actual directory name, not
  `.`).
- **Sandbox notice text** (the `--append-system-prompt` message) needs new
  wording for this mode — it currently says "sandbox for branch '{name}'";
  `path` has no branch concept when DIRECTORY isn't a repo, so this should
  describe a directory sandbox instead.

## Relevant files

- `src/launch.rs` — `run()` (interactive launch, worktree creation,
  `in_place` logic at lines 86-91/99-110) and `run_headless()` are the two
  existing patterns to draw from; `run_path()` (or similar) sits between
  them — interactive like `run()`, cwd-scoped like `run_headless()`.
- `src/main.rs` — add a `Path { directory: PathBuf, prompt_parts: Vec<String>, ... }`
  variant to the `Commands` enum, alongside `Hook`, `Sessions`, `Worktrees`,
  `Modes`, `Headless`, `SessionWorktree`.
- `src/config.rs` — unaffected; `path` reuses the existing `Mode`/`Config`
  loading as-is.
- `src/hook.rs` — PreToolUse write-boundary enforcement; needs to work with
  a boundary that isn't a worktree path (should already generalize, since
  `WTCLAUDE_SANDBOX` env var is just a path today).
- `README.md` — document the new `wtclaude path` subcommand once it lands.

## Next steps

- Implement `run_path()` in `src/launch.rs`, factoring shared bits with
  `run()`/`run_headless()` (`write_hook_settings`, `write_sbpl_policy`,
  `update_trust`, `TmuxWindowName`) rather than duplicating them wholesale.
- Decide exact error behavior for a nonexistent DIRECTORY (not raised during
  the grill — straightforward "error out", but confirm no `git init`-style
  prompt is wanted here, unlike the main launch path's cwd fallback).
- Update `README.md` and zsh completions (`src/main.rs`'s
  `print_completions_zsh`) for the new subcommand.

## Grill Log

### 2026-08-03

- Q: Given `wtclaude <current-branch-name>` already triggers in-place mode,
  what's the actual gap? — A: Ergonomics only; the behavior is right, but
  retyping the current branch name to trigger it is unintuitive.
- Q: What should the explicit invocation look like (new subcommand vs `.`
  sentinel vs flag)? — A: Started with "special positional value `.`" on the
  existing launch path, then revised to a dedicated new subcommand named
  `path` taking a directory as its first argument instead of a worktree
  name.
- Q: `wtclaude path <DIRECTORY> <PROMPT>` — optional-with-cwd-default or
  required? — A: Required; `.` is the idiomatic value for "here", not an
  implicit default.
- Q: Interactive or headless? — A: Interactive — `headless` already covers
  the non-interactive cwd case; `path` fills the interactive gap.
- Q: Sandbox boundary — DIRECTORY itself, or the containing repo_root? —
  A: DIRECTORY itself (narrower, matches headless's existing model).
- Q: Must DIRECTORY be inside a git repo? — A: No; arbitrary non-repo
  directories are allowed. When there's no repo, skip git-pull, `.git`
  allowlisting, and repo_root-keyed trust update — just run claude.
- Q: Keep git-pull and tmux-rename when DIRECTORY *is* in a repo? — A: Yes,
  same as the main launch path; but skip git-pull specifically when not in
  a git directory (tmux-rename still happens either way).
- Q: What should tmux-rename and the trust-dialog bypass key off for a
  non-repo DIRECTORY? — A: DIRECTORY's canonicalized basename — not the
  literal string typed (so `.` resolves to the real directory name).
