# Issue: Inject --resume command into zsh history on exit

## Desired behaviour

After a wtclaude session ends, pressing up-arrow in the shell should immediately
offer `wtclaude --resume <session-id> <branch-name>` so the user can resume
without looking up the session ID manually.

## Mechanism

zsh's `print -s <string>` injects a line into the current shell's in-memory
history. A Rust binary cannot call it directly (it cannot touch the parent
shell's memory), so the feature requires two parts:

1. **Binary side**: after `claude` exits, resolve the session ID of the session
   that just ran and write it to a temp file keyed by `PPID`:
   ```
   /tmp/wtclaude-last-session-<PPID>
   ```
   The session ID can be found by scanning `~/.claude/projects/` for the
   most-recently-modified session entry — the same data the `sessions`
   subcommand already reads (`src/sessions.rs`).

2. **Shell wrapper side**: replace the `wtclaude` binary call with a zsh
   function in the user's rc file (or in the completions script):
   ```zsh
   wtclaude() {
     command wtclaude "$@"
     local _wt_session _wt_file="/tmp/wtclaude-last-session-$$"
     _wt_session="$(cat "$_wt_file" 2>/dev/null)"
     rm -f "$_wt_file"
     [[ -n $_wt_session ]] && print -s "wtclaude --resume $_wt_session ${@[-1]}"
   }
   ```
   `$$` in the shell wrapper is the shell's PID, which equals `PPID` from the
   binary's perspective.

## Implementation notes

- **Session ID resolution**: after `cmd.status()` returns in `launch.rs`,
  call a helper that finds the newest session under
  `~/.claude/projects/<encoded-worktree-path>/` by mtime. The `sessions`
  subcommand already has this logic — extract it into a shared function.
- **PPID**: available in Rust via `std::os::unix::process` or simply
  `std::process::id()` won't work — use `nix::unistd::getppid()` or read
  `/proc/self/status` on Linux; on macOS `libc::getppid()` works, or just
  shell out to `echo $PPID` if avoiding a new dependency is preferred.
  Alternatively, pass `PPID` in from the shell wrapper as an env var to
  avoid the dependency entirely:
  ```zsh
  wtclaude() {
    WTCLAUDE_PPID=$$ command wtclaude "$@"
    ...
  }
  ```
- **Temp file cleanup**: the shell wrapper deletes the file after reading.
  The binary should also clean it up on abnormal exit (e.g. via a Drop guard)
  to avoid stale files accumulating.
- **Collision risk**: if two wtclaude instances exit simultaneously from the
  same shell PID (unlikely but possible with subshells), the last writer wins.
  Acceptable for now.
- **Wrong-argument guard**: `${@[-1]}` gives the last positional argument,
  which is the branch name for normal invocations. If the user ran
  `wtclaude --resume OLD_ID branch`, the injected command would be
  `wtclaude --resume NEW_ID branch`, which is correct. If they passed flags
  after the branch name (e.g. an initial prompt), the injected command will
  be missing those — acceptable since `--resume` doesn't need a prompt.
- **Shell wrapper distribution**: the wrapper could be emitted by
  `wtclaude --completions zsh` alongside the existing completion function, so
  users get it automatically by sourcing the completions output.

## Version note

Crate is at `v0.0.11` on `main` after PR #19. Per `CLAUDE.md`, bump patch
once per branch.

## Suggested skills

- `/verify` — confirm up-arrow after exit shows the resume command
- `/self-review` — pre-PR check
