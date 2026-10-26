# `wtclaude resume`: resume only the current repository's worktrees

## Context

Early asked for a `resume` subcommand (2026-10-26). It does what
`wtclaude resume-all` does, but only for the worktrees of the repository
you run it in. `resume-all` shipped in v0.1.60 (`src/resume.rs`). It
reads every transcript under `projects_dir()` and resumes dead worktrees
in every repo. The same pipeline should serve both commands: discovery,
the porcelain check, the live, merged and gone filters, the per-repo tmux
session, the launch, and the report. `resume` keeps only one repo.

`--dry-run` behaves the same as on `resume-all`. That is an inference;
the request didn't mention it.

## Design notes

- **"Current repository" must mean the main repo root, not the worktree
  you're standing in.** `git rev-parse --show-toplevel` run inside
  `.claude/worktrees/<dir>` prints the worktree's path. Use the parent of
  `git rev-parse --path-format=absolute --git-common-dir`, or something
  equivalent. Then canonicalize it, because discovery canonicalizes each
  repo root (comment at `src/resume.rs:624`). Comparing the two ensures
  "current repository" picks out one repo, not several.
- **Filtering:** the simplest place is after `discover`, before `sweep`:
  drop every `Session` whose repo root isn't the current one.
  - An optional shortcut is to read only the project dirs whose name
    starts with `path_to_slug(repo)--claude-worktrees-`.
  - That shortcut can't replace the root comparison. The slug is lossy
    (`/` and `.` → `-`), so for example the repos `a.b` and `a-b` share a
    prefix.
- **Collision with the bare launch form:**
  - `main.rs` sends any first token that matches a subcommand name to the
    subcommand tree (`src/main.rs:79`).
  - A new `resume` subcommand therefore stops `wtclaude resume` from
    opening a worktree or branch named `resume`.
  - `resume-all` already does this for a branch named `resume-all`, but
    `resume` is a far likelier branch name.
  - Decide whether that's acceptable. The other option is a flag on
    `resume-all` (e.g. `--here`).
  - The existing `--resume <id>` launch flag is unaffected, because a
    flag is never the first positional token. A user could still mix up
    the two names.
- **Outside a git repo**, `resume` should fail with a clear message
  rather than fall back to `resume-all`.
- **Reuse `resume-all`'s abort rules.** If `claude agents --json` fails,
  abort, same as `resume-all`. The same holds for a broken tmux
  preflight, and for tmux resolving into `/snap/bin` under the sandbox.
  For that last case, see the resolved issue
  `issues/resolved/resolved-01a1789a-18f2-701a-bae8-ec6685ab4090-resume-all-subcommand.md`.

## Relevant files

- `src/resume.rs:19` — `ResumeAllArgs`. A `ResumeArgs`, or a shared args struct, goes next to it
- `src/resume.rs:30` — `run()`: resolve tools, discover, sweep, report. This is where the repo filter fits
- `src/resume.rs:298` — `discover()`; `:217` `split_worktree_cwd` derives each session's repo root
- `src/resume.rs:601` — `sweep()`, which groups sessions by repo at `:624`
- `src/main.rs:41`, `:132` — the `ResumeAll` variant and its dispatch; add `Resume` alongside
- `src/main.rs:70`–`:81` — subcommand-keyword routing that causes the branch-name collision
- `src/sessions.rs` — `projects_dir()` / `path_to_slug()` for the optional prefix shortcut
- `tests/dispatch.rs` — dispatch tests, updated when `resume-all` was added
- `README.md:271` — `resume-all` docs; document `resume` here
