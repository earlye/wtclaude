# Per-mode `allowlist` in wtclaude.yml

## Context

Requested by earlye, relayed by the `review-assistant` Claude session.

Add a `modes.<name>.allowlist` key. When that mode is active, its entries are
added to the top-level `allowlist`. Today `allowlist` exists only at the top
level (`UserConfig.allowlist`, `src/config.rs:21`), and `MODE_KEYS` is just
`claude-flags` and `path-prepend` (`src/config.rs:92`). So a path that only
one mode needs ends up writable in every mode.

### Motivating case

review-assistant's systemd Pass is moving to
`wtclaude headless --mode review-pass`. That Pass clones the code under review
into `~/.cache/review-assistant/`, so it needs write access there. earlye
doesn't want that directory writable from other wtclaude sessions.

Intended config:

```yaml
modes:
  review-pass:
    claude-flags: [...]
    path-prepend: [/snap/gh/current]
    allowlist:
      - ~/.cache/review-assistant
```

## Questions to settle

### Key spelling

earlye wrote `allowList`, but the top-level key is `allowlist`, and mode keys
are already lowercase (`claude-flags`, `path-prepend`). The likely answer is
`allowlist`. Note that unknown mode keys are rejected (`src/config.rs:157`),
so a misspelling will fail loudly, not be silently ignored.

### Does `sandbox::plan` know the active mode?

**No.** `plan(sandbox_root, repo_root)` (`src/sandbox/mod.rs:225`) re-reads
`config::load_user()` on every Bash call and uses only the top-level
`allowlist`. The mode is resolved only at launch
(`src/launch.rs:227` for worktree mode, `:378` for headless, `:512` for path
mode, each as `args.mode`, then `WTCLAUDE_DEFAULT_MODE`, then
`config.default_mode`). Nothing passes it down to the session.

`plan` has two callers at Bash time, and both currently get their inputs
from env vars that `SessionGuard::apply_env` sets (`src/launch.rs:69`):

- the PreToolUse hook (`src/hook.rs:186`, which reads `WTCLAUDE_SANDBOX` and
  `WTCLAUDE_REPO_ROOT`)
- the Landlock wrapper `wtclaude sandbox` (`src/sandbox/landlock.rs:275`),
  which re-derives the plan itself before calling `restrict_self`

A first idea is to export `WTCLAUDE_MODE` alongside them and give `plan` a
`mode` parameter. Both callers have to agree on the mode, because the
wrapper is the one that actually enforces it.

**Security consideration:** a wider allowlist now depends on that mode
input, so check whether a sandboxed command could influence what the
*wrapper* sees as `WTCLAUDE_MODE`. The trust model is the same as for the
existing `WTCLAUDE_SANDBOX`, which also controls what becomes writable.
Two options:
- **Rely on that model:** if it already holds for `WTCLAUDE_SANDBOX`, it
  holds for `WTCLAUDE_MODE` too. Confirm it does.
- **Record the mode elsewhere:** write it into the per-session directory
  (`WTCLAUDE_SESSION_DIR`), which lives outside the sandbox root.

### Validation

Per-mode entries should go through the same `validate_allowlist`
(`src/config.rs:191`, which rejects bare or root-anchored wildcards) as
top-level ones. Right now that check runs only on `config.allowlist`
(`src/config.rs:100`).

Also check whether `launch::is_sandbox_nullifying_root` and the glob
handling (`partition_allowlist`, `src/sandbox/mod.rs:307`;
ADR 0002 says glob entries are Seatbelt-only) apply in the same way.

### Should `socket_allowlist` get a per-mode version too?

`socket_allowlist` is also top-level only. It's out of scope unless a mode
needs it, but worth deciding whether the two should stay consistent.

## Relevant files

- `src/config.rs:19`: `UserConfig`, with top-level `allowlist` and
  `socket_allowlist`
- `src/config.rs:63`: the per-mode struct (`path_prepend` is the precedent
  for a per-mode list)
- `src/config.rs:92`: `MODE_KEYS`
- `src/config.rs:100`, `:191`: `validate_allowlist` and its single call site
- `src/sandbox/mod.rs:225`: `plan`, which has no mode input
- `src/hook.rs:186`, `src/sandbox/landlock.rs:275`: the per-Bash-call
  `plan` callers
- `src/launch.rs:69`: `apply_env`, where session env is exported
- `src/launch.rs:227`, `:378`, `:512`: mode resolution in each launch path
