# `wtclaude resume-all [--dry-run]`: reopen dead worktree sessions in tmux

## Context

The `/resume-tmux` skill reopens every worktree under `.claude/worktrees/`
whose Claude session isn't running. Each one gets a tmux window that
resumes its newest session, with one tmux session per repo, the same
layout `src` uses. You run it after a power loss or reboot. The skill is
deterministic, so Early wants it moved into wtclaude as a subcommand. The
skill is deleted once this ships.

The spec is the skill itself, and every caveat in it came from a real
run. It exists only on the skills branch `earlye/a411e1ea53d2/md-issue-resume`:

- `~/Documents/github.com/earlye/skills/.claude/worktrees/earlye+a411e1ea53d2+md-issue-resume/skills/resume-tmux/SKILL.md`
- `~/Documents/github.com/earlye/skills/.claude/worktrees/earlye+a411e1ea53d2+md-issue-resume/skills/resume-tmux/launch-window.sh`

Requested by the skills-secretary session for Early, 2026-10-26.

## Behaviour

`--dry-run` prints the same report without creating tmux sessions or
windows.

1. **Discover worktrees from transcripts.**
   - Read `projects_dir()/*--claude-worktrees-*/*.jsonl`, top-level files
     only, which leaves out subagent transcripts.
   - Read the last 64 KiB, newest line first. If the tail has no usable
     line (one huge final line), read the whole file.
   - Take `cwd`, `gitBranch` and `sessionId` from that line. If
     `sessionId` is missing, use the file stem.
   - The repo root is `cwd` with `/.claude/worktrees/<dir>…` stripped.
   - Keep the newest session per worktree, and report the older session
     ids.
   - Don't use the name column of `sessions` / `session-worktree`.
     `path_to_slug` turns `/` and `.` into `-` (and the dir name's `+`
     as well), so the original names can't be recovered.
   - If no transcripts were scanned, the glob found nothing. Say so
     instead of reporting "nothing to resume".
2. **Take the branch from `git worktree list --porcelain`**, run in each
   repo, not from the transcript. wtclaude derives the worktree directory
   from the branch and runs `git worktree add` when that directory is
   missing. A transcript that ended on another branch or on `HEAD` would
   therefore create a stray worktree. Report any mismatch between the two.
   Report these worktrees and leave them alone:
   - detached worktrees (no `branch` line)
   - worktrees whose directory name isn't the branch with `/`→`+`. These
     can't be reopened, because `git worktree add` fails with "already
     checked out".
   - Worktrees that have no transcript get a fresh session. A repo with
     no transcripts at all is never discovered.
3. **Filters**, applied in this order:
   - **Gone:** not listed, `prunable`, or the directory is missing. Skip
     silently. This is the only silent skip.
   - **Live:** the path equals the `.cwd` of an entry in
     `claude agents --json`. Skip it. If that call fails, abort: with no
     live list, every running session looks dead and would get a second
     Claude. Don't infer liveness from tmux window names, because a
     window outlives its Claude.
   - **Merged:** `gh pr list --head <branch> --state merged`, run in the
     repo. Skip. If `gh` errors, treat the branch as unmerged and say so
     in the report.
   - Everything else is resumed. Never guess that a worktree is abandoned.
   - The skill's md-issue "resolved" filter (`issues/resolved/` lookup)
     is **dropped**, by Early's decision. It ties wtclaude to one repo's
     issue convention, and the merged-PR check covers most cases.
