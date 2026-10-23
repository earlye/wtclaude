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
is emit `cargo:rustc-env=WTCLAUDE_VERSION=0.1.<count>+<hash>`, which the
binary reads with `env!` and shows in `--version`.

Some options:

1. **Version only in the binary:** `build.rs` computes `0.1.<count>+<hash>`
   for `wtclaude --version`, and `Cargo.toml` stays at a fixed `0.1.0`.
   Drop the per-branch bump rule from `CLAUDE.md`. The cost is that
   `cargo install` output and `cargo pkgid` show the fixed number. That
   doesn't matter for a binary installed from a path.
2. **Version stamped into `Cargo.toml`:** a `just` recipe or git hook
   rewrites `Cargo.toml` before building or at merge time. This keeps
   Cargo's own version accurate, but it's back to managing a file and adds
   churn in the diff.
3. **Option 1 plus a bump-check:** fixes the version shown, but keeps the
   problem this issue is trying to remove.

Option 1 looks closest to what was asked. This needs a decision.

## Full semver

Semver 2.0 build metadata (`+hash`) is valid in `Cargo.toml` and accepted
by clap's `version`. Under option 1 it's a string we write, so nothing
stops us. Precedence rules ignore build metadata, which is fine because the
commit count already orders builds.

## Details to settle

- **Which count:** `git rev-list --count HEAD` counts commits on the
  current branch, so a feature branch with 5 commits shows more than `main`
  will after the squash-merge. `--first-parent` makes no difference for
  this repo's squash merges. Builds from different branches may not sort
  in order, which seems acceptable.
- **Dirty tree:** consider `+<hash>.dirty` when there are uncommitted
  changes, because `cargo install --path .` from a worktree with
  uncommitted changes is exactly how the 0.1.16 confusion happened.
- **Rebuild triggers:** emit `cargo:rerun-if-changed` for `HEAD` and the
  ref it points to. In a linked worktree `.git` is a file, not a directory,
  so find the paths with `git rev-parse --git-dir` and `--git-common-dir`
  rather than hardcoding `.git/HEAD`. If this is missing, the version stays
  stale until the next real rebuild.
- **No git available:** a source tarball, or a build where `git` isn't on
  PATH, needs a fallback such as `0.1.0+unknown`, not a build failure.
- **Sandbox:** inside a wtclaude session, `build.rs` runs `git` under
  Landlock. Reading is unrestricted, so this should work, but confirm it.
- **Add `--version`:** add clap's `version = env!("WTCLAUDE_VERSION")` to
  `Args`, and to the `Cli` in `src/main.rs:11`, so the stamped version is
  visible. The custom argv routing in `main.rs` (`--completions` is
  special-cased at `src/main.rs:47`) may need to handle `--version` before
  it falls back to treating the argument as a worktree name.
- **`CLAUDE.md`:** update or remove its "Versioning" section to match the
  chosen option.

## Relevant files

- `Cargo.toml:3`: the hand-maintained `version`
- `CLAUDE.md`: the per-branch patch bump rule
- `src/main.rs:11`, `src/main.rs:47`: the top-level `Cli` and argv routing,
  where `--version` would need to be added
- `src/launch.rs:113`: `Args` (`#[command(name = "wtclaude")]`, no
  `version`)
