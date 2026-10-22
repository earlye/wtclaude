# wtclaude should not prevent running snap-installed tools (gh, just)

## Context

On Linux under the landlock backend, any snap-packaged command fails
inside the sandbox. On this machine `gh` and `just` are both snaps
(`/snap/bin/gh -> /usr/bin/snap`, `/snap/bin/just -> /usr/bin/snap`), so
neither works from a wtclaude session. That breaks PR workflows (`gh`)
and the project's task runner (`just`).

Observed in a `wtclaude path` session (`WTCLAUDE_BACKEND=landlock`):

```
$ gh --version
snap-confine has elevated permissions and is not confined but should be. Refusing to continue to avoid permission escalation attacks
Please make sure that the snapd.apparmor service is enabled and started.
```

`just --version` fails with the same message.

## Root cause hypothesis

The landlock backend sets `NO_NEW_PRIVS` before `landlock_restrict_self`,
because the kernel requires that flag for unprivileged self-sandboxing.
`snap-confine` depends on elevated privileges (setuid/capabilities) and an
AppArmor profile transition on exec. `NO_NEW_PRIVS` blocks both, so
`snap-confine` decides it is running unconfined and refuses to start. The
snapd.apparmor hint in the message is a red herring: the service is fine
outside the sandbox.

If that is right, no landlock rule change will fix this. Some options:

- Run snap commands outside the NNP process. For example, the launcher
  (which has no NNP set) could exec them on the sandboxed process's behalf.
  Doing that would also take them out of the filesystem sandbox, which is a
  real security trade-off.
- Detect `/snap/bin/*` and resolve to a non-snap install, or tell the user
  clearly to install `gh` and `just` some other way (apt, cargo, release
  binary).
- At minimum, have the Bash hook recognize this failure and print an
  actionable message instead of snap-confine's misleading one.

## Relevant files

- `src/sandbox/landlock.rs:231`: `restrict_self()`, where NNP and landlock
  take effect for sandboxed commands
- `src/sandbox/landlock.rs:115`: the sandbox notice explaining NO_NEW_PRIVS
- `src/hook.rs:257`: hook text about privilege escalation being impossible
