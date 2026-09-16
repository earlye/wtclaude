//! macOS Seatbelt backend: renders an SBPL profile and runs each Bash
//! command under `/usr/bin/sandbox-exec`.
//!
//! Seatbelt is the only backend that must materialize a profile — `-f` wants
//! a path on disk — so it writes one uniquely-named file per Bash call into
//! the session dir.

use anyhow::{Context, Result};
use std::path::Path;

use super::{Availability, Backend, Plan, shell_single_quote};

pub struct Seatbelt;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

impl Backend for Seatbelt {
    fn name(&self) -> &'static str {
        "seatbelt"
    }

    fn availability(&self) -> Availability {
        if Path::new(SANDBOX_EXEC).exists() {
            Availability::Ready
        } else {
            Availability::Unavailable(format!("{SANDBOX_EXEC} not found"))
        }
    }

    fn describe(&self, plan: &Plan) -> String {
        // The sandbox root is otherwise only inferrable by reading the rules.
        // Prepended for display only, never written into the profile that
        // `sandbox-exec` parses — a profile that fails to compile would break
        // every Bash command, which is far too much risk for a comment.
        format!(
            "; sandbox root: {}\n{}",
            plan.sandbox_root.display(),
            render(plan)
        )
    }

    fn confine(&self, plan: &Plan, command: &str, session_dir: &Path) -> Result<String> {
        if !session_dir.is_dir() {
            anyhow::bail!(
                "session dir {} does not exist; the launching wtclaude process is gone",
                session_dir.display()
            );
        }
        let profile = session_dir.join(unique_profile_name("sb"));
        std::fs::write(&profile, render(plan))
            .with_context(|| format!("writing sandbox profile {}", profile.display()))?;
        Ok(format!(
            "{SANDBOX_EXEC} -f {} sh -c {}",
            shell_single_quote(&profile.to_string_lossy()),
            shell_single_quote(command)
        ))
    }

    fn notice(&self) -> &'static str {
        "Avoid heredocs (e.g. `cat <<'EOF' ... EOF`) nested inside a `$(...)` command \
         substitution inside double quotes — the common `git commit -m \"$(cat <<'EOF' ... \
         EOF)\"` idiom included. Apple's bash 3.2 (which backs both /bin/sh and /bin/bash on \
         macOS, and is what this sandbox's `sh -c` wrapper runs) has a real parsing bug there: \
         depending on the exact single-quote/backslash content of the heredoc body, it can \
         miscount quote nesting and fail with 'unexpected EOF while looking for matching' or a \
         syntax error, even though the same text is valid POSIX shell and works fine in zsh. \
         This is unrelated to sandboxing and reproduces with no sandbox involved at all. Prefer \
         writing multi-line content (like a commit message) to a temp file and using it \
         directly, e.g. `git commit -F /tmp/msg.txt`, instead of the heredoc-in-command-\
         substitution pattern."
    }

    fn heredocs_blocked(&self) -> bool {
        true
    }

    fn privilege_escalation_blocked(&self) -> bool {
        false
    }

    fn unhonored(&self, _plan: &Plan) -> Vec<String> {
        // Seatbelt expresses every plan entry, globs included.
        Vec::new()
    }
}

/// A filename unique within a session dir. Bash calls run in parallel, so a
/// single rewritten path would be truncated mid-read by a concurrent call.
fn unique_profile_name(extension: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("profile-{}-{nanos}.{extension}", std::process::id())
}

