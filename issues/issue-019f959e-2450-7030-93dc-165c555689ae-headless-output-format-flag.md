# Add `--output-format {format}` flag to `wtclaude headless`

## Context

`wtclaude headless` needs a new flag, `--output-format {format}`, that threads directly
through to the underlying `claude --print` invocation (i.e. it should map to `claude`'s
own `--output-format` flag).

Note: the project has no `clap` dependency (checked `Cargo.toml` and `src/main.rs`) —
headless arg parsing is a hand-rolled loop in `parse_headless_args`, not a clap
derive/builder. The new flag should follow that existing manual-parsing pattern
(like `--mode`, `--resume`, `--show-policy`) rather than introducing clap.

## Relevant files

- `src/launch.rs:227-232` — `HeadlessArgs` struct; needs a new `output_format: Option<String>` field.
- `src/launch.rs:234-273` — `parse_headless_args`; needs a new `"--output-format" => { ... }` match arm that consumes a value, mirroring `--mode`/`--resume`.
- `src/launch.rs:275-353` — `run_headless`; needs to append `cmd.arg("--output-format").arg(value)` to the `claude` invocation (built starting at `src/launch.rs:318`), alongside the existing `--print`/`--settings`/`--append-system-prompt` args.
- `src/main.rs:161` — usage string (`wtclaude headless [--mode MODE] [--resume SESSION_ID] [--show-policy] [PROMPT]`) should be updated to mention `--output-format`.
- `tests/headless.rs` — existing headless CLI tests; new flag needs coverage.
- `src/launch.rs:1157` (`mod headless_tests`) — unit tests for `parse_headless_args`; add cases for `--output-format`.

## Next steps

- Decide default behavior when `--output-format` is omitted (likely: don't pass the flag to `claude` at all, let `claude` use its own default).
- Confirm valid format values are validated by `claude` itself rather than re-validated in wtclaude (keep wtclaude's parsing a passthrough).
