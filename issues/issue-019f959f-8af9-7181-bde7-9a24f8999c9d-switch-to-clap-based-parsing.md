# Switch CLI arg parsing to clap

## Context

The project currently has no `clap` dependency at all (confirmed: not in `Cargo.toml`,
not imported in `src/main.rs`). All CLI parsing is hand-rolled:

- `src/main.rs:9` — top-level subcommand dispatch is a manual `match` on
  `args.first()` (`hook`, `sessions`, `worktrees`, `modes`, `headless`,
  `session-worktree`, `--completions`, `--help`/`-h`, and a fallback to
  `launch::parse_args`).
- `src/launch.rs:234-273` — `parse_headless_args` is a manual loop matching on
  `--mode`, `--show-policy`, `--resume`, erroring on unknown `--` flags, and
  collecting everything else into a joined prompt string.
- `src/launch.rs` also has `parse_args` for the non-headless path (referenced at
  `src/main.rs:78`), same hand-rolled style.
- Zsh completions are generated manually (`src/main.rs:62-74`, `print_completions_zsh`)
  rather than via `clap_complete`.

This surfaced while tracking `issues/issue-019f959e-2450-7030-93dc-165c555689ae-headless-output-format-flag.md`
(add `--output-format` flag to `wtclaude headless`) — that issue was written to
follow the existing manual-parsing pattern, but the user wants to migrate to
clap instead, presumably so new flags (like `--output-format`) and future ones
get validation, `--help` generation, and shell completions for free.

## Relevant files

- `Cargo.toml` — add `clap` (and likely `clap_complete` to replace the manual
  zsh completion generator).
- `src/main.rs:6-90` — top-level dispatch; candidate for a clap derive
  `#[derive(Parser)]`/`#[derive(Subcommand)]` enum covering `hook`, `sessions`,
  `worktrees`, `modes`, `headless`, `session-worktree`.
- `src/launch.rs:227-273` — `HeadlessArgs` + `parse_headless_args`; candidate for
  a clap derive struct.
- `src/launch.rs:1157` (`mod headless_tests`) — existing unit tests for
  `parse_headless_args`; will need to be adapted to clap's parsing entry point.
- `tests/headless.rs` — CLI-level tests exercising headless arg parsing.
- `src/main.rs:62-74` — manual zsh completion generation, to potentially be
  replaced by `clap_complete`.

## Next steps

- Decide scope: migrate only headless args first, or the whole CLI (all
  subcommands) in one pass.
- Decide whether to keep `--completions zsh` as a custom subcommand or switch
  to clap's standard completion generation flow.
- Once scope is decided, revisit `issues/issue-019f959e-2450-7030-93dc-165c555689ae-headless-output-format-flag.md`
  to implement `--output-format` on top of clap rather than the old manual parser.
- `issues/issue-019f95be-1006-79b2-abc1-49e3b13b1bdc-headless-include-partial-messages-flag.md`
  (new `--include-partial-messages` headless flag) is soft-blocked on this issue too —
  build it against the clap-derived `HeadlessArgs` once this lands.

## Grill Log

### 2026-07-24

- Q: Migrate only headless args to clap, or the whole CLI (interactive launch
  path + top-level subcommand dispatch) in one pass? — A: Whole CLI in one
  pass. `main.rs`'s top-level dispatch becomes a clap derive enum/subcommand
  tree; both `parse_args` (interactive) and `parse_headless_args` (headless)
  become clap derive structs. Rationale: the CLI surface is small (2
  flag-taking subcommands, ~5 simple no-arg ones), so migrating incrementally
  would leave hand-rolled and clap parsing coexisting indefinitely, undercutting
  the point of migrating at all (consistent validation/`--help`/completions).
- Q: Keep the current hand-written dynamic zsh completion script, or switch to
  `clap_complete`? — A: Keep the custom script (`print_completions_zsh` in
  `main.rs`) as-is. It does live completion — shelling out to `wtclaude
  sessions`/`worktrees`/`modes` at completion time to suggest real session
  IDs, worktree names, and modes. `clap_complete` only generates static
  completions and cannot replicate that runtime behavior, so switching would
  be a regression. Migrating arg parsing to clap does not require touching
  completions at all.
