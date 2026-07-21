# Detect and block agent attempts to use heredoc

## Context

Follow-up to `issues/resolved/resolved-019f4874-a52c-79f2-a3ba-439ef4f80e4c-heredoc-single-quote-corruption.md`. That investigation found that heredocs nested inside `$(...)` command substitution inside double quotes (e.g. `git commit -m "$(cat <<'EOF' ... EOF)"`) trigger a genuine Apple bash 3.2 quote-parsing bug, unrelated to wtclaude's sandboxing. The only mitigation shipped was a warning appended to the system prompt (`src/launch.rs:157-177`, the `sandbox_notice`), telling agents to avoid the pattern and use `git commit -F <tempfile>` instead.

An agent has now hit this bug again despite the warning — relying on the system prompt to steer model behavior isn't sufficient. The ask: tighten this down so shell commands containing heredoc syntax are rejected by the `PreToolUse` hook before they ever reach the shell, with an error message pointing back at the system-prompt warning (e.g. "heredoc is blocked because of the bug warned about in the system prompt...").

## Relevant files

- `src/hook.rs:36` — `run()`, the `PreToolUse` hook entry point (only active when `WTCLAUDE_SANDBOX` is set).
- `src/hook.rs:83` — `wrap_bash_in_sandbox()`, where the `Bash` tool's `command` is inspected/rewritten before being wrapped in `sandbox-exec`. Natural place to add a pre-check that denies instead of wrapping.
- `src/hook.rs:138` — `deny_bash()`, existing helper for producing a `permissionDecision: deny` response with a reason string.
- `src/launch.rs:157` — `sandbox_notice`, the existing system-prompt warning text this feature is meant to backstop.

## Decisions

- **Scope**: block *all* heredoc syntax, not just the specific nested-and-proven-buggy shape. Distinguishing safe from buggy shapes reliably would mean re-implementing bash's own quote-parsing (the same class of problem that caused the underlying bug), and the safe alternatives (temp file + `-F`, or the `Write` tool) are cheap.
- **Detection heuristic**: match a heredoc opener (`<<` or `<<-`, optional quote, then an identifier) *and* confirm a later line consisting solely of that identifier (leading tabs allowed for `<<-`). Requiring both open and matching close makes a stray `<<` (e.g. a bitshift in an embedded snippet) statistically not worth guarding against — no shell quote-tracking needed.
- **Placement**: first check inside `wrap_bash_in_sandbox()` (`src/hook.rs:83`), before the `sbpl_path` lookup, returning `deny_bash(...)` on a match. No new gating — it inherits the existing `WTCLAUDE_SANDBOX`/`Bash`-only gate from `run()` for free.
- **Deny message**: "heredoc is blocked because of the bug warned about in the system prompt: heredocs (even non-nested ones) are blocked outright because reliably distinguishing safe from buggy shapes isn't possible without re-implementing shell quote-parsing. Use a temp file instead, e.g. `git commit -F /tmp/msg.txt`, or write multi-line content with the Write tool."
- **`sandbox_notice` (`src/launch.rs:157`)**: leave unchanged — the deny message's reference to "the bug warned about in the system prompt" depends on the notice still describing it, and the notice still saves a wasted turn by steering the agent away before it hits the deny.

## Next steps

Implement in `src/hook.rs`:
1. Add a `contains_heredoc(command: &str) -> bool` helper implementing the open+matching-close-line heuristic above.
2. Call it at the top of `wrap_bash_in_sandbox()`; on a match, return `Ok(Some(deny_bash(HEREDOC_DENY_MESSAGE)))` before the `sbpl_path` checks.
3. Add unit tests: real heredoc commands (plain, and the nested `$(cat <<'EOF' ... EOF)"` shape) are denied; commands with `<<` but no matching closer line (e.g. an inline bitshift) are allowed through.

## Grill Log

### 2026-07-18

- Q: Block scope — ban all heredoc syntax outright, or only try to detect the specific nested-in-`$(...)`-in-`"..."` shape that's proven buggy? — A: Block all heredocs. Reliably detecting just the risky nested shape would require reimplementing bash's own quote-parsing logic (the same class of problem that caused the original bug), and safe alternatives (temp file + `-F`, or the Write tool) are cheap.
- Q: Detection precision — naive quote-blind scan for `<<` vs. a quote/paren-aware scanner? — A: Neither exactly as posed. Match an opener (`<<`/`<<-`, optional quote, then an identifier) **and** confirm a subsequent line consisting solely of that identifier (allowing leading tabs for `<<-`). Requiring both the open and a matching close line makes stray `<<` (e.g. a bitshift in an embedded snippet) statistically not worth guarding against, without needing any shell quote-tracking.
- Q: Where should the check live in `src/hook.rs`, and should it apply outside the existing sandbox gate? — A: First check inside `wrap_bash_in_sandbox()` (`src/hook.rs:83`), before the `sbpl_path` lookup, returning `deny_bash(...)` on a match. No new gating needed — it inherits the existing gate for free, since `run()` only calls `wrap_bash_in_sandbox` for `Bash` tool calls when `WTCLAUDE_SANDBOX` is set.
- Q: Exact wording of the deny message? — A: "heredoc is blocked because of the bug warned about in the system prompt: heredocs (even non-nested ones) are blocked outright because reliably distinguishing safe from buggy shapes isn't possible without re-implementing shell quote-parsing. Use a temp file instead, e.g. `git commit -F /tmp/msg.txt`, or write multi-line content with the Write tool."
- Q: Should `src/launch.rs`'s `sandbox_notice` be trimmed now that the hook enforces this? — A: Leave it unchanged. The deny message references "the bug warned about in the system prompt", which only makes sense if the notice still describes the bug; it also still helps by steering the agent away before it wastes a turn on a denied call.

## Resolution

Implemented as decided: `contains_heredoc()` and `has_matching_close_line()` added to `src/hook.rs`, called at the top of `wrap_bash_in_sandbox()` before the `sbpl_path` lookup. Detection matches a heredoc opener (`<<`/`<<-`, optionally quoted or backslash-escaped) plus a later line consisting solely of that delimiter (tabs tolerated only for `<<-`) — deliberately without tracking shell quoting, since a stray `<<` (e.g. a bitshift) essentially never has a coincidentally matching close line.

Self-review (`review-1.md`) caught a real gap during implementation: an unterminated/mismatched closing quote around the delimiter (e.g. `<<'EOF` with no closing `'`) was causing a false negative — a real heredoc opener slipping past undetected. Fixed by no longer requiring the closing quote to match, since detection was never meant to depend on the shell's own quoting being well-formed anyway. A second review pass came back clean.

`sandbox_notice` in `src/launch.rs` was left unchanged per the decision above. 11 new/existing tests pass in `src/hook.rs` (1 pre-existing test ignored, requires an unsandboxed terminal).
