# Issue: Show dirty files before prompting to force-remove worktree

## Relevant code

`src/launch.rs`, `remove_worktree()` (~line 475):

```rust
fn remove_worktree(worktree_path: &Path, repo_root: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["worktree", "remove"])
        .arg(worktree_path)
        .current_dir(repo_root)
        .status()
        .context("git worktree remove")?;
    if !status.success() {
        use std::io::{self, Write};
        print!("git worktree remove failed. Force-remove? [y/N] ");
        ...
    }
}
```

## The bug

When `git worktree remove` fails (typically because the worktree has uncommitted
changes), the user is immediately asked whether to force-remove. They have no
context for *why* it failed — they cannot see what files are dirty without
opening a separate terminal.

## Desired behaviour

Before printing the force-remove prompt, run the equivalent of:

```sh
git -C <worktree_path> status --porcelain | head -n 10
```

and print the output so the user can make an informed decision. If the output
is truncated (more than 10 lines), print a note like `(… and more)`.

## Implementation notes

- Run `git status --porcelain` with `current_dir(worktree_path)` (not
  `repo_root`) so paths are relative to the worktree.
- Capture stdout, split on newlines, take the first 10, print them, then
  print the truncation notice if `line_count > 10`.
- The prompt that follows stays the same: `Force-remove? [y/N]`.

## Version note

Crate is at `v0.0.10` on `main`. Per `CLAUDE.md`, bump patch once per branch.

## Suggested skills

- `/verify` — manually trigger a failed `git worktree remove` (leave a dirty
  file) and confirm the status lines appear before the prompt
- `/self-review` — pre-PR check before opening the pull request
