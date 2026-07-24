# Add `--include-partial-messages` flag to `wtclaude headless`

## Context

`wtclaude headless` needs a new flag, `--include-partial-messages`, that
threads directly through to the underlying `claude --print` invocation
(same passthrough shape as `claude`'s own `--include-partial-messages` flag).

This surfaced immediately after tracking and grilling
`issues/issue-019f959f-8af9-7181-bde7-9a24f8999c9d-switch-to-clap-based-parsing.md`
(migrate wtclaude's CLI parsing to clap, whole-CLI scope, decided
2026-07-24). Unlike `--output-format`
(`issues/resolved/resolved-019f959e-2450-7030-93dc-165c555689ae-headless-output-format-flag.md`,
implemented against the old hand-rolled parser before the clap decision was
made), this flag has not been implemented yet — it should most likely be
built directly against the new clap-derived `HeadlessArgs` once the clap
migration lands, rather than against the hand-rolled `parse_headless_args`.

Unlike `--output-format`, `--include-partial-messages` is a boolean flag in
the underlying `claude` CLI (no value), so it should be modeled as a plain
`bool` (like the existing `--show-policy`), not an `Option<String>`.

## Relevant files

- `src/launch.rs:227-273` — `HeadlessArgs` / `parse_headless_args` (or their
  clap-derived replacement, depending on migration order) — needs an
  `include_partial_messages: bool` field/flag.
- `src/launch.rs:275-353` — `run_headless` — needs to append
  `cmd.arg("--include-partial-messages")` to the `claude` invocation when set.
- `src/main.rs:161` — usage string — should mention
  `--include-partial-messages`.
- `tests/headless.rs` — needs coverage mirroring
  `headless_subcommand_forwards_output_format_to_claude` /
  `headless_subcommand_places_resume_and_print_flags_and_forwards_prompt`.

## Blocked by

- `issues/issue-019f959f-8af9-7181-bde7-9a24f8999c9d-switch-to-clap-based-parsing.md`
  (soft dependency — implement this on top of clap rather than the old
  hand-rolled parser; not a hard blocker if clap migration stalls).
