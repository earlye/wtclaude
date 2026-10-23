# wtclaude

## Versioning

Don't edit `version` in `Cargo.toml` by hand. On each merge to `main`, CI (`.github/workflows/release.yml`) sets it to `0.1.<commit count>`, commits that, and tags `v0.1.<commit count>`. `wtclaude --version` adds `+<git hash>` (and `.dirty`) via `build.rs`. Minor and major bumps require an explicit decision, and mean changing the workflow's `0.1` prefix.
