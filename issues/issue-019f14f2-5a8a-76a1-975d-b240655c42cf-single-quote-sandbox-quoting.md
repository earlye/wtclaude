# Issue: Single-quote quoting breaks sandbox Bash wrapper for heredocs

## Context

Repo: https://github.com/earlye/wtclaude  
Branch to base off: `main` (currently at v0.0.10 after PR #18 merged)

## The bug

`wtclaude` sandboxes Bash commands by rewriting them in the `PreToolUse` hook
(`src/hook.rs`) to:

```
/usr/bin/sandbox-exec -f <policy.sb> sh -c '<escaped-command>'
```

The escaping is done by `shell_single_quote` (hook.rs, near the bottom):

```rust
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
```

This correctly escapes plain single quotes but breaks **quoted heredoc
delimiters**. When Claude tries to run:

```sh
git commit -m "$(cat <<'EOF'
...content with apostrophes like GC's...
EOF
)"
```

`shell_single_quote` turns `<<'EOF'` into `<<'\''EOF'\''`. The shell then
interprets the heredoc terminator as the literal string `'EOF'` (with quotes),
but the actual closing line is bare `EOF` — so the heredoc never closes:

```
sh: -c: line 3: unexpected EOF while looking for matching `''
sh: -c: line 12: syntax error: unexpected end of file
```

This was discovered in the ironic situation of landing PR #18 (which fixed the
sandbox policy file going missing) — the fix commit itself was blocked by this
quoting bug.

## Relevant code

| File | Location | What it does |
|------|----------|--------------|
| `src/hook.rs` | `wrap_bash_in_sandbox()` ~line 83 | Rewrites Bash commands into `sandbox-exec` invocations |
| `src/hook.rs` | `shell_single_quote()` ~line 155 | Escapes the command for `sh -c '...'` |

## Suggested fix direction

Single-quote escaping cannot reliably survive complex shell constructs (quoted
heredoc delimiters, nested quoting, etc.). The robust fix is to avoid
`sh -c '<quoted-command>'` entirely. Options in rough preference order:

1. **Temp file via base64 (cleanest)**: write the command bytes to a `mktemp`
   file in `/tmp` (already sandbox-allowlisted) via base64, then run
   `sandbox-exec ... sh <tempfile>`:
   ```sh
   _f=$(mktemp /tmp/wtclaude-cmd-XXXXXX.sh) && printf '%s' <base64> | base64 -d > "$_f" && /usr/bin/sandbox-exec -f <policy> sh "$_f"; _rc=$?; rm -f "$_f"; exit $_rc
   ```
   Base64-encoding the command bytes sidesteps shell quoting entirely; the
   wrapper is all ASCII-safe. The Rust side base64-encodes `command` before
   embedding it in the wrapper string.

2. **Write from Rust**: in the hook, write the command to a temp file before
   returning the `updatedInput`, then the wrapped command is simply
   `sandbox-exec -f <policy> sh <tempfile>`. Requires managing temp-file
   lifetime past the hook call, which is awkward since the hook just returns
   a JSON string.

Avoid `sh -c 'eval "$1"' --` — it has the same heredoc-delimiter problem.

## Version note

After PR #18 the crate is at `v0.0.10`. Per `CLAUDE.md`, bump the patch
version once per branch (`0.0.x` only).

## Suggested skills

- `/verify` — confirm a `git commit -m "$(cat <<'EOF'...EOF)"` with an
  apostrophe in the body succeeds in a live wtclaude session after the fix
- `/code-review` — run after implementing to catch remaining quoting edge cases
- `/self-review` — pre-PR check before opening the pull request
