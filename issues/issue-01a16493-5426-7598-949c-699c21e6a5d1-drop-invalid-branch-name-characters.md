# Drop invalid characters from the branch name instead of failing

## Context

If you launch `wtclaude <name>` with a name that git won't accept as a
branch name, the launch fails. It should strip the invalid characters and
carry on with the cleaned-up name.

Right now `ensure_worktree` passes `name` to `git worktree add` (and
`-b <name>`) unchanged. Git rejects the name, and wtclaude reports this
after all three attempts fail:

```
git worktree add failed: <git's stderr>
```

The only change wtclaude makes to the name today is for the worktree
*directory*, where `sanitize_name` replaces `/` with `+`. Nothing cleans the
*branch* name.

`git check-ref-format --branch` rejects all of these, for example:

| Input         | Why git rejects it                    |
|---------------|---------------------------------------|
| `fix the bug` | space                                 |
| `a..b`        | `..`                                  |
| `a:b` `a~b` `a^b` `a?b` `a*b` `a[b` `a\b` | forbidden characters |
| `a@{b`        | `@{`                                  |
| `a//b` `a/`   | empty path component, trailing slash  |
| `.a` `a.`     | leading or trailing `.`               |
| `a.lock`      | `.lock` suffix                        |
| `-a`          | leading `-` (and clap treats it as a flag anyway) |

## Open questions

- **Drop or replace?** Taken literally, "drop" turns `fix the bug` into
  `fixthebug`. Replacing spaces with `-` might read better, but that's a
  choice for the user to make.
- **Some invalid parts aren't single characters:** `..`, `//`, `@{`, a
  `.lock` suffix, and a leading or trailing `.` or `/`. They need
  collapsing or trimming, not character-by-character removal.
- **Validation:** sanitize first, then check the result with
  `git check-ref-format --branch`. Fail clearly if nothing usable is left
  (for example, the input was all punctuation).
- **What uses the cleaned name:** it has to be the one name used everywhere
  the branch name goes, which is the worktree path (`sanitize_name`), the
  tmux window title, `claude --name`, and the post-exit menu. Otherwise a
  later relaunch with the same raw input won't find the same worktree.
- **Tell the user:** probably print a notice when the name changes, so they
  know which branch they're on.

## Relevant files

- `src/launch.rs:728`: `ensure_worktree`, which passes the raw name to
  `git worktree add`
- `src/launch.rs:602`: `sanitize_name`, which only handles `/` and only for
  the directory name
- `src/launch.rs:132`: `Args.name`, the `WORKTREE_NAME` positional
- `src/launch.rs:183`, `src/launch.rs:229`: the tmux window name and
  `claude --name`, which also use `args.name`

## Grill Log

### 2026-10-22

- Q: Drop or replace invalid characters? — A: Split rule. Whitespace runs
  become a single `-` (`fix the bug` → `fix-the-bug`). Other forbidden
  characters (`~ ^ : ? * [ \`, control characters) are deleted. Multi-character
  problems collapse: `..` → `.`, `//` → `/`, `@{` removed, leading `-`/`.`/`/`
  and trailing `.`/`/`/`.lock` trimmed (`trailing.` → `trailing`). Repeat until
  the name stops changing.
