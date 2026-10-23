# Replacing `.git` with a symlink may redirect the sandbox write plan

## Context

This is a possible sandbox escape. The
`earlye/6f7bc121bde6/zsh-history-resume-injection` session sent it at the
user's request. A reviewer found it while reading code for PR #41
(resume-history injection, now merged). The problem is older than that PR,
and the PR doesn't fix it.

**Status:** the reviewer found it by reading the code, with about 80%
confidence. **No one has reproduced it.** The first step of a fix is to
confirm it, in a throwaway repository rather than a real checkout.

**Impact, if real:** a sandboxed session gets write access to the whole home
directory (for example `~/.zshrc`, `~/.ssh/authorized_keys`, or
`~/.cargo/bin`), which defeats the sandbox.

## Suspected mechanism

1. In in-place mode the sandbox root is the repository root, so the session
   can write `.git` itself.
2. The session runs `mv .git .git.real && ln -s ~ .git`.
3. `sandbox::plan()` is rebuilt on every Bash call, by both the PreToolUse
   hook (`src/hook.rs`) and the Landlock wrapper
   (`src/sandbox/landlock.rs`, which builds its own plan before
   `restrict_self`). On the next call, `git rev-parse --git-dir
   --git-common-dir` inside `launch::git_dirs` fails because `~` isn't a
   repository, so `git_dirs` falls back to `repo_root/.git`.
4. `resolve()` calls `config::resolve_existing_prefix`, which canonicalizes
   that path *through the symlink* to `$HOME`. Landlock then grants write
   access to the whole home directory for that call.
5. Any literal plan entry inside a directory the session can write can
   probably be redirected the same way.
6. **Hiding it:** the session can put `.git` back before it exits. A
   background process started while the grant was active keeps the access.
   A check after the session ends, such as PR #41's `Plan::grants_write`
   check on the resume handoff file, only sees the restored state.

## What the code shows (read only, 2026-10-23)

- `src/launch.rs:1473`, `git_dirs`: confirmed to fall back to
  `vec![repo_root.join(".git")]` whenever `git rev-parse` fails or can't be
  started. When rev-parse succeeds, it uses the paths git prints, made
  absolute by joining to `repo_root` and not canonicalized at that point.
- `src/sandbox/mod.rs:345`, `resolve`: confirmed to delegate to
  `config::resolve_existing_prefix`, which canonicalizes, and that follows
  symlinks.

## Related cases to check (not confirmed)

- **Path mode, not just in-place mode:** `wtclaude path <repo-root>` (which
  this reporting session runs under) also has sandbox root = repository
  root, with `repo_root` set, so it should be exposed the same way.
- **Worktree mode:** in a linked worktree, `<worktree>/.git` is a
  `gitdir: …` pointer *file* inside the sandbox root. A session could
  rewrite it to point at a fake git directory it builds inside the
  sandbox, whose `commondir` file names an arbitrary directory such as
  `$HOME`. If `git rev-parse --git-common-dir` returns that path without
  checking it, rev-parse *succeeds* and the plan includes the arbitrary
  directory, with no fallback or symlink involved. Whether git checks that
  `commondir` is a real repository is the thing to test.

## Suggested fixes

- **Fix at launch (reviewer's first suggestion):** compute `git_dirs` and
  resolve every literal plan entry once, when the launcher starts (before
  the session can touch anything). Pass those results to the hook and the
  wrapper, for example through `WTCLAUDE_SESSION_DIR`, which is outside the
  sandbox root. Then stop running `git rev-parse` per call.
  - **Tradeoff:** the per-call rebuild exists so that cache directories
    created mid-session can become writable, because Landlock needs the path
    to already exist (see the comment on `plan`). Only the paths that
    depend on the repository need fixing at launch. Cache directories
    outside the sandbox root can't be redirected by the session the same
    way, unless their parent is something the session can write.
- **Reject changed targets (reviewer's second suggestion):** refuse a plan
  entry whose resolved target is different from what it was at launch, or
  which resolves outside its expected parent.
- **Defence in depth:** refuse any git dir that resolves to `$HOME`, `/`, or
  an ancestor of the sandbox root, like `launch::is_sandbox_nullifying_root`
  already does for the sandbox root.

## Reproduction (for the fix session; use a scratch repository)

1. Start an in-place or path-mode session on a throwaway repository.
2. In one Bash call, run `mv .git .git.real && ln -s ~ .git`.
3. In a later Bash call, try `touch ~/canary`. If the file is created, the
   bug is confirmed.
4. Run `wtclaude sandbox --describe` (or `--show-policy` at launch) before
   and after the swap to compare the plans.
5. Clean up: `rm ~/canary; rm .git && mv .git.real .git`.

Don't run this in the wtclaude repository's own root checkout.

## Relevant files

- `src/launch.rs:1473`: `git_dirs` and its `repo_root/.git` fallback
- `src/sandbox/mod.rs:225`: `plan`, rebuilt per call and including
  `git_dirs(repo_root)`
- `src/sandbox/mod.rs:345`: `resolve` → `config::resolve_existing_prefix`
- `src/hook.rs:186`, `src/sandbox/landlock.rs:275`: the per-call `plan`
  callers
- `src/launch.rs:259`: how in-place mode is detected