fn render(plan: &Plan) -> String {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        "(deny file-write* (subpath \"/\"))".to_string(),
        "(allow file-write* (literal \"/dev/null\"))".to_string(),
        // osascript / AppleEvents support
        "(allow process-exec* (literal \"/usr/bin/osascript\"))".to_string(),
        "(allow appleevent-send)".to_string(),
    ];

    for p in &plan.write_subtrees {
        lines.push(format!(
            "(allow file-write* (subpath \"{}\"))",
            p.to_string_lossy()
        ));
    }

    for pattern in &plan.write_globs {
        // Resolve the pattern's static prefix the same way literal entries
        // are resolved: Seatbelt evaluates rules against the kernel-resolved,
        // symlink-free path, so a glob rooted under a symlinked prefix (e.g.
        // anything under /tmp) would otherwise compile into a regex that can
        // never match — silently permitting nothing, the exact bug class this
        // glob support exists to fix.
        let resolved_pattern = crate::config::resolve_glob_prefix(pattern);
        lines.push(format!(
            "(allow file-write* (regex #\"{}\"))",
            sbpl_string_escape(&glob_to_regex(&resolved_pattern))
        ));
    }

    for p in &plan.sockets {
        let s = p.to_string_lossy();
        lines.push(format!("(allow file-write* (subpath \"{s}\"))"));
        lines.push(format!("(allow network-outbound (subpath \"{s}\"))"));
        lines.push(format!("(allow network-bind    (subpath \"{s}\"))"));
    }

    lines.join("\n") + "\n"
}

/// Translate a shell-style glob (`*` only, usable anywhere in the pattern)
/// into a whole-string-anchored SBPL regex. `subpath` matches by path
/// component, not by glob/wildcard, so it can't express `.tmpXXXX`-style
/// siblings of a file — entries like `~/.claude.json*` need `regex` instead.
fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '.' | '^' | '$' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    out
}

/// Escapes a string for embedding inside an SBPL `#"..."` string literal.
/// `glob_to_regex`'s output can itself contain backslashes (from escaping
/// regex metacharacters); those and any literal `"` both need escaping here
/// so the emitted profile stays valid SBPL rather than truncating the string
/// literal or failing to parse.
fn sbpl_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_to_regex_translates_trailing_star_to_anchored_dotstar() {
        // Regression test: `~/.claude.json*` in wtclaude.yml must produce a
        // regex that matches the base file, not just literal-`*` paths that
        // never exist (the bug that made the entry a silent no-op).
        assert_eq!(
            glob_to_regex("/Users/earlye/.claude.json*"),
            r"^/Users/earlye/\.claude\.json.*$"
        );
    }

    #[test]
    fn glob_to_regex_escapes_regex_metacharacters() {
        assert_eq!(glob_to_regex("/a/b.c*"), r"^/a/b\.c.*$");
    }

    #[test]
    fn glob_to_regex_escapes_every_listed_metacharacter() {
        assert_eq!(
            glob_to_regex(r"a.b^c$d+e?f(g)h[i]j{k}l|m\n"),
            r"^a\.b\^c\$d\+e\?f\(g\)h\[i\]j\{k\}l\|m\\n$"
        );
    }

    #[test]
    fn glob_to_regex_handles_leading_and_middle_wildcards() {
        assert_eq!(glob_to_regex("*.log"), r"^.*\.log$");
        assert_eq!(glob_to_regex("/a/foo*bar"), r"^/a/foo.*bar$");
    }

    #[test]
    fn sbpl_string_escape_escapes_backslash_and_quote() {
        assert_eq!(sbpl_string_escape(r#"a\b"c"#), r#"a\\b\"c"#);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn render_emits_a_subpath_rule_per_writable_subtree_and_denies_the_root() {
        use std::path::PathBuf;
        let plan = Plan {
            sandbox_root: PathBuf::from("/Users/x/wt"),
            write_subtrees: vec![PathBuf::from("/Users/x/wt"), PathBuf::from("/private/tmp")],
            write_globs: vec!["/Users/x/.claude.json*".to_string()],
            sockets: vec![],
        };
        let profile = render(&plan);
        assert!(profile.contains("(deny file-write* (subpath \"/\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/Users/x/wt\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/private/tmp\"))"));
        assert!(
            profile.contains(r#"(allow file-write* (regex #"^/Users/x/\.claude\.json.*$"))"#),
            "glob entry should compile to an anchored regex: {profile}"
        );
    }
}