4. **One tmux session per repo.**
   - The name is the basename of the repo root with `.` and `:` → `_`
     (tmux's `session_check_name`).
   - Look it up with `has-session -t =name`. The `=` stops `skills` from
     matching `skills-old`.
   - If the session is missing, run `src dir <repo> --no-switch`, then
     re-check with `has-session`, because `src` exits 0 even when
     `tmux new` failed. If the re-check fails, defer the repo and say so.
   - Never use plain `src`. It `switch-client`s the user's view, or runs
     `tmux attach` and hangs.
   - Preflight with `tmux list-sessions`. "no server running" and
     "error connecting to … No such file or directory" are fine; any
     other error aborts. This is the only point where a broken tmux
     reports its own error, because the `has-session` checks discard
     stderr.
5. **Open each window detached**, running
   `wtclaude --no-pull --resume <sid> <branch>`.
   - `--no-pull`: wtclaude's pull is a bare `git pull` in the root
     checkout. It moves the root checkout, and aborts when the branch has
     no upstream.
   - `--resume` still needs the branch.
   - Don't pass `--mode`.
   - A worktree with no session gets a fresh session with a plain prompt
     (e.g. "Run `pwd && git branch`…"), not `! pwd && git branch`.
     Claude Code 2.1.310 (2026-10-25) may pass a `!` prompt to the model
     as text; that was never confirmed either way.
   - **Stale windows** (named after the branch): open the new window
     anyway, and report the stale one. Don't kill it or type into it.
   - If a launch fails, record the error and continue with the next
     worktree. A wtclaude refusal shows up inside the window, not in the
     report.
6. **Report**, grouped by outcome:
   - resumed
   - started fresh
   - failed
   - skipped live
   - skipped merged
   - stale windows
   - deferred repos
   - detached or mismatched worktrees
   - number of transcripts scanned, and any that were skipped

## Window launch: port `launch-window.sh` to Rust

Port the script's logic; don't shell out to it. In the skills repo,
skills-secretary is tracking a move of `/md-issue tmux` onto wtclaude for
its launch. This launcher should later be something both can call.

- **Target:** `tmux new-window -d -P -F '#{window_id}' -t =SESSION: -n NAME -c CWD "<cmd>"`.
- **CWD:** must be the **repo root**. Started inside a worktree, wtclaude
  would nest `.claude/worktrees/`. Check that CWD exists before
  launching. If `-c` names a missing directory, tmux silently falls back
  to `$HOME`.
- **startish by absolute path:** resolve `startish` before launching. The
  window gets the tmux server's environment, which may not have
  `~/.cargo/bin`. `wtclaude` stays a bare word, because it runs in the
  user's interactive shell after the rc file has set PATH and defined the
  wtclaude wrapper.
- **Quoting:** tmux runs the command as one string through its
  default-shell's `-c`. So `%q`-quote every word of
  `<startish> --history <h>… -- <command words…>`.
- **History:** pass `--history "wtclaude --no-pull <branch>"`. The
  wtclaude wrapper pushes the resume line on exit. In the window,
  up-arrow then gives the command first, then the `--history` entries
  from last to first.
- **Window id:** on 2026-10-23 the skill saw `-P -F '#{window_id}'`
  print nothing. In that case it used the single window id that wasn't
  there before the launch. On 2026-10-26 the same call printed `@11`, so
  that didn't reproduce. Keep the fallback.

## Sandbox: tmux works under Landlock via `path-prepend` (tested 2026-10-26)

The skill says (2026-10-22) that tmux "fails outright" inside a wtclaude
sandbox, because `/snap/bin/tmux` goes through snap-confine and
snap-confine refuses to run under NO_NEW_PRIVS. That only holds when the
snap launcher is the binary that runs. A test inside a Landlock sandbox,
`WTCLAUDE_MODE=dangerous`, gave:

```
$ which -a tmux
/snap/tmux/current/usr/local/bin/tmux
/snap/bin/tmux
$ tmux -V
tmux 3.7c
$ tmux list-sessions
skills: 3 windows (...) (attached)
src: 5 windows (...)
wtclaude: 2 windows (...)
$ tmux has-session -t =wtclaude; echo $?
0
$ tmux new-window -d -P -F '#{window_id}' -t '=wtclaude:' -n rtest-sandbox2 'sleep 5'
@11
$ tmux kill-window -t '=wtclaude:rtest-sandbox'     # rc=0
```

- It works because the user's `~/.config/wtclaude/wtclaude.yml` (mode
  `dangerous`) has `/snap/tmux/current/usr/local/bin` in `path-prepend`.
  The classic snap's payload runs without snap-confine.
- `/snap/bin/tmux` itself wasn't run. wtclaude's PreToolUse snap guard
  (`src/hook.rs:166`) blocked it, so this test produced no fresh
  snap-confine error text.
- Decision (grill 2026-10-26): wtclaude does **not** check how `tmux`
  resolves. `resume-all` is assumed to run from an unsandboxed shell
  (the post-reboot terminal), where `/snap/bin/tmux` works. An earlier
  draft suggested failing whenever `tmux` resolved into `/snap/bin`; that
  would refuse on the very machine this is for. Run inside a sandbox
  whose mode lacks the payload dir in `path-prepend`, the tmux preflight
  (`list-sessions`) fails and reports the snap-confine error itself.
- The tools the launch needed resolved without snap-confine:
  - `startish` and `wtclaude` → `~/.cargo/bin`
  - `claude` → `~/.local/bin`
  - `gh` → the `/snap/gh/current` payload, also via `path-prepend`

## Relevant files

- `src/main.rs:21` — `enum Commands`, where the new subcommand goes; dispatch at `src/main.rs:83`
- `src/main.rs:72` — subcommand keyword detection, derived from `enum Commands`; adding the variant is enough
- `src/sessions.rs:152` — `projects_dir()`, to reuse
- `src/sessions.rs:160` — `path_to_slug()`, the lossy slug; not usable for this
- `src/sessions.rs:18`, `:95`, `:127` — existing transcript-directory listing. Nothing parses the JSONL contents yet, so tail-reading `cwd`/`gitBranch`/`sessionId` is new code
- `src/launch.rs:202`, `:261`, `:1073` — `--no-pull` and `git_pull`
- `src/launch.rs:205`, `:644` — `--resume` and `add_session_args`
- `src/launch.rs:689`–`:744` — resume-line handoff to the shell wrapper (history)
- `src/launch.rs:1360` — `TmuxWindowName`, the window renaming the stale-window check relies on
- `src/config.rs:81`, `src/launch.rs:119` — `path-prepend` and `apply_path_prepend`
- `src/hook.rs:166`, `:277` — snap guard / `snap_command` (context only; no tmux resolution check, see Sandbox)

## Grill Log

### 2026-10-26

- Q: When should the `/snap/bin/tmux` resolution check fire (always, or only inside a sandbox)? — A: Never. wtclaude doesn't check tmux; `resume-all` is assumed not to run sandboxed itself.
