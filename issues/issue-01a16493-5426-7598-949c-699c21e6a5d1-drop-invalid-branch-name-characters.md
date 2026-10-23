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

## Decisions

- **Sanitizing rules:** leading and trailing whitespace is trimmed rather
  than turned into `-`; inner whitespace runs become a single `-`. The forbidden
  characters `~ ^ : ? * [ \` and control characters are deleted. `..`
  collapses to `.`, `//` to `/`, and `@{` is removed. Leading `-`/`.`/`/` and
  trailing `.`/`/`/`.lock` are trimmed. Repeat until the name stops changing.
  Examples: `fix the bug` → `fix-the-bug`, `trailing.` → `trailing`.
- **Validation:** check the result with `git check-ref-format --branch`. If
  it's empty or rejected, bail before any git mutation with a clear error
  naming the raw input. There is no generated fallback name.
- **One name everywhere:** clean once at the top of `run` and use that value
  for the in-place `current_branch()` comparison, the tmux window title, the
  worktree path (`sanitize_name`), `ensure_worktree`, the sandbox notice,
  `claude --name`, and the post-exit menu.
- **Notice:** if the name changed, print
  `wtclaude: using branch '<clean>' (from '<raw>')` to stderr.

## Relevant files

- `src/launch.rs` `branch_name` / `sanitize_branch_name`: clean the raw name
  and validate it with `git check-ref-format --branch`
- `src/launch.rs` `run`: cleans the name once and uses the result everywhere
- `src/launch.rs` `sanitize_name`: maps the branch name to the worktree
  directory name (`/` → `+`)
- `src/launch.rs` `ensure_worktree`: creates the branch and worktree
- `src/launch.rs` `Args.name`: the `WORKTREE_NAME` positional

## Grill Log

### 2026-10-22

- Q: Drop or replace invalid characters? — A: Split rule. Whitespace runs
  become a single `-` (`fix the bug` → `fix-the-bug`). Other forbidden
  characters (`~ ^ : ? * [ \`, control characters) are deleted. Multi-character
  problems collapse: `..` → `.`, `//` → `/`, `@{` removed, leading `-`/`.`/`/`
  and trailing `.`/`/`/`.lock` trimmed (`trailing.` → `trailing`). Repeat until
  the name stops changing.
- Q: How to validate, and what if nothing usable is left? — A: After
  sanitizing, check the result with `git check-ref-format --branch`. If the
  result is empty or git rejects it, bail before any git mutation with a clear
  error naming the raw input. No generated fallback name, because a fallback
  would break "same raw input finds the same worktree".
- Q: Should the user be told the name changed? — A: Yes. When the cleaned
  name differs from the raw one, print one line to stderr:
  `wtclaude: using branch 'fix-the-bug' (from 'fix the bug')`. Print nothing
  when it's unchanged, and don't ask for confirmation.
