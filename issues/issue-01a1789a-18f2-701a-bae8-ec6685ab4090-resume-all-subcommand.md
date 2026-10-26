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
4. **Launch each window through `src window`**, running
   `wtclaude --no-pull --resume <sid> <branch>`. wtclaude never runs
   `tmux` itself; see "Window launch" below.
   - `--no-pull`: wtclaude's pull is a bare `git pull` in the root
     checkout. It moves the root checkout, and aborts when the branch has
     no upstream.
   - `--resume` still needs the branch.
   - Don't pass `--mode`.
   - A worktree with no transcript gets a fresh session with **no
     prompt**: `wtclaude --no-pull <branch>`. (Decision, grill
     2026-10-26: the skill seeded "Run `pwd && git branch`…", which
     spends a turn per window for nothing the window name doesn't show.)
   - **Stale windows** (a window already named after the branch): pass
     on whatever `src window` reports about it. Open question, see below.
   - If `src window` exits non-zero, record its stderr under "failed"
     and continue with the next worktree. A wtclaude refusal shows up
     inside the window, not in the report.
5. **Report**, grouped by outcome:
   - resumed
   - started fresh
   - failed (including `src window` failures, e.g. a session that
     couldn't be created)
   - skipped live
   - skipped merged
   - stale windows
   - detached or mismatched worktrees
   - number of transcripts scanned, and any that were skipped

## Window launch: delegated to `src window`

Decision (grill 2026-10-26): the tmux side lives in `src`, not wtclaude.
`src` already owns the per-repo session layout (naming, `new -A -d`,
emacs/`$WORKSPACE` setup), and a future `/md-issue tmux` can call the
same command without wtclaude growing a public launcher subcommand.
Rejected: porting `launch-window.sh` into wtclaude (the earlier plan),
which would have duplicated src's session naming and needed a
`has-session` re-check because `src` exited 0 when `tmux new` failed.

Requested of the `src` session on 2026-10-26, and tracked there as
`~/Documents/github.com/earlye/src/issues/issue-01a178a9-f901-76da-ae75-459dd50d208a-src-window-subcommand.md`
(src `main` at 8437d6e). That issue also fixes the session target: tmux
turns `.` and `:` into `_` in session names, but src compared the raw
basename, so repos with a dot in their name were missed.

`src window <DIR> -n NAME -- ARGV…` ensures DIR's session exists
without switching or attaching, checks DIR exists, opens
`tmux new-window -d -P -F '#{window_id}' -t =SESSION: -n NAME -c DIR "<cmd>"`
with every ARGV word `%q`-quoted, prints the window id (with the
before/after diff fallback for an empty `-P` output), and exits
non-zero with a reason on stderr on any failure.

wtclaude's side:

- **DIR:** the **repo root**. Started inside a worktree, wtclaude would
  nest `.claude/worktrees/`.
- **NAME:** the branch.
- **ARGV:** `<startish> --history "wtclaude --no-pull <branch>" -- wtclaude --no-pull --resume <sid> <branch>`.
  - Resolve `startish` to an absolute path before launching. The window
    gets the tmux server's environment, which may not have
    `~/.cargo/bin`.
  - `wtclaude` stays a bare word, because it runs in the user's
    interactive shell after the rc file has set PATH and defined the
    wtclaude wrapper.
  - The wtclaude wrapper pushes the resume line on exit. In the window,
    up-arrow then gives the command first, then the `--history` entries
    from last to first.
- **Ordering:** this PR depends on `src window` being released and
  installed. With an older `src`, every launch fails with clap's
  "unrecognized subcommand" and lands under "failed".
- **Open question:** stale-window detection needs the session name,
  which now lives only in `src`. Proposed to src: warn on stderr when a
  window named NAME already exists, still open the new one, and don't
  kill it or type into it. wtclaude reports the warning. The alternative
  is to drop the stale-window report.

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
  whose mode lacks the payload dir in `path-prepend`, `src window`'s tmux
  call fails and its stderr carries the snap-confine error into the
  report. (wtclaude itself no longer runs tmux; see "Window launch".)
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
- `src/launch.rs:1360` — `TmuxWindowName`, the window renaming that keeps windows named after the branch
- `src/config.rs:81`, `src/launch.rs:119` — `path-prepend` and `apply_path_prepend`
- `src/hook.rs:166`, `:277` — snap guard / `snap_command` (context only; no tmux resolution check, see Sandbox)

## Grill Log

### 2026-10-26

- Q: When should the `/snap/bin/tmux` resolution check fire (always, or only inside a sandbox)? — A: Never. wtclaude doesn't check tmux; `resume-all` is assumed not to run sandboxed itself.
- Q: Should the ported window launcher be a public wtclaude subcommand, or an internal module? — A: Neither: delegate session creation and window launch to `src` (new `src window` subcommand, requested of the src session). wtclaude never runs tmux. `src --no-switch` has landed.
- Q: Should a worktree with no transcript be started with a seeded prompt? — A: No prompt; plain `wtclaude --no-pull <branch>`. Revisit if needed.
