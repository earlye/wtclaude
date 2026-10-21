# Writable directories on PATH let a session run code unconfined

## Context

Found by the clean-context review of
[the Linux sandbox branch](./issue-01a06db9-0f7c-7d72-9c3a-3a3c398a02bd-linux-sandbox-backend-trait.md),
and verified on that branch. Two distinct vectors, one shared cause: the
**Plan** grants `~/.cargo` wholesale so that builds work, and `~/.cargo/bin`
is both inside that grant and early on `PATH`. Anything a session writes
there runs later *outside* the sandbox.

Both are prompt-injection vectors, not accidental-damage ones, so neither
blocks Linux support — `CONTEXT.md` ranks accidental writes first and
injection second, best-effort. Filed so the decision is deliberate rather
than forgotten.

### 1. The wrapper binary can be replaced

`sandbox::landlock::Landlock::confine` re-execs `std::env::current_exe()` for
every Bash command. Installed the documented way (`cargo install --path .`)
that binary is `~/.cargo/bin/wtclaude` — writable from inside the sandbox.
A session can overwrite the very wrapper that confines it; the next Bash
call runs the replacement.

This is the same hazard the `SessionDir` doc comment already designs around
for profile files ("`/tmp` is writable by the session, so a profile kept
there could be rewritten by the session it constrains"). The reasoning was
never extended to the binary.

Mitigation already documented in `README.md`: install to `~/.local/bin`,
which is not in the plan. That is a workaround, not a fix — the default
install path is still exposed.

### 2. `git` is resolved through PATH from unconfined contexts

`launch::git_dirs` calls `Command::new("git")`, i.e. a `PATH` lookup. It runs
from two places that are *not* sandboxed:

- the `PreToolUse` hook process, which is never sandboxed; and
- `wtclaude sandbox`, which derives the plan *before* calling `restrict()`.

So planting `~/.cargo/bin/git` yields arbitrary unconfined execution on the
next Bash call, without touching the wtclaude binary at all. Verified that
`~/.cargo/bin` precedes `/usr/bin` on `PATH` on the dev host.

## Why the obvious fix is not obviously right

Resolving `git` to an absolute path at launch is not sufficient on its own:
a *previous* compromised session could already have planted
`~/.cargo/bin/git`, so resolving from `PATH` at launch inherits the same
problem one launch earlier. A real fix needs a trusted resolution policy —
prefer `/usr/bin/git`, or refuse to start when `git` resolves to a path
inside the plan.

Narrowing the `~/.cargo` grant is also not straightforward: cargo needs to
write `.package-cache` and `.global-cache` at the root of `~/.cargo`, so
granting only `registry/` and `git/` breaks builds with "failed to acquire
package cache lock".

## Options

1. **Warn at launch** when the wtclaude binary, or the resolved `git`, sits
   inside a granted subtree — same shape as the existing `unhonored`
   warnings, no behavior change. Cheap, but fires for every `cargo
   install`-based setup, so it risks being noise that gets ignored.
2. **Refuse to start** in that case. Honest, but breaks the documented
   install path until the user moves the binary.
3. **Resolve `git` absolutely, preferring `/usr/bin/git`**, and pass it to
   the hook and wrapper via the environment. Closes vector 2 specifically.
4. **Accept and document**, which is the current state.

## Relevant files

- `src/sandbox/landlock.rs` — `confine`, which re-execs `current_exe()`
- `src/sandbox/mod.rs` — `PKG_CACHE_DIRS` (`~/.cargo`), and the `SessionDir`
  doc comment that states the principle this violates
- `src/launch.rs` — `git_dirs`, the `Command::new("git")` call
- `README.md` — "Where you install the binary matters on Linux"

## Not affected

macOS. Seatbelt's wrapper is `/usr/bin/sandbox-exec`, not the wtclaude
binary, and the `~/.cargo` grant has the same shape but no re-exec of a
session-writable binary. Vector 2 applies to the hook there too, since
`git_dirs` is shared.
