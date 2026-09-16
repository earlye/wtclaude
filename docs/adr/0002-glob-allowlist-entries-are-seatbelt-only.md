---
status: accepted
---

# Glob allowlist entries are a Seatbelt-only capability

Glob **Allowlist** entries (`~/.claude.json*`) exist to permit the
**atomic-write dance** — write `{file}.{tmpid}`, rename over `{file}` — which
needs create and unlink rights on the file's *parent directory*. SBPL can
express that as an anchored regex; no Linux mechanism can. So globs stay
Seatbelt-only, and the Landlock backend prints one loud line per glob entry it
cannot honor rather than silently granting nothing.

## Considered options

- **Warn per entry** (chosen). Silently granting nothing is precisely the bug
  in issue 019f8ff2, where an entry looked like it worked and matched nothing.
- **Drop globs from both platforms.** Tempting — it would delete
  `glob_to_regex`, `glob_match`, `resolve_glob_prefix`, the wildcard branch of
  `validate_allowlist` and the two-enforcement-path consistency problem they
  created. Rejected as a deliberate macOS regression with no escape hatch for
  a future pattern-shaped entry.
- **Session-local `CLAUDE_CONFIG_DIR`.** Verified workable: a relocated config
  dir plus a seeded `.credentials.json` authenticates fine, the whole directory
  is grantable so the dance works, and the real `~/.claude.json` becomes
  untouchable. Rejected for now because it puts a copy of an OAuth token in the
  session dir and silently discards config changes made during a session.
- **Best-effort expansion on Linux** (grant file-level write to existing
  matches). Rejected: buys nothing, since Claude's writer uses the dance rather
  than writing in place.

## Consequences

`claude mcp add` and nested-session config persistence do not work inside a
Linux sandbox. Under the accidental-writes-first threat model this is arguably
correct: `~/.claude.json` holds global trust settings and MCP server commands
for every project on the machine.

Note the constraint is about *location*, not about the dance. gpg performs the
same dance on `pubring.kbx` when merely signing, and it works fine because
`~/.gnupg` is a dedicated directory that can be granted wholesale. The dance is
unexpressible only for a file sitting directly in `$HOME`.
