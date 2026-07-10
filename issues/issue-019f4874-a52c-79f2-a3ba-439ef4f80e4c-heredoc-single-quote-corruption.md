# Sandbox wrapper corrupts commands containing single quotes (e.g. quoted heredocs)

## Context

Running Claude Code inside a wtclaude-managed sandboxed worktree. Claude Code's Bash tool issued a command containing a heredoc with a quoted delimiter (`<<'EOF' ... EOF`), which is standard POSIX/bash syntax used to pass a multi-line string (a git commit message) to a subcommand without the shell expanding `$`, backticks, etc. inside it.

This is a recurrence of a previously-reported heredoc issue — heredocs still do not work.

Exact tool call issued (Bash tool `command` parameter, verbatim):

```
git add cli/devcon/src/app.rs cli/devcon/src/term_emulator.rs issues/issue-019f477a-13b0-7810-b45f-e06b02143882-introduce-app-struct.md && git commit -m "$(cat <<'EOF'
Introduce App struct for introduce-app-struct

Moves run()'s pty/reader/writer/emulator/cols/rows locals into an App
struct, with App::new() owning setup and run_loop() holding the event
loop, so future state (e.g. a session tree) has somewhere to live.
Also adds a term_emulator::new_emulator() factory so app.rs no longer
names the concrete AlacrittyTerminalEmulator type directly.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

**Expected behavior:** This is valid, ordinary shell syntax — it runs correctly in a plain terminal (bash or zsh) with no modification. It should execute unchanged inside the sandbox wrapper.

**Actual behavior:** Exit code 2, with:

```
sh: -c: line 3: unexpected EOF while looking for matching `''
sh: -c: line 12: syntax error: unexpected end of file
```

## Root cause hypothesis

(From black-box observation, not code review.) The wrapper appears to take the fully-assembled command string and re-wrap it in its own single quotes before invoking `sandbox-exec`, i.e. something like `sh -c '<the whole command>'`.

This is directly visible in another command from the same session that did work, captured in a task-notification:

```
/usr/bin/sandbox-exec -f /tmp/wtclaude-sbpl-5691.sb sh -c 'find / -path /proc -prune -o -type d -iname "portable-pty-0.9*" -print 2>/dev/null | head -5'
```

That confirms the `sh -c '...'` outer-wrapping pattern. The problem: the original command contains a literal single quote as part of `<<'EOF'`. When the wrapper stuffs the whole command inside its own `'...'`, that embedded single quote closes the wrapper's outer quoting early, corrupting everything after it — hence "unexpected EOF while looking for matching `'`".

Suggested fix direction (untested, no code review done): when re-wrapping the command for `sh -c`, the wrapper needs to escape single quotes in the original command using the standard technique (replace `'` with `'\''`) rather than assuming the command contains no single quotes. Any command using `<<'EOF'`, `awk '...'`, `grep '...'`, etc. will trigger this same failure.

## Relevant files

- `src/hook.rs:149-161` — `shell_single_quote()`, the function that escapes the command for `sh -c '...'`.
- `src/hook.rs:108-112` — `wrap_bash_in_sandbox()`, where the `sh -c '<escaped-command>'` wrapper is assembled.

## Prior history — this was already reported and "fixed" once

This is a recurrence of `issues/resolved-019f14f2-5a8a-76a1-975d-b240655c42cf-single-quote-sandbox-quoting.md`, filed 2026-07-01, describing this exact heredoc-corruption bug.

That issue's own "Suggested fix direction" section already warned: *"Single-quote escaping cannot reliably survive complex shell constructs (quoted heredoc delimiters, nested quoting, etc.)"* and recommended abandoning `sh -c '<quoted-command>'` in favor of a temp-file/base64 approach.

PR #21 (commit `2e7f18f`, "Fix shell_single_quote to use char-by-char escaping... matching the Gemini-suggested approach") did not do that. It rewrote `shell_single_quote`'s `s.replace('\'', "'\\''")` call as an equivalent char-by-char loop — same escaping algorithm, byte-for-byte the same output string, just different Rust style. The issue was then marked resolved in commit `9ed7f6c` with no functional code change and apparently no repro test of the heredoc case.

So the underlying bug was never actually fixed — PR #21 was cosmetic only. Any real fix needs to change the escaping *strategy* (e.g. the temp-file/base64 approach from the prior issue), not just the implementation style of the same broken strategy.

## Investigation & resolution

The hypothesis above (both this issue's and the prior resolved issue's) turned out to be **wrong**. Added regression tests in `src/hook.rs` (`#[cfg(test)] mod tests`) exercising `shell_single_quote()` and the full `wrap_bash_in_sandbox()` path:

