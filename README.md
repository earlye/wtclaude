# wtclaude

`wtclaude` is a CLI wrapper for
[Claude Code](https://claude.ai/code) that launches an AI coding
session inside a git worktree with file-write sandboxing. It combines
an OS sandbox — macOS Seatbelt, or Linux Landlock — with Claude's
`PreToolUse` hooks to confine all writes to the worktree directory, so
a session can run hands-off without the rest of the machine being in
range.

The primary thing it prevents is *accidental* damage: a session editing
your main branch, a sibling worktree, or a dotfile because it misread
the task. Confining writes also shrinks the blast radius of prompt
injection, but it does not close it — reads and network access are
unrestricted, so credentials elsewhere on the machine remain readable
and reachable. And no sandbox can protect you from malicious code
*inside* the worktree, since that is exactly what a session writes;
review before running or merging is what covers that.


## How it works

When you run `wtclaude <name>`, it:

1. Resolves the git repo root and computes the worktree path at
   `.claude/worktrees/<name>`.

2. Creates the git worktree (or reuses it if it already exists), by
   running `git worktree add` directly — the worktree is managed by
   `wtclaude`, not delegated to `claude`.

3. Works out the **plan**: the set of paths this session may write to —
   the worktree, the repo's `.git` directory, and common package
   manager cache directories (Cargo, npm, pip, etc.) so that dependency
   fetching works without leaving the sandbox.

4. Registers a `PreToolUse` hook (itself, via `wtclaude hook`) that
   intercepts every tool call Claude attempts:

   - **Bash**: rewrites the command to run under the sandbox backend,
     enforcing write restrictions at the OS level.

   - **Write / Edit / NotebookEdit**: checks the target path against
     the worktree boundary and denies the call if it would write
     outside.

5. Sets `hasTrustDialogAccepted: true` for the repo in `~/.claude.json`
   so Claude's trust dialog is bypassed automatically.

6. Launches `claude` with the hook settings, `--append-system-prompt`
   (informing Claude of the sandbox boundary), and the working directory
   set to the worktree.

7. If running inside tmux, renames the current window to `<name>` for
   easy navigation (and restores the original name on exit).

8. When the Claude session exits, shows an interactive menu to keep or
   remove the worktree.

The settings JSON is written to `/tmp` and deleted automatically when
the session exits. Each session also gets a scratch directory under
`~/.local/state/wtclaude/sessions/<pid>/`, removed on exit, which holds
any profile the backend has to write to disk. It deliberately lives
outside the plan — `/tmp` is writable by the session, so a profile kept
there could be rewritten by the very session it constrains.

Each scratch directory holds an `owner.lock` that its session keeps open,
so a later launch can tell an orphaned directory (from a session killed
with `SIGKILL`) from one still in use, and sweeps only the orphans. Age
is not used for this: a session running for weeks would otherwise have
its profile deleted out from under it.


## Sandbox backends

| Backend | Platform | Mechanism |
|---------|----------|-----------|
| `seatbelt` | macOS | Compiles an SBPL profile, runs each Bash command under `/usr/bin/sandbox-exec`. |
| `landlock` | Linux 5.19+ | Applies a Landlock ruleset in-process via `wtclaude sandbox`, which then execs the command. |
| `bubblewrap` | Linux | Reserved, not implemented. Asking for it reports so explicitly. |

Selection is automatic — the platform's preferred available backend —
and can be pinned with `sandbox-backend` in `~/.config/wtclaude/wtclaude.yml`.

If no backend on the machine can enforce a plan, `wtclaude` refuses to
launch rather than starting a session that only looks sandboxed.

### Differences between backends

Neither backend is a superset of the other; these are the places where
the same configuration behaves differently.

**`sudo` cannot work under Landlock.** The kernel requires the
`NO_NEW_PRIVS` flag before an unprivileged process may sandbox itself,
and that same flag stops setuid binaries from elevating. So `sudo`,
`doas`, `pkexec` and `su` fail no matter how they are invoked. The hook
refuses them in command position with an explanation, rather than
letting them fail with a confusing kernel message. Install toolchains
outside the sandbox and let the session use them.

**Heredocs are only restricted under Seatbelt.** Apple's bash 3.2
miscounts quote nesting for a heredoc inside a `$(...)` inside double
quotes — the common `git commit -m "$(cat <<'EOF' ... EOF)"` idiom —
so the hook denies heredocs there outright. Linux bash parses that
shape correctly, so they are allowed.

**Glob allowlist entries only work under Seatbelt.** An entry like
`~/.claude.json*` compiles to an anchored SBPL regex on macOS. Landlock
has no path-pattern matching at all: it grants rights per directory or
file, and *unions* overlapping rules, so it cannot express "this
directory but only these names". Such entries are reported at launch
and grant nothing on Linux. See
[ADR-0002](docs/adr/0002-glob-allowlist-entries-are-seatbelt-only.md).

**Landlock rules need paths that already exist.** A rule is attached to
an open file descriptor, so an allowlisted cache directory that isn't
there yet is skipped. The plan is re-derived on every Bash command, so
such a path becomes writable as soon as something outside the sandbox
creates it — but a tool cannot create it from inside and then write to
it in the same command. `--show-policy` lists which paths were skipped.

**Sandboxes nest on Linux.** Landlock rulesets stack, each intersecting
the last, so a `wtclaude` session inside a `wtclaude` session works and
cannot widen what the outer one allowed. macOS refuses `sandbox_apply`
from an already-sandboxed process, so nesting fails there.


## Requirements

- macOS (uses `/usr/bin/sandbox-exec`), or Linux 5.19+ with Landlock
  enabled — `CONFIG_SECURITY_LANDLOCK=y` and `landlock` present in
  `/sys/kernel/security/lsm`. Linux 6.2+ is preferable; before that
  Landlock has no `TRUNCATE` right, and `wtclaude` warns once that
  `truncate(2)` on a path outside the sandbox is unrestricted.

- [Claude Code](https://claude.ai/code) CLI (`claude`) on your `PATH`

- A git repository

- tmux (optional, for automatic window renaming)


## Installation

Build from source with Cargo:

```sh
cargo build --release
cp target/release/wtclaude /usr/local/bin/
```

The binary must be on your `PATH`. The optional config file
(`wtclaude.yml`) is looked up next to the binary at runtime.

### Zsh completion

Add one line to `~/.zshrc`:

```zsh
eval "$(wtclaude --completions zsh)"
```

Once loaded, `wtclaude --resume <TAB>` lists recent sessions (showing
worktree and date). After selecting a session ID, pressing `<TAB>` on the
next word auto-fills the matching worktree name.


## Usage

```
wtclaude [--mode MODE] [--no-pull] [--resume SESSION_ID] \
         [--show-policy] [--test-sandbox-breakage hide|missing] \
         WORKTREE_NAME [INITIAL_PROMPT]
```

`WORKTREE_NAME` is the name for the git worktree. `wtclaude` creates
it at `.claude/worktrees/<name>` inside your repo. Slashes in the name
are replaced with `+` to match Claude's own directory naming.

`INITIAL_PROMPT` (optional) is passed as the opening prompt to Claude.
If it contains spaces, quote it or pass it as multiple trailing
arguments — they are joined with spaces. A prompt that starts with a
hyphen must be preceded by `--`, e.g. `wtclaude my-feature -- "-1 fix
this"` — otherwise it looks like an unrecognized flag.

### Flags

| Flag | Description |
|------|-------------|
| `--mode MODE` | Operation mode (see Modes). Overrides `WTCLAUDE_DEFAULT_MODE` and config. |
| `--no-pull` | Skip `git pull` before launching. |
| `--resume SESSION_ID` | Resume a previous Claude session by ID. |
| `--show-policy` | Print the plan the backend would enforce, and pause before launching. |
| `--test-sandbox-breakage hide\|missing` | Inject a sandbox fault for testing (see below). |

Examples:

```sh
# Start a sandboxed session for a feature branch
wtclaude my-feature

# Start with an initial task
wtclaude fix-login "Fix the broken login redirect"

# Use the 'dangerous' mode (skips permission prompts)
wtclaude --mode dangerous refactor-auth

# Skip pulling before launch
wtclaude --no-pull my-feature
```


## Modes

The active mode controls which flags are passed to `claude`. Modes are
defined in `wtclaude.yml` (see Configuration below). The built-in
modes are:

| Mode | Claude flags | Notes |
|------|-------------|-------|
| `safe` | _(none)_ | Default. Claude prompts for permission. |
| `dangerous` | `--dangerously-skip-permissions` | Claude acts without prompts. |

The mode is resolved in priority order:

1. `--mode MODE` flag (highest priority)
2. `WTCLAUDE_DEFAULT_MODE` environment variable
3. `default-mode` in `wtclaude.yml`
4. Built-in default (`safe`)

Set `WTCLAUDE_DEFAULT_MODE` in your shell profile to change the default
without editing config files:

```sh
export WTCLAUDE_DEFAULT_MODE=dangerous
```


## Configuration

Place a `wtclaude.yml` file next to the `wtclaude` binary to override
defaults. If no file is found, the built-in defaults are used.

```yaml
default-mode: safe

modes:
  safe:
    claude-flags: []
  dangerous:
    claude-flags: ["--dangerously-skip-permissions"]
```

You can add custom modes with any `claude` flags you need:

```yaml
default-mode: safe

modes:
  safe:
    claude-flags: []
  dangerous:
    claude-flags: ["--dangerously-skip-permissions"]
  verbose:
    claude-flags: ["--verbose"]
```

### Per-machine configuration

`~/.config/wtclaude/wtclaude.yml` holds settings that belong to the
machine rather than the binary:

```yaml
# Which backend enforces the plan: auto (default), seatbelt, landlock.
sandbox-backend: auto

# Extra paths a session may write to, beyond the worktree, .git and the
# built-in cache directories. `~` is expanded.
allowlist:
  - ~/.gnupg
  - ~/some/shared/cache

# Unix sockets a session may write to and connect to.
socket_allowlist:
  - ~/.colima/default/docker.sock
```

An `allowlist` entry containing `*` is a glob, which only the Seatbelt
backend can express (see Differences between backends). An entry whose
wildcard has no literal path prefix — `*`, `/*`, `*.log` — is rejected
at startup, since it would match nearly every path and quietly disable
the sandbox.


## Sandbox enforcement

Two layers of enforcement work together:

**OS layer** — the backend enforces the plan on every Bash command.
Writes outside the allowed paths fail at the kernel level, with
`Operation not permitted` under Seatbelt or `Permission denied`
(`EACCES`) under Landlock. A `PostToolUseFailure` hook injects context
into Claude's next message so it understands why the write failed.

The plan is derived fresh for each Bash command rather than compiled
once at launch. It costs a few milliseconds against a hook that already
pays a process spawn, and on Linux it is close to required: a Landlock
rule needs a path that already exists, so a cache directory created
part-way through a session would otherwise stay unwritable for the rest
of it.

The following paths are writable in addition to the worktree and
`.git`:

| Tool | Path |
|------|------|
| Cargo | `~/.cargo` |
| Rust toolchain | `~/.rustup` |
| npm | `~/.npm` |
| pnpm | `~/.pnpm-store`, `~/.local/share/pnpm` |
| Yarn | `~/.yarn`, `~/.cache/yarn` |
| pip | `~/.cache/pip` |
| uv | `~/.cache/uv` |
| Poetry | `~/.cache/pypoetry` |
| RubyGems | `~/.gem` |
| Bundler | `~/.bundle` |
| Maven | `~/.m2` |
| Gradle | `~/.gradle` |
| Go modules | `~/go/pkg/mod` |
| Composer | `~/.composer` |
| NuGet | `~/.nuget` |
| Conan | `~/.conan2` |
| Docker | `~/.docker` |
| cargo-xwin | `~/Library/Caches/cargo-xwin` (macOS) |
| Go build cache | `~/.cache/go-build` (Linux) |
| gpg | `~/.gnupg` |
| Keychain | `~/Library/Keychains` (macOS) |
| Agent sockets | `$XDG_RUNTIME_DIR` (Linux) |

| Temp files | `/tmp`, `$TMPDIR`, plus `/private/tmp`, `/var/folders`, `/private/var/folders` (macOS) or `/var/tmp` (Linux) |
| Devices | `/dev/null` (macOS), the whole of `/dev` (Linux) |

Package manager *installers* (e.g. Homebrew) are not allowlisted;
only caching directories are included.

`~/.gnupg` is included because `commit.gpgsign` makes signing part of
committing, and gpg takes a lock in the keyring *directory* before it
will sign — it creates `.#lk<addr>`, links it to `pubring.kbx.lock`,
and unlinks it afterwards. Without write access there, `git commit`
fails with `failed to write commit object`, which points nowhere near
the sandbox. (It writes nothing to `trustdb.gpg` in the process; the
lock is pessimism against a concurrent gpg rewriting the keybox
mid-read.)

On Linux the whole of `/dev` is granted rather than an enumerated list.
Ordinary filesystem permissions still stop a non-root session from
writing real devices, and the enumeration is long and easy to leave a
gap in — a missing `/dev/null` alone breaks `git`, `mktemp`, `stty` and
any script redirecting to it.

**Hook layer** — The `PreToolUse` hook inspects `Write`, `Edit`, and
`NotebookEdit` tool calls before Claude executes them. Any path
outside the worktree is denied immediately with an explanation, before
any filesystem access occurs.


## Post-exit menu

After the Claude session exits, `wtclaude` shows a brief interactive
menu:

```
> keep worktree my-feature
  remove worktree my-feature
```

Use the arrow keys to select and press Enter. Choosing **remove**
runs `git worktree remove` on the worktree directory. Ctrl-C or
selecting **keep** leaves the worktree in place.


## Testing sandbox breakage

The `--test-sandbox-breakage` flag is for development and testing:

- `hide` — omits `WTCLAUDE_BACKEND` from the environment (simulates the
  hook being unable to tell which backend is active).

- `missing` — points `WTCLAUDE_SESSION_DIR` at a path that does not
  exist (simulates the launching process being gone).

In both cases `wtclaude hook` will block all Bash execution and report
the reason to Claude. The hook never falls back to picking a backend
itself, so neither fault can quietly result in an unconstrained
command.

The `--show-policy` flag prints the plan to stdout and pauses (waiting
for Enter) before launching Claude. Useful for inspecting exactly what
write paths are granted — and, on Linux, which were skipped for not
existing yet.

`wtclaude sandbox --describe` (Linux) prints the same plan from inside
the wrapper, reading `WTCLAUDE_SANDBOX` and `WTCLAUDE_REPO_ROOT` from
the environment the way a real Bash call would.


## License

MIT — see [LICENSE](LICENSE).
