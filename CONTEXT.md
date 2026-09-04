# wtclaude

`wtclaude` launches a Claude Code session inside a git worktree and confines
its file writes to that worktree, so a session can be run hands-off (up to
`--dangerously-skip-permissions`) without the rest of the machine being in
range.

## Language

### The confinement

**Sandbox**:
The confinement a session runs under.
_Avoid_: jail, container

**Sandbox root**:
The single directory a session owns — a worktree, or the DIRECTORY given to
`wtclaude path`.
_Avoid_: sandbox (unqualified), worktree (only one kind of sandbox root)

**Blast radius**:
What a session can damage — the **Sandbox** plus any code inside it that the
user later executes outside it.

### How it is expressed

**Plan**:
The computed, platform-neutral set of paths a session may write to.
_Avoid_: policy, allowlist (an **Allowlist** is one input to a **Plan**)

**Backend**:
The mechanism that enforces a **Plan** — Seatbelt on macOS, Landlock on
Linux, bubblewrap reserved as a second Linux option.
_Avoid_: sandbox, driver, engine

**Profile**:
A **Backend**'s rendered form of a **Plan** — SBPL text, a Landlock
ruleset, or bubblewrap argv.
_Avoid_: policy, SBPL (SBPL is one Backend's dialect, not the concept)

**Sandbox notice**:
The `--append-system-prompt` text telling a session what its **Backend**
forbids, so it doesn't waste turns discovering it. Backend-specific:
Seatbelt warns about heredocs (Apple's bash 3.2 miscounts quote nesting
inside `$(...)`), Landlock warns that privilege escalation is impossible.

**Allowlist**:
User-configured paths (`wtclaude.yml`) added to the **Plan** beyond the
sandbox root, `.git`, and the built-in package-manager cache dirs.

**Atomic-write dance**:
Writing `{file}.{tmpid}` and renaming it over `{file}`. Common, not a
Claude quirk — gpg does it to `pubring.kbx` when merely signing. Needs
create/unlink rights on the file's *parent directory*, so it is grantable
whenever the file lives in a dedicated directory (`~/.gnupg`) and
unexpressible only when the file sits directly in `$HOME`
(`~/.claude.json`).

## Threat model

Ranked, because the ranking decides how much machinery is proportionate:

1. **Accidental writes** (primary) — a session edits the main branch, a
   sibling worktree, or a dotfile because it misread the task.
2. **Prompt injection** (secondary, best-effort) — a session is steered by
   hostile content it read. Containing writes shrinks this but does not
   close it; reads and network are unrestricted, so credentials remain
   readable and reachable.

The **Sandbox** cannot protect against malicious code *inside* the
**Blast radius** — a session writes code the user will later run. Review
before running or merging is the residual control.

## Relationships

- A session has exactly one **Sandbox root** and exactly one **Plan**
- A **Plan** is derived fresh per Bash call, from the sandbox root, the repo
  root and the **Allowlist**
- A **Plan** is rendered into a **Profile** by exactly one **Backend**
- A **Backend** decides whether a **Profile** is even materialized: Seatbelt
  must write one to disk, Landlock applies it in-process, bubblewrap puts it
  in argv

## Example dialogue

> **Dev:** "Where does the **Profile** live so the hook can point at it?"
>
> **Author:** "Only Seatbelt needs it to live anywhere — `sandbox-exec -f`
> wants a path. Landlock re-derives the **Plan** inside the wrapper and
> applies it directly, so there's nothing on disk to point at."
>
> **Dev:** "Then what does the child process need in its environment?"
>
> **Author:** "The **Sandbox root**, the repo root, the session scratch dir,
> and which **Backend** is active. Not a **Profile** path."

## Flagged ambiguities

- "policy" was used for both the platform-neutral path set and the rendered
  backend-specific text — resolved: **Plan** and **Profile** respectively.
  The code still says `generate_sbpl_policy` / `WTCLAUDE_SBPL`, which
  conflates the two and names one dialect; being renamed.
- "sandbox" was used for both the confinement and the root directory —
  resolved: **Sandbox** and **Sandbox root**.
- The README described the goal as preventing *accidental* modification,
  while the motivation for hands-off sessions is partly prompt injection —
  resolved above: accidental is the spec, injection is a bonus, and the
  README should say so.
- Glob **Allowlist** entries (`~/.claude.json*`) are expressible in SBPL as
  a regex but in no Linux mechanism — resolved: they are a Seatbelt-only
  capability, and the Landlock backend warns per entry rather than silently
  granting nothing. See [ADR-0002](docs/adr/0002-glob-allowlist-entries-are-seatbelt-only.md).
- The heredoc ban was assumed to be a property of the **Sandbox** — resolved:
  it is a property of one **Backend**'s shell (Apple's bash 3.2). Verified
  working under Linux bash, so it belongs in Seatbelt's **Sandbox notice**
  only.
