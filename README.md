# wtclaude

`wtclaude` is a macOS CLI wrapper for
[Claude Code](https://claude.ai/code) that launches an AI coding
session inside a git worktree with file-write sandboxing. It uses the
macOS `sandbox-exec` facility and Claude's `PreToolUse` hooks to
confine all writes to the worktree directory, so Claude cannot
accidentally modify your main branch or files outside the task scope.


## How it works

When you run `wtclaude <name>`, it:

1. Resolves the git repo root and computes the worktree path at
   `.claude/worktrees/<name>`.

2. Creates the git worktree (or reuses it if it already exists), by
   running `git worktree add` directly — the worktree is managed by
   `wtclaude`, not delegated to `claude`.

3. Writes a temporary SBPL (Sandbox Policy Language) file that allows
   file writes within the worktree, the repo's `.git` directory, and
   common package manager cache directories (Cargo, npm, pip, etc.) so
   that dependency fetching works without leaving the sandbox.

4. Registers a `PreToolUse` hook (itself, via `wtclaude hook`) that
   intercepts every tool call Claude attempts:

   - **Bash**: rewrites the command to run under `sandbox-exec`,
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

The SBPL policy file and the settings JSON are written to `/tmp` and
deleted automatically when the session exits.


## Requirements

- macOS (uses `/usr/bin/sandbox-exec`)

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
         [--show-policy] [--test-sbpl-breakage hide|missing] \
         WORKTREE_NAME [INITIAL_PROMPT]
```

`WORKTREE_NAME` is the name for the git worktree. `wtclaude` creates
it at `.claude/worktrees/<name>` inside your repo. Slashes in the name
are replaced with `+` to match Claude's own directory naming.

`INITIAL_PROMPT` (optional) is passed as the opening prompt to Claude.
If it contains spaces, quote it or pass it as multiple trailing
arguments — they are joined with spaces.

### Flags

| Flag | Description |
|------|-------------|
| `--mode MODE` | Operation mode (see Modes). Defaults to `safe`. |
| `--no-pull` | Skip `git pull` before launching. |
| `--resume SESSION_ID` | Resume a previous Claude session by ID. |
| `--show-policy` | Print the generated SBPL policy and pause before launching. |
| `--test-sbpl-breakage hide\|missing` | Inject a sandbox policy fault for testing (see below). |

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


## Sandbox enforcement

Two layers of enforcement work together:

**OS layer** — `sandbox-exec` enforces the SBPL policy on every Bash
command. Writes outside the allowed paths fail with `Operation not
permitted` at the kernel level. A `PostToolUseFailure` hook injects
context into Claude's next message so it understands why the write
failed.

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
| cargo-xwin | `~/Library/Caches/cargo-xwin` |

| Temp files | `/tmp`, `/private/tmp`, `/var/folders`, `/private/var/folders`, `$TMPDIR` |

Package manager *installers* (e.g. Homebrew) are not allowlisted;
only caching directories are included.

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

The `--test-sbpl-breakage` flag is for development and testing:

- `hide` — omits `WTCLAUDE_SBPL` from the environment entirely
  (simulates a missing env var).

- `missing` — sets `WTCLAUDE_SBPL` to a path that does not exist
  (simulates a deleted policy file).

In both cases `wtclaude hook` will block all Bash execution and report
the reason to Claude.

The `--show-policy` flag prints the generated SBPL policy to stdout
and pauses (waiting for Enter) before launching Claude. Useful for
inspecting exactly what write paths are allowlisted.


## License

MIT — see [LICENSE](LICENSE).
