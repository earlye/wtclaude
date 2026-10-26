# Launch claude under nice so it can't starve the host

## Context

wtclaude should start `claude` with a lowered scheduling priority (nice'd)
so that a session, and the builds, tests, and subagents it spawns, can't
saturate the host and make it unresponsive. Children inherit the niceness,
so setting it on the launched `claude` process covers everything the
session runs.

## Relevant files

- `src/launch.rs:297` — `Command::new("claude")` for the worktree launch path
- `src/launch.rs:423` — `Command::new("claude")` for the second launch path (resume-only args)
- `src/launch.rs:594` — `Command::new("claude")` for the third launch path (labelled session)
- `src/sandbox/landlock.rs:299` — the landlock helper execs the final command; also a possible place to apply the niceness, since every sandboxed launch passes through it
