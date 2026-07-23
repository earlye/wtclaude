# wtclaude headless mode's sandbox policy doesn't resolve linked-worktree `.git` indirection

## Context

`pdd-orchestrate` (this repo, `crates/pdd-orchestrate`) invokes `wtclaude headless <PROMPT>` with cwd
set to a linked git worktree that `pdd-orchestrate` itself creates via `git worktree add` — not one
`wtclaude` creates. This is exactly the "launched by something else" scenario wtclaude headless mode's
own design (wtclaude issue `019f8ff8`, "Headless (-p-style) mode for wtclaude") describes as its target
use case.

When cwd is a linked worktree, `git rev-parse --show-toplevel` returns the worktree's own path, and
`.git` at that path is a *file* containing a `gitdir:` pointer (e.g. `gitdir:
/path/to/main-repo/.git/worktrees/<name>`), not a directory. `wtclaude`'s sandbox-policy generation
resolves `repo_root()` this way and allow-lists `repo_root.join(".git")` for write access — but for a
linked worktree that's just the small pointer file, not the real mutable git metadata directory. The
actual `FETCH_HEAD`, per-worktree index, `HEAD`, `MERGE_HEAD`, etc. for that worktree live in the *main*
repository's `.git/worktrees/<name>/` directory — a completely different path the current policy
generation never resolves to or allow-lists.

**Concrete failure observed**: a `wtclaude headless` session running inside such a linked worktree tried
to run `git fetch origin <branch>`, which needs to write `FETCH_HEAD` inside `.git/worktrees/<name>/`
(the shared metadata dir one level up from the worktree checkout). The sandbox denied the write.
Claude's session correctly diagnosed the cause, asked for guidance as its final message, and exited 0 —
a "normal" exit from headless mode's point of view, since there's no one there to answer. On the caller
side (`pdd-orchestrate`) this initially looked like a silent, hard-to-detect failure (exit 0 doesn't
distinguish "did the work" from "asked a question and stopped") until `pdd-orchestrate` added its own
`PROMPT_RESULT: DONE`/`BLOCKED` marker convention to compensate.

## Root cause hypothesis

Sandbox-policy generation for headless mode allow-lists `repo_root.join(".git")` assuming it's always a
real directory. For a linked worktree it's a pointer file instead, and the policy generator doesn't
follow the `gitdir:` indirection (something git itself already does internally) to find the real
metadata directory that needs to be writable.

## Suggested direction (not prescriptive)

When resolving `repo_root` for sandbox-policy purposes, follow the `.git` file's `gitdir:` pointer to
find the real metadata directory, and allow-list writes there (or at least the specific files needed:
`FETCH_HEAD`, `index`, `HEAD`, `MERGE_HEAD`, `ORIG_HEAD`) rather than assuming `repo_root/.git` is
always a real directory.

Not asking for headless mode to auto-detect or handle this in some other clever way — just flagging
that it doesn't currently account for its own explicitly-stated target use case (being invoked inside a
worktree the caller already created), and that git commands needing to write shared per-worktree
metadata (fetch chief among them) will hit this every time until it's addressed.

## Relevant files

- `~/Documents/github.com/earlye/wtclaude/src/launch.rs` — `write_sbpl_policy`/`generate_sbpl_policy`
  and `repo_root()`, the pre-headless-mode versions of this logic (for reference; headless mode's own
  policy generation is new code added alongside these).
- `crates/pdd-orchestrate/src/wtclaude.rs` (this repo) — the caller invoking `wtclaude headless` with
  cwd set to an already-existing linked worktree.
- `crates/pdd-orchestrate/src/git.rs` (this repo) — `ensure_worktree`, which creates the linked
  worktrees `wtclaude headless` gets invoked inside of.
