# Provide an ssh-agent to the Claude session

## Context

The user asked for wtclaude to make sure its Claude session has a working
ssh-agent. The `origin` remote is `git@github.com:earlye/wtclaude.git`, so a
session without an agent can commit but can't push, and so can't open a PR
from that branch either.

## Observations (2026-10-23, Linux, landlock backend)

**A session launched from an interactive shell works.** This session has
`SSH_AUTH_SOCK=/home/earlye/.ssh/agent/s.TK9T4Qz9uz.agent.mCKCzJuLtr` (the
OpenSSH `~/.ssh/agent/` location), `ssh-add -l` lists the ED25519 key, and
`git push` succeeded from inside the sandbox. Landlock doesn't stop connecting
to a pathname socket outside the sandbox root.

**Sessions opened through `/md-issue-tmux` probably have no agent.** The tmux
server's environment has no `SSH_AUTH_SOCK`:

```
$ tmux show-environment -g SSH_AUTH_SOCK
unknown variable: SSH_AUTH_SOCK
$ tmux show-environment SSH_AUTH_SOCK
-SSH_AUTH_SOCK
```

`update-environment` is at the tmux default, which includes `SSH_AUTH_SOCK`.
A `-` entry means the client that last attached had it unset, so
`tmux new-window` starts its command without an agent. That's the path
`/md-issue-tmux` uses.

This might explain why the `3f7477eacbe8` fix session (window `@30`) made
two commits on `earlye/3f7477eacbe8/version-from-git-commit-count` but never
opened a PR. That's unconfirmed: nobody has looked at that session's
transcript.

**Agents are available on the machine even when the variable isn't set.**
- systemd user env: `SSH_AUTH_SOCK=/run/user/1000/gcr/ssh` (GNOME
  keyring/gcr)
- `/run/user/1000/openssh_agent` (the systemd `ssh-agent` user unit's
  socket)
- `~/.ssh/agent/s.*` (the one this session uses)

`agent_socket_dir()` (`src/sandbox/mod.rs:274`) already adds
`$XDG_RUNTIME_DIR` to the plan on Linux, so the two `/run/user/1000`
sockets would be reachable. On macOS, agent sockets are handled per machine
with `socket_allowlist`.

## Approaches

1. **Find and pass on an existing agent.** At launch, if `SSH_AUTH_SOCK` is
   unset or its socket doesn't accept connections, look for a live one:
   `systemctl --user show-environment`, then
   `$XDG_RUNTIME_DIR/openssh_agent`, `$XDG_RUNTIME_DIR/gcr/ssh`, and
   `~/.ssh/agent/s.*`. Set it on the `claude` command in `apply_env`
   (`src/launch.rs:69`). This is cheap and reuses keys that are already
   unlocked. The risk is choosing an agent the user didn't intend.
2. **Run a per-session `ssh-agent`.** Start one that lives as long as the
   session (its socket in `WTCLAUDE_SESSION_DIR`, killed during cleanup),
   and load keys into it. This keeps the agent separate, and could hold a
   key limited to just this session, such as a deploy key. But keys with
   passphrases need a prompt at launch, which doesn't work for headless or
   systemd runs.
3. **Configuration:** a setting in `wtclaude.yml`, possibly per mode, that
   either names the socket to use or lists keys for approach 2.

Whichever approach is chosen, `wtclaude` should print a warning when it
can't find a working agent and the repository's remote is SSH, rather than
leaving the session to find out when it pushes.

## Open questions

- **What prompted this request:** the tmux path, the systemd
  `wtclaude headless --mode review-pass` run (whose env includes the gcr
  socket), or something else. The answer decides whether approach 1 is
  enough.
- **Should an agent be passed on at all in some modes?** If one key can push
  anywhere, a prompt-injected session can use it. Approach 2, or a per-mode
  setting, would limit what a session can do with it.
- **Does gcr's ssh socket offer the keys needed** without an unlock prompt
  in a session with no attached user?

## Relevant files

- `src/sandbox/mod.rs:274`: `agent_socket_dir()`, which adds
  `$XDG_RUNTIME_DIR` to the plan on Linux
- `src/config.rs:27`: `socket_allowlist`, which handles macOS agent sockets
- `src/launch.rs:69`: `apply_env`, where session environment variables are
  set
- `~/.claude/skills/md-issue-tmux/SKILL.md`: launches sessions with
  `tmux new-window`, which inherits the tmux server's environment
