# Name the Claude session after the branch or selected path

## Context

When wtclaude launches `claude`, the session should be named after what it was launched for:

- worktree mode (`wtclaude <branch>`): the branch name
- path mode (`wtclaude path <dir>`): the selected directory

Today wtclaude sets the tmux window title (see `directory_label` for path mode) but not the Claude session's own name, so sessions in `/resume` and similar lists can't be told apart by what they were working on.

The installed `claude --help` doesn't list a flag for setting a session name or title (checked 2026-10-22: no matches for `name`, `title`, `display`, or `rename` apart from tool and skill names). So the first step is to find out how a session name can be set at launch, e.g. a CLI flag, an env var, a settings key, or sending `/rename` at startup.

## Relevant files

- `src/launch.rs:150` — `run` (worktree mode); builds `Command::new("claude")` at `src/launch.rs:221`
- `src/launch.rs:298` — `run_headless`; `Command::new("claude")` at `src/launch.rs:341` (decide whether headless sessions get a name too)
- `src/launch.rs:431` — `run_path` (path mode); `Command::new("claude")` at `src/launch.rs:509`
- `src/launch.rs:589` — `directory_label`, which already derives the path-mode label from the canonicalized directory's basename and can be reused for the session name
- `src/launch.rs:596` — `sanitize_name`; note that branch names can contain `/`

## Open questions

- Which mechanism does Claude Code support for setting a session name at launch?
- With `--resume`, should wtclaude keep the resumed session's existing name or rename it?
