# wtclaude.yml glob allowlist entries (e.g. `~/.claude.json*`) are silently no-ops

## Context

While investigating a user report that `claude mcp add` printed a success
message but the server never actually showed up in `claude mcp list`, we
traced it to `~/.claude.json` writes being denied by the sandbox despite
`~/.claude.json*` being present in `wtclaude.yml`'s `allowlist` (intended to
cover the file plus its atomic-write `.tmpXXXX` siblings).

Verified live: `claude mcp add test-sandbox-probe -- echo hello` reported
"File modified: /Users/earlye/.claude.json", but the file's size/mtime never
changed, no entry was added, and it was absent from `claude mcp list`. The
CLI's own error handling for this write is a separate, secondary issue (not
wtclaude's) — it doesn't verify the write landed before reporting success.

## Root cause

Two independent sandbox enforcement paths, and both treated a trailing `*`
in an allowlist entry as a literal character rather than a glob:

- **Bash tool** (`src/launch.rs:generate_sbpl_policy`): `~/.claude.json*`
  had `~` expanded to `$HOME`, then was passed through `resolve()`
  (`p.canonicalize().unwrap_or(p)`). `canonicalize()` fails because no real
  path is literally named `.claude.json*` (asterisk and all), so it fell
  back to the unresolved literal string, which was then emitted as
  `(allow file-write* (subpath "/Users/x/.claude.json*"))`. Seatbelt's
  `subpath` does literal filesystem-path-prefix matching, not glob
  expansion — that rule matches nothing that can ever exist.
- **Write/Edit/NotebookEdit hook** (`src/hook.rs:is_within_sandbox`): same
  blind spot one level up — `PathBuf::starts_with` is component-wise, so a
  component containing a literal `*` never matches a real path's component.

Net effect: `~/.claude.json*` looked like it allowlisted `~/.claude.json`,
but no code path actually permitted writing to it.

## Relevant files

- `src/launch.rs` — `generate_sbpl_policy` (SBPL profile generation for the
  Bash-tool sandbox-exec wrapper), `resolve`.
- `src/hook.rs` — `is_within_sandbox` (PreToolUse hook for
  Write/Edit/NotebookEdit).

## Fix

- `src/launch.rs`: added `glob_to_regex()`, escaping regex metacharacters
  and turning `*` into `.*`. Allowlist entries containing `*` now bypass
  `canonicalize`/`subpath` and instead emit an anchored
  `(allow file-write* (regex #"..."))` rule.
- `src/hook.rs`: added `glob_match()`, a small `*`-wildcard string matcher.
  Entries containing `*` now match against the normalized path string
  directly instead of going through component-wise `starts_with`.
- Added regression tests for both (base file, `.tmpXXXX` siblings, and
  unrelated-path non-matches).
- Bumped `Cargo.toml` to `0.1.6` per this repo's per-branch versioning
  convention.

## Reproduction steps (pre-fix)

1. Ensure `~/.claude.json*` (or any allowlist entry ending in `*`) is in
   `wtclaude.yml`'s `allowlist`.
2. Start a wtclaude-sandboxed Claude Code session.
3. Run `claude mcp add some-server -- echo hi` via the Bash tool.
4. Observe a false "success"/"File modified" message; `claude mcp list`
   never shows the server, and `~/.claude.json`'s size/mtime don't change.
