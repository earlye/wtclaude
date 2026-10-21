# Read default-mode and modes from ~/.config/wtclaude/wtclaude.yml

## Context

wtclaude reads its settings from two different files, and the keys
cannot be swapped between them:

- `wtclaude.yml` next to the binary (`config::load`): `default-mode` and
  `modes`.
- `~/.config/wtclaude/wtclaude.yml` (`config::load_user`): `allowlist`,
  `socket_allowlist` and `sandbox-backend`.

The user put `default-mode: dangerous` and a `modes:` block in
`~/.config/wtclaude/wtclaude.yml`. Running `wtclaude test` still launched
in safe mode, and nothing said why. `UserConfig` does not reject unknown
fields, so the misplaced keys were silently ignored. No `wtclaude.yml`
existed next to the binary (`~/.cargo/bin`), so the built-in default
(`safe`) applied.

The user wants `default-mode` and `modes` read from
`~/.config/wtclaude/wtclaude.yml` as well. Where both files set them,
the user file takes priority over the one next to the binary.

The binary-adjacent file has its own problem: a standard
`cargo install` puts it in `~/.cargo/bin`, inside `~/.cargo`, which the
sandbox makes writable (`PKG_CACHE_DIRS`). A session can therefore edit
the file that sets the claude flags for later sessions. The related
writable-PATH-dirs issue,
`issues/issue-01a0ac0b-34a4-7d52-94cf-41549da6e90d-writable-path-dirs-allow-unconfined-exec.md`,
has the same shape.

## Relevant files

- `src/config.rs:7` — `Config` (`default-mode`, `modes`)
- `src/config.rs:14` — `UserConfig` (`allowlist`, `socket_allowlist`,
  `sandbox-backend`); has no `deny_unknown_fields`
- `src/config.rs:47` — `load_user`, which reads
  `~/.config/wtclaude/wtclaude.yml`
- `src/config.rs:137` — `load`, which reads `wtclaude.yml` next to
  `current_exe()` and falls back to `src/default_config.yml`
- `src/launch.rs:152`, `src/launch.rs:300`, `src/launch.rs:433`,
  `src/sessions.rs:9` — callers of `config::load`
- `README.md:243` — the order in which the mode is chosen (`--mode`, then
  `WTCLAUDE_DEFAULT_MODE`, then `default-mode`, then `safe`)
- `README.md:254` — the Configuration section, which documents the two
  files separately

## Open questions

These have not been decided yet:

- If both files define `modes`, should the user file's map replace the
  binary file's map, or be merged into it mode by mode?
- Should `UserConfig` also reject keys it still doesn't recognise, so a
  misplaced key is an error at startup rather than ignored?

## Grill Log

### 2026-10-21

- Q: If both files define `modes`, does the user file's map replace the binary file's map, or merge? — A: Merge by mode name. `modes.{key}` in the user file replaces `modes.{key}` from the binary-adjacent file (or built-in default) as a whole entry; `claude-flags` lists are not concatenated. Modes the user file doesn't name are kept. E.g. user `modes.dangerous = {a,b}` over binary `modes.dangerous = {c,d}` yields `{a,b}`.
