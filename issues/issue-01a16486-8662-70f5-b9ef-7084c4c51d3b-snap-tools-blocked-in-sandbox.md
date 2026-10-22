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

## Grill Log

### 2026-10-22

- Q: Is the NO_NEW_PRIVS hypothesis right? — A: Confirmed in a landlock
  session: `NoNewPrivs: 1`, `gh --version` prints the snap-confine refusal.
  snap-confine is a privileged (setuid/file-caps) helper that builds a mount
  namespace, switches AppArmor profile, and moves the process into a
  `snap.<name>.<app>` cgroup; NNP neuters all of that and it refuses.
- Q: Can the snap payload run without snap-confine? — A: Yes for classic
  snaps. `gh` and `just` are both `confinement: classic`, and
  `/snap/gh/current/gh --version` and `/snap/just/current/bin/just --version`
  both work inside the sandbox (`gh auth status` too). For classic snaps
  snap-confine buys only refresh-awareness bookkeeping and a uniform launch
  path. Strict snaps depend on their base-snap mount namespace and are out of
  scope.
- Q: Relay snap commands through the unsandboxed launcher? — A: Rejected.
  It is a general escape hatch (`gh api`, extensions, aliases) out of the
  filesystem sandbox, and no relay channel exists to build on.
- Q: Generate per-session shims from `meta/snap.yaml`? — A: Rejected as more
  machinery than the problem needs.
- Q: Which approach? — A: A per-mode `path-prepend` list in `wtclaude.yml`,
  so a mode opts in per tool (e.g. `dangerous` prepends just's dir but not
  gh's). Plus a hook deny for commands resolving to `/snap/bin/*` with an
  actionable message instead of snap-confine's.
- Q: What if a `path-prepend` entry is inside a writable subtree (the
  writable-PATH hole)? — A: Warn at launch, don't refuse. wtclaude is a
  hallucination safety net, not a security boundary (`CONTEXT.md` threat
  model: accidental writes primary, injection best-effort).
- Q: What should the hook do when a command still resolves to `/snap/bin/*`?
  — A: Deny under landlock (beside the `sudo` check). Read `meta/snap.yaml`:
  for a classic snap, name the direct payload path and suggest a
  `path-prepend` entry; for a strict snap, say it can't run and to install a
  non-snap build. Giving the direct path is fine — omitting `gh` from
  `path-prepend` is about convenience, not keeping the agent away from it.
