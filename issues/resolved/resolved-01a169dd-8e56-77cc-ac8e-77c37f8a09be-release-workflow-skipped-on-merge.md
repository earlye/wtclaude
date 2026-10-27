# Release workflow didn't run for 083f753

## Context

083f753 squash-merged PR #42 at 2026-10-23 12:42:32 -0500. It added
`.github/workflows/release.yml`, which runs on `push` to `main` and stamps
`Cargo.toml` with `0.1.<commit count>`. It never ran.

Observed about two minutes after the merge:

- `gh api repos/earlye/wtclaude/actions/permissions`: Actions are
  `enabled`, with `allowed_actions: all`.
- The workflow is registered: id `394357973`, state `active`, created
  12:42:34, two seconds after the push.
- `actions/runs` has `total_count: 0`. No run has ever happened for any
  workflow in the repository.
- There are no `v0.1.*` tags. `Cargo.toml` is still `0.1.17`. With 51
  commits at 083f753, it should be `0.1.51`.

## Root cause (high confidence)

GitHub skips `push` and `pull_request` workflows when the head commit's
message contains `[skip ci]` (or `[ci skip]`, `[no ci]`, `[skip actions]`,
`[actions skip]`) **anywhere in the message, including the body**. The
squash-merge message of 083f753 contains it on body line 7, where it
describes what the workflow itself does:

```
entry, commits as github-actions[bot] with [skip ci], and tags v0.1.N, so
```

So writing about the marker in prose turned it on. The skip is silent: no
run is created, which matches the zero run count.

To confirm: the next push to `main` whose head commit doesn't mention the
marker should create a run. The commit that adds this issue file is
probably that push.

## Fixes

- **For 083f753 itself:** probably nothing to do. The workflow computes the
  version from the commit count at whatever `HEAD` it runs on, so the next
  run stamps `main` correctly and skips nothing. The only thing lost is
  that `v0.1.51` won't be tagged at 083f753 itself.
- **Add `workflow_dispatch:`** to `on:` so a skipped or failed release can
  be run by hand (`gh workflow run release.yml`).
- **Avoid the literal marker in prose:** write "a skip-ci marker", not the
  bracketed form, in commit messages, PR descriptions (squash merges copy
  them into the commit body), and docs that get copied into commit
  messages. Possibly add a check that flags the literal marker in a PR's
  squash message, though GitHub only puts that message together at merge
  time.
- The workflow's own release commit uses the marker on purpose
  (`chore: release $VERSION [skip ci]`), so the push it makes doesn't start
  the workflow again. That part is correct. A push made with the default
  `GITHUB_TOKEN` doesn't start workflows anyway, so the marker there is
  extra protection rather than required.

## Relevant files

- `.github/workflows/release.yml`: `on: push: branches: [main]`, with no
  `workflow_dispatch`
- `CLAUDE.md`: the Versioning section describes the per-merge stamping
- `issues/resolved/resolved-01a16994-a9d9-7542-92f0-3f7477eacbe8-version-from-git-commit-count.md`:
  the issue this workflow resolved
