# wtclaude: Write/Edit/NotebookEdit PreToolUse hook ignores wtclaude.yml's `allowlist`

## Context

`wtclaude.yml`'s `allowlist` includes `~/.claude`, with a comment explaining
it's there because it's "Claude Code's own data directory (session state,
settings)". In practice, though, a Claude Code session running under
wtclaude still gets denied when it tries to write there via the `Write`
tool (e.g. `~/.claude/projects/<project>/memory/*.md`, part of Claude's
auto-memory feature), with:

```
SANDBOX VIOLATION: '<path>' is outside the allowed worktree. All file
writes must stay within: <worktree>
```

The same path is reachable via the `Bash` tool (e.g. `echo test >
~/.claude/...`), which confirmed the allowlist *is* honored somewhere —
just not for the Write/Edit/NotebookEdit path.

This was discovered while trying to save Claude auto-memory files (under
`~/.claude/projects/<project>/memory/`) and separately while trying to
write nqaf fork/prompt files under `~/Documents/github.com/earlye/nqaf`
(not itself allowlisted, so that one's a separate, valid restriction —
the `~/.claude` case is the actual bug).

## Root cause

Two independent enforcement paths exist, and only one of them reads the
allowlist:

- **Bash tool**: commands get wrapped in a generated `sandbox-exec`
  (Seatbelt) profile that *does* fold in `user_config.allowlist`
  (`src/launch.rs:681`).
- **Write / Edit / NotebookEdit tools**: handled by a separate
  `PreToolUse` hook that never consults the allowlist at all:
  - `src/hook.rs:61-62` — for non-Bash tools, calls
    `is_within_sandbox(&path_str, &payload.cwd, &sandbox)`.
  - `src/hook.rs:272-283` (`is_within_sandbox`) — checks the candidate
    path against a single `sandbox` string only; no allowlist is
    consulted anywhere in this function.
  - `sandbox` comes from the `WTCLAUDE_SANDBOX` env var, which
    `src/launch.rs:189` sets to exactly one path (the canonicalized
    worktree dir) — it is never a list.

Net effect: the yml's intent ("`~/.claude` should be writable") is
correctly implemented for the Bash path but was never wired into the
Write/Edit/NotebookEdit hook, so that hook denies everything outside the
single worktree path regardless of what's in `allowlist`.

## Relevant files

- `src/hook.rs:61-62` — non-Bash tool dispatch, calls `is_within_sandbox`
  with only `sandbox`, no allowlist.
- `src/hook.rs:272-283` — `is_within_sandbox`, the actual path check;
  needs to also check allowlist entries.
- `src/launch.rs:189` — sets `WTCLAUDE_SANDBOX` to a single canonicalized
  worktree path.
- `src/launch.rs:681` — where `user_config.allowlist` is currently
  consumed (Bash/Seatbelt profile generation only).

## Reproduction steps

1. Ensure `~/.claude` (or any allowlisted path) is listed in
   `wtclaude.yml`'s `allowlist`.
2. Start a wtclaude-sandboxed Claude Code session in some worktree.
3. Use the `Write` tool to write a file under that allowlisted path (e.g.
   `~/.claude/projects/<project>/memory/test.md`).
4. Observe: denied with "SANDBOX VIOLATION... outside the allowed
   worktree".
5. Contrast: `Bash: echo test > ~/.claude/projects/<project>/memory/test.md`
   succeeds from the same session.

## Root cause hypothesis

Confirmed via source read (not just hypothesis) — see Root cause above.

## Next steps

Thread `user_config.allowlist` into the hook binary as well (e.g. have it
load `wtclaude.yml` directly itself, or pass the allowlist through
another env var alongside `WTCLAUDE_SANDBOX`), and extend
`is_within_sandbox` to permit a path if it matches either the worktree
sandbox path or any allowlist entry — not just the former.
