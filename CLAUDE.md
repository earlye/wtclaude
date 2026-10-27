# wtclaude

## Versioning

Don't edit `version` in `Cargo.toml` by hand. On each merge to `main`, CI (`.github/workflows/release.yml`) sets it to `0.1.<commit count>`, commits that, and tags `v0.1.<commit count>`. `wtclaude --version` adds `+<git hash>` (and `.dirty`) via `build.rs`. Minor and major bumps require an explicit decision, and mean changing the workflow's `0.1` prefix.

In commit messages and PR titles, write "a skip-ci marker", never any of [GitHub's skip markers](https://docs.github.com/en/actions/managing-workflow-runs/skipping-workflow-runs) literally. GitHub silently skips push workflows when the head commit's message contains one anywhere, body included, and this repo's squash merges build that message from the PR title and the branch's commit messages. If a release was skipped anyway, run `gh workflow run release.yml`.
