# Detect and block agent attempts to use heredoc

## Context

Follow-up to `issues/resolved/resolved-019f4874-a52c-79f2-a3ba-439ef4f80e4c-heredoc-single-quote-corruption.md`. That investigation found that heredocs nested inside `$(...)` command substitution inside double quotes (e.g. `git commit -m "$(cat <<'EOF' ... EOF)"`) trigger a genuine Apple bash 3.2 quote-parsing bug, unrelated to wtclaude's sandboxing. The only mitigation shipped was a warning appended to the system prompt (`src/launch.rs:157-177`, the `sandbox_notice`), telling agents to avoid the pattern and use `git commit -F <tempfile>` instead.

An agent has now hit this bug again despite the warning — relying on the system prompt to steer model behavior isn't sufficient. The ask: tighten this down so shell commands containing heredoc syntax are rejected by the `PreToolUse` hook before they ever reach the shell, with an error message pointing back at the system-prompt warning (e.g. "heredoc is blocked because of the bug warned about in the system prompt...").

## Relevant files

- `src/hook.rs:36` — `run()`, the `PreToolUse` hook entry point (only active when `WTCLAUDE_SANDBOX` is set).
- `src/hook.rs:83` — `wrap_bash_in_sandbox()`, where the `Bash` tool's `command` is inspected/rewritten before being wrapped in `sandbox-exec`. Natural place to add a pre-check that denies instead of wrapping.
- `src/hook.rs:138` — `deny_bash()`, existing helper for producing a `permissionDecision: deny` response with a reason string.
- `src/launch.rs:157` — `sandbox_notice`, the existing system-prompt warning text this feature is meant to backstop.

## Open questions

- Scope: block ALL heredoc syntax (`<<`/`<<-`) outright, or only the specific bug-triggering shape (heredoc nested inside `$(...)` inside double quotes)? The bug was only proven for the nested shape; a plain `cat > file <<'EOF' ... EOF` wasn't shown to be affected.
- Detection method: naive substring/regex scan for `<<` vs. something nesting-aware. A naive scan risks false positives (e.g. `python -c "print(1<<2)"`, a bitshift in an embedded snippet) and could itself be fooled by quoting the same way the bash bug is.
- Exact error message text and whether it should restate the workaround (`git commit -F <tempfile>`).

## Grill Log

### 2026-07-18

- Q: Block scope — ban all heredoc syntax outright, or only try to detect the specific nested-in-`$(...)`-in-`"..."` shape that's proven buggy? — A: Block all heredocs. Reliably detecting just the risky nested shape would require reimplementing bash's own quote-parsing logic (the same class of problem that caused the original bug), and safe alternatives (temp file + `-F`, or the Write tool) are cheap.
- Q: Detection precision — naive quote-blind scan for `<<` vs. a quote/paren-aware scanner? — A: Neither exactly as posed. Match an opener (`<<`/`<<-`, optional quote, then an identifier) **and** confirm a subsequent line consisting solely of that identifier (allowing leading tabs for `<<-`). Requiring both the open and a matching close line makes stray `<<` (e.g. a bitshift in an embedded snippet) statistically not worth guarding against, without needing any shell quote-tracking.