- `shell_single_quote_survives_quoted_heredoc` — the minimal Gemini-style repro (`cat <<'EOF' ... EOF`)
- `shell_single_quote_survives_real_world_commit_command` — the exact multi-line commit body from the report above
- `shell_single_quote_survives_command_substitution_around_heredoc` — the exact nesting shape that actually failed: `git commit -m "$(cat <<'EOF' ... EOF)"` (double-quoted command substitution wrapping a single-quote-delimited heredoc)
- `wrap_bash_in_sandbox_survives_real_sandbox_exec` — calls the real production function and runs its output through a real `/usr/bin/sandbox-exec` invocation (wide-open `(allow default)` policy), not just a simulated `sh -c` re-parse. Self-skips (with an explanation) when run from inside a wtclaude sandbox, since macOS blocks a sandboxed process from calling `sandbox_apply` again — confirmed independently by running `sandbox-exec` directly in a sandboxed session and seeing the same `sandbox_apply: Operation not permitted`. Verified passing when run from a plain, unsandboxed terminal.

All four pass. The `'` → `'\''` single-quote escaping in `shell_single_quote()` (present, unchanged in substance, since the very first commit of this file) correctly survives quoted heredoc delimiters, command substitution, and a real `sandbox-exec` round-trip for the exact command shape that was reported as failing — both in this issue and the original 2026-07-01 report. There is no defect in the escaping algorithm.

A stale/pre-fix `wtclaude` binary was considered (plausible explanation for the recurrence, since the fix landed 2026-07-01 and this report also predates a rebuild) but the user ruled it out: a crash the night before this report caused a full relaunch of all agents, which would have picked up a current binary. Confirmed separately: the binary actually registered as the `PreToolUse` hook (`/Users/earlye/.cargo/bin/wtclaude`) was built from commit `5262174` (2026-07-07), which already includes PR #21 — not stale.

### The actual root cause: this is a real bash 3.2 parsing bug, unrelated to wtclaude

While self-reviewing this fix, the exact same failure reproduced live in this session on a fresh commit — using code already proven correct by the tests above. Forensics on that live failure:

1. Captured the *exact* PreToolUse JSON payload and fed it through the real installed `wtclaude hook` binary, then ran its exact wrapped output — reproduced the identical error (`unexpected EOF while looking for matching` / `syntax error: unexpected end of file`).
2. Verified phase-1 unwrapping (the outer `sh -c '<escaped>'` single-quote parsing) reconstructs the original command text **byte-for-byte** before any execution happens — confirmed via a `printf '%s'` probe, not just reasoning about it.
3. Ran the **raw, unescaped original command** directly via a single `sh -c` — no wtclaude, no hook, no escaping involved at all — and it **failed identically**.
4. Bisected to the minimal trigger: a heredoc body nested inside `$(...)` inside double quotes (`"$(cat <<'EOF' ... EOF)"`) fails whenever the heredoc body's single-quote characters have an odd "effective" toggle count — where a `\'` sequence gets absorbed by the shell's own quote-tracking as one non-toggling unit instead of two literal characters (heredoc body content is supposed to be completely opaque to quoting, but the parser's quote-balance prescan doesn't treat it that way).
5. Reproduces identically on both `/bin/sh` and `/bin/bash` on this Mac — both report `GNU bash, version 3.2.57(1)-release`, Apple's ancient, GPLv3-frozen bash. Does **not** reproduce on `zsh`.

This is a genuine bash 3.2 parsing bug for heredocs nested in command substitution inside double quotes, triggered by certain single-quote arrangements in the heredoc body — completely independent of wtclaude. The original 2026-07-01 report's commit message had exactly one apostrophe (`run()'s`, an odd count) — that's why it hit this. `shell_single_quote()` and `wrap_bash_in_sandbox()` were never the bug; both this issue's and the original resolved issue's hypotheses were wrong. Likely not wtclaude-specific either: if Claude Code's own Bash tool defaults to `/bin/bash` on macOS (same ancient 3.2.57), any session using the `git commit -m "$(cat <<'EOF' ... EOF)"` idiom — which Claude Code itself commonly generates — can hit this, sandboxed or not, whenever the message body's quote arrangement is unlucky.

### What was actually done about it

No code change to `shell_single_quote()`/`wrap_bash_in_sandbox()` — there was nothing to fix there, and the regression tests added above stay as valid coverage that the escaping is sound. Instead, `src/launch.rs`'s `sandbox_notice` (appended to every spawned session's system prompt via `--append-system-prompt`) now warns against the heredoc-in-`$()`-in-double-quotes pattern and recommends `git commit -F <tempfile>` instead, since this is the cheapest way to actually prevent agents from stepping on this landmine going forward.

### Follow-up (tracked separately, not implemented here)

To make "was this a stale binary" answerable instead of guessed at, `wtclaude` should export `WTCLAUDE_VERSION` (the crate version) into the environment of the `claude` process it launches, and the `PostToolUseFailure` sandbox-violation hook message should include it — so any future sandbox-related failure report carries the running binary's version for free.
