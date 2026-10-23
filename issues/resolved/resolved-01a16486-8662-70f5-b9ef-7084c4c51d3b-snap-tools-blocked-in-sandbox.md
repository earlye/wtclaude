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

## Root cause (confirmed)

The landlock backend sets `NO_NEW_PRIVS` before `landlock_restrict_self`,
because the kernel requires that flag for unprivileged self-sandboxing.
`/snap/bin/<app>` is a symlink to `/usr/bin/snap`, which hands off to
`snap-confine`: a setuid/file-caps helper that builds a mount namespace,
switches AppArmor profile and moves the process into a `snap.<name>.<app>`
cgroup. `NO_NEW_PRIVS` blocks the privilege and the profile transition, so
`snap-confine` finds itself unconfined and refuses. The snapd.apparmor hint
is a red herring. No landlock rule change can fix this.

The payload does not need `snap-confine` when the snap is
`confinement: classic` (no namespace, no AppArmor/seccomp; snap-confine only
does refresh-awareness bookkeeping). `gh` and `just` are both classic, and
running `/snap/gh/current/gh` and `/snap/just/current/bin/just` directly
works inside the sandbox. Strict snaps depend on their base-snap mount
namespace and stay unsupported.

## Plan

1. **Per-mode `path-prepend` in `wtclaude.yml`.** Add
   `path-prepend: Vec<String>` to `Mode` (`src/config.rs`) next to
   `claude-flags`, and add it to `MODE_KEYS`. The launcher prepends the
   entries, in order, to `PATH` in the environment it gives Claude, so
   every Bash command and every nested call (justfile recipes, scripts)
   inherits it. Example:

   ```yaml
   modes:
     dangerous:
       claude-flags: [ "--dangerously-skip-permissions" ]
       path-prepend: [ "/snap/just/current/bin" ]
   ```

   - Expand a leading `~` only; no `$VAR` expansion.
   - An entry that doesn't exist: warn at launch, still prepend.
   - An entry inside one of the plan's write subtrees: warn at launch (it
     recreates the writable-PATH hole from
     `issue-01a0ac0b-…-writable-path-dirs-allow-unconfined-exec.md`), don't
     refuse.
   - Applies to every backend (it is just `PATH`).
   - `default_config.yml` ships no `path-prepend`; which snaps to expose
     depends on the machine, so it goes in `~/.config/wtclaude/wtclaude.yml`.
   - Modes merge whole (existing behaviour), so no merge logic is needed.

2. **Hook deny for snap commands, landlock only.** Alongside the
   `privilege_escalation_command` check (`src/hook.rs:161`): if a
   command-position word resolves through `PATH` to `/snap/bin/<app>`,
   deny it. Read `/snap/<name>/current/meta/snap.yaml`:
   - classic: name the direct payload path (`/snap/<name>/current/<command>`)
     and suggest the user add its directory to this mode's `path-prepend`;
   - strict: say it can't run under NO_NEW_PRIVS and to ask the user for a
     non-snap install.

   Nested calls can't be caught by the hook; `path-prepend` is the fix for
   those.

### Rejected

- **Relay snap commands through the unsandboxed launcher.** A general
  escape hatch out of the filesystem sandbox (`gh api`, extensions,
  aliases), and no relay channel exists to build on.
- **Generate shims from `meta/snap.yaml` at launch.** More machinery than
  the problem needs; `path-prepend` gives the user the same result with an
  explicit allow-list.
- **Refuse to launch on writable `path-prepend` entries.** wtclaude is a
  hallucination safety net, not a security boundary.

## Relevant files

- `src/sandbox/landlock.rs:231`: `restrict_self()`, where NNP and landlock
  take effect for sandboxed commands
- `src/sandbox/landlock.rs:115`: the sandbox notice explaining NO_NEW_PRIVS
- `src/hook.rs:257`: hook text about privilege escalation being impossible
- `src/hook.rs:161`: where the snap deny check goes, beside the `sudo` one
- `src/config.rs:55`: `Mode`, where `path-prepend` goes (and `MODE_KEYS`)
- `src/launch.rs:223`, `:345`, `:514`: where mode config is applied at launch

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
- Q: How are `path-prepend` entries interpreted? — A: Leading `~` expanded,
  no `$VAR`; missing dirs warn but are still prepended; applies on every
  backend; `default_config.yml` ships none.
