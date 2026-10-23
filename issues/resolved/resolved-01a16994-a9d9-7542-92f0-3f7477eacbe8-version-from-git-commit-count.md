# Derive the patch version from git commit count (+ git hash)

## Context

`CLAUDE.md` asks every branch to bump the patch version in `Cargo.toml`
by hand. That gets missed: the per-mode allowlist branch (`613d56f358a3`)
merged as 08b1d60 without a bump, so two different builds both report
`0.1.16`, and `cargo install` can't tell them apart.

The user wants a `build.rs` that sets the patch level to the number of
commits in the tree, so nobody has to maintain it. If the version format
allows full semver, it should also append build metadata: `+{git-hash}`.

Current state:

- There is no `build.rs`.
- `Cargo.toml` has `version = "0.1.16"`.
- Nothing in `src/` reads `CARGO_PKG_VERSION`, and the clap `Args` has no
  `version` attribute, so `wtclaude --version` is parsed as a worktree name
  and fails ("unexpected argument '--version' found").
- `git rev-list --count HEAD` is currently 46, at 08b1d60. The patch
  number would jump from 16 to 46, which still increases.

## Constraint: `build.rs` cannot change the package version

`CARGO_PKG_VERSION`, `cargo install`'s "Replacing ... vX" line, and
`.crates2.json` all come from `Cargo.toml` itself. `build.rs` runs after
Cargo has read that version and can't change it. What `build.rs` *can* do
is emit `cargo:rustc-env=WTCLAUDE_VERSION=<version>+<hash>`, which the
binary reads with `env!` and shows in `--version`.

## Decision

Cargo's own version must be real (`cargo install` output, `.crates2.json`,
`cargo pkgid`), so builds can be pinned with
`cargo install --git <repo> --tag v0.1.N`. So the version goes into
`Cargo.toml`, written by CI rather than by hand:

1. **CI writes the version on merge.** `.github/workflows/release.yml` runs
   on push to `main`: it sets `Cargo.toml` to `0.1.<git rev-list --count
   HEAD>`, updates wtclaude's own `Cargo.lock` entry with
   `cargo update --workspace`, commits as `github-actions[bot]` with
   `chore: release 0.1.N [skip ci]`, tags `v0.1.N`, and pushes both. A
   `concurrency` group stops two releases racing; `[skip ci]` plus a
   "version already 0.1.N or tag exists" check stops it looping on its own
   commit. The release commit itself adds 1 to the count, so versions step
   by about 2 per merge; only monotonic matters.
2. **`build.rs` adds build metadata** for `wtclaude --version`:
   `{CARGO_PKG_VERSION}+{short-hash}`, plus `.dirty` with uncommitted
   changes, exposed as `WTCLAUDE_VERSION`.
3. **`--version`** on `Cli` and `Args`.
4. **`CLAUDE.md`** drops the hand-bump rule.

If `main` is protected, the Actions bot may need a ruleset bypass to push
the release commit and tag. We don't know this repo's protection setup.

## Full semver

Semver 2.0 build metadata (`+hash`) is valid in `Cargo.toml` and accepted
by clap's `version`. Here it is only in the `--version` string `build.rs`
writes; `Cargo.toml` carries plain `0.1.N`. Precedence rules ignore build
metadata, which is fine because the commit count already orders builds.

## Details to settle

- **Which count:** CI counts `git rev-list --count HEAD` on `main` only,
  so feature-branch commit counts never reach the version. Builds from a
  branch report the last version merged into it, plus their own hash.
- **Dirty tree:** append `.dirty` (`+<hash>.dirty`) when
  `git status --porcelain` is non-empty, because `cargo install --path .` from a worktree with
  uncommitted changes is exactly how the 0.1.16 confusion happened.
- **Rebuild triggers:** emit `cargo:rerun-if-changed` for `HEAD`, the ref
  it points to, `packed-refs`, and the index. In a linked worktree `.git` is
  a file, not a directory, so find the paths with `git rev-parse --git-path`
  rather than hardcoding `.git/HEAD`. Once any rerun-if-changed is emitted
  Cargo only reruns `build.rs` on those paths, so also list `src` and
  `Cargo.toml`, or `.dirty` goes stale after a source edit.
- **No git available:** a source tarball, or a build where `git` isn't on
  PATH, falls back to plain `CARGO_PKG_VERSION`, not a build failure.
- **Sandbox:** inside a wtclaude session, `build.rs` runs `git` under
  Landlock. Reading is unrestricted, and this was confirmed to work.
- **Add `--version`:** add clap's `version = env!("WTCLAUDE_VERSION")` to
  `Args` and to the `Cli`. `--version`/`-V` aren't subcommand keywords, so
  main.rs's argv routing passes them to `Args`, whose clap `version`
  answers them; no extra special case is needed.
- **`CLAUDE.md`:** replace the per-branch bump rule with "don't edit the
  version by hand; CI sets `0.1.<count>` and tags it on merge; minor/major
  bumps need an explicit decision and a change to the workflow's `0.1`
  prefix".

## Relevant files

- `.github/workflows/release.yml` (new): stamps and tags on merge to `main`
- `build.rs` (new): `WTCLAUDE_VERSION` with `+hash[.dirty]`
- `Cargo.toml:3`: the `version`, now written by CI
- `CLAUDE.md`: the "Versioning" rule
- `src/main.rs:16`, `src/main.rs:51`: the top-level `Cli` and argv routing
- `src/launch.rs:191`: `Args`
- `tests/dispatch.rs`: `--version`/`-V` dispatch test
