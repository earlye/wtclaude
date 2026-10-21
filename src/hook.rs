use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use crate::sandbox;

#[derive(Deserialize)]
struct HookPayload {
    tool_name: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool_input: serde_json::Value,
}

#[derive(Serialize)]
struct HookResponse {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput,
}

#[derive(Serialize)]
struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    hook_event_name: String,
    #[serde(rename = "permissionDecision", skip_serializing_if = "Option::is_none")]
    permission_decision: Option<String>,
    #[serde(
        rename = "permissionDecisionReason",
        skip_serializing_if = "Option::is_none"
    )]
    permission_decision_reason: Option<String>,
    #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
    updated_input: Option<serde_json::Value>,
}

pub fn run() -> Result<()> {
    let sandbox = std::env::var("WTCLAUDE_SANDBOX").unwrap_or_default();
    if sandbox.is_empty() {
        return Ok(());
    }

    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    let payload: HookPayload = match serde_json::from_str(&input) {
        Ok(p) => p,
        Err(e) => {
            let r = deny_bash(&format!("hook: bad payload: {e}"));
            println!("{}", serde_json::to_string(&r)?);
            return Ok(());
        }
    };

    if payload.tool_name == "Bash" {
        if let Some(response) = wrap_bash_in_sandbox(&payload, Path::new(&sandbox))? {
            println!("{}", serde_json::to_string(&response)?);
        }
        return Ok(());
    }

    let allowlist = crate::config::load_user()
        .map(|c| c.allowlist)
        .unwrap_or_else(|e| {
            // load_user() can now fail on a bad allowlist entry (e.g. an
            // unanchored wildcard), not just I/O/parse errors — surface it
            // instead of silently degrading to an empty allowlist, which
            // would otherwise deny every allowlisted write with no clue why.
            eprintln!(
                "wtclaude: warning: failed to load allowlist ({e}); \
                 proceeding with an empty allowlist"
            );
            Vec::new()
        });

    if let Some(path_str) = extract_file_path(&payload)
        && !is_within_sandbox(&path_str, &payload.cwd, &sandbox, &allowlist)
    {
        let reason = format!(
            "SANDBOX VIOLATION: '{}' is outside the allowed worktree. \
                All file writes must stay within: {}",
            path_str, sandbox
        );
        let response = HookResponse {
            hook_specific_output: HookSpecificOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: Some("deny".to_string()),
                permission_decision_reason: Some(reason),
                updated_input: None,
            },
        };
        println!("{}", serde_json::to_string(&response)?);
    }

    Ok(())
}

const HEREDOC_DENY_MESSAGE: &str = "heredoc is blocked because of the bug warned about in the \
     system prompt: heredocs (even non-nested ones) are blocked outright because reliably \
     distinguishing safe from buggy shapes isn't possible without re-implementing shell \
     quote-parsing. Use a temp file instead, e.g. `git commit -F /tmp/msg.txt`, or write \
     multi-line content with the Write tool.";

/// Rewrites a Bash command into its sandboxed form, or denies it.
///
/// What is forbidden depends on the backend, so that is resolved first: the
/// heredoc ban belongs to Seatbelt's bash 3.2 and privilege escalation is
/// impossible only under Landlock.
fn wrap_bash_in_sandbox(
    payload: &HookPayload,
    sandbox_root: &Path,
) -> Result<Option<HookResponse>> {
    // Re-resolved on every call rather than trusted from launch: this is the
    // backstop that catches a backend which has stopped being able to enforce
    // anything mid-session.
    let backend = match active_backend() {
        Ok(backend) => backend,
        Err(e) => {
            return Ok(Some(deny_bash(&format!(
                "{e}. Bash is blocked because nothing would constrain it."
            ))));
        }
    };

    let session_dir = match std::env::var("WTCLAUDE_SESSION_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            return Ok(Some(deny_bash(
                "WTCLAUDE_SESSION_DIR is not set, so this session's sandbox scratch \
                 directory is unknown. Bash is blocked.",
            )));
        }
    };

    sandboxed_bash_response(payload, backend.as_ref(), &session_dir, sandbox_root)
}

/// The decision itself, with the backend and session dir already resolved.
/// Separate from `wrap_bash_in_sandbox` so tests can supply a backend rather
/// than mutating process-global environment variables.
fn sandboxed_bash_response(
    payload: &HookPayload,
    backend: &dyn sandbox::Backend,
    session_dir: &Path,
    sandbox_root: &Path,
) -> Result<Option<HookResponse>> {
    let command = match payload.tool_input.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Ok(Some(deny_bash(
                "Bash tool_input missing 'command'; cannot sandbox. Blocked.",
            )));
        }
    };

    if backend.heredocs_blocked() && contains_heredoc(command) {
        return Ok(Some(deny_bash(HEREDOC_DENY_MESSAGE)));
    }

    if backend.privilege_escalation_blocked()
        && let Some(name) = privilege_escalation_command(command)
    {
        return Ok(Some(deny_bash(&privilege_escalation_deny_message(name))));
    }

    let repo_root = std::env::var("WTCLAUDE_REPO_ROOT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    // Derived per call, not cached at launch: on Linux a rule needs a path
    // that already exists, so a cache dir created earlier in the session only
    // becomes writable if the plan is recomputed. It also means a broken
    // wtclaude.yml denies Bash rather than silently proceeding.
    let plan = match sandbox::plan(sandbox_root, repo_root.as_deref()) {
        Ok(plan) => plan,
        Err(e) => {
            return Ok(Some(deny_bash(&format!(
                "could not work out what this session may write to ({e:#}). Bash is blocked."
            ))));
        }
    };

    match backend.confine(&plan, command, session_dir) {
        Ok(wrapped) => Ok(Some(HookResponse {
            hook_specific_output: HookSpecificOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: None,
                permission_decision_reason: None,
                updated_input: Some(serde_json::json!({ "command": wrapped })),
            },
        })),
        Err(e) => Ok(Some(deny_bash(&format!(
            "could not sandbox this command ({e:#}). Bash is blocked."
        )))),
    }
}

/// The backend the launching wtclaude process chose, named in
/// `WTCLAUDE_BACKEND`.
///
/// Deliberately does not fall back to a preference of its own: if the
/// launcher didn't say, the hook must not guess, or `--test-sandbox-breakage
/// hide` (and any real bug with the same shape) would silently run commands
/// unconstrained.
fn active_backend() -> Result<Box<dyn sandbox::Backend>> {
    let name = std::env::var("WTCLAUDE_BACKEND").unwrap_or_default();
    if name.is_empty() {
        bail!("WTCLAUDE_BACKEND is not set, so the active sandbox backend is unknown");
    }
    let (backend, _caveat) = sandbox::select(&name)?;
    Ok(backend)
}

const PRIVILEGE_COMMANDS: &[&str] = &["sudo", "doas", "pkexec", "su"];

/// Finds a privilege-escalation command in *command position* — the start of
/// the line, or straight after a `;`, `&&`, `||`, `|`, `&` or newline —
/// ignoring `VAR=value` prefixes and any leading path.
///
/// Matching command position rather than any occurrence keeps the common
/// false positive away: `git commit -m "document sudo"` has `echo`/`git` in
/// command position, so it passes. It is not airtight, since quoting isn't
/// tracked — `echo "step 1; sudo dnf install x"` does trip it — and the deny
/// message says so, in the same spirit as `contains_heredoc`.
fn privilege_escalation_command(command: &str) -> Option<&'static str> {
    for segment in command.split(['\n', ';', '&', '|']) {
        let segment =
            segment.trim_start_matches(|c: char| c.is_whitespace() || c == '(' || c == '{');
        let Some(word) = segment.split_whitespace().find(|w| !is_env_assignment(w)) else {
            continue;
        };
        let name = word.rsplit('/').next().unwrap_or(word);
        if let Some(found) = PRIVILEGE_COMMANDS.iter().find(|c| **c == name) {
            return Some(found);
        }
    }
    None
}

/// Whether `word` is a `VAR=value` prefix rather than the command itself.
fn is_env_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn privilege_escalation_deny_message(name: &str) -> String {
    format!(
        "`{name}` cannot work in this sandbox — as the system prompt said, privilege \
         escalation is impossible here. The kernel requires the NO_NEW_PRIVS flag before an \
         unprivileged process may sandbox itself, and that same flag stops setuid binaries \
         from elevating, so `{name}` would fail no matter how it is invoked. It is refused \
         here rather than left to fail with a confusing kernel message. If this task needs \
         root — installing a system package, say — ask the user to do it outside the sandbox. \
         If `{name}` only appeared inside quoted text, rewrite the command so it isn't the \
         first word after a `;`, `&&`, `||` or newline."
    )
}

fn deny_bash(reason: &str) -> HookResponse {
    HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse".to_string(),
            permission_decision: Some("deny".to_string()),
            permission_decision_reason: Some(reason.to_string()),
            updated_input: None,
        },
    }
}

/// Detects heredoc syntax (`<<`/`<<-`) without tracking shell quoting: a
/// stray `<<` (e.g. a bitshift in an embedded snippet) only rarely has a
/// later line matching its "delimiter" exactly, so requiring both the
/// opener and a matching closing line keeps false positives rare, not
/// impossible — e.g. a coincidental line consisting solely of a numeric
/// operand (`"n=1<<2\n2"`) would still match.
fn contains_heredoc(command: &str) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i + 1 < len {
        if bytes[i] != b'<' || bytes[i + 1] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        if j < len && bytes[j] == b'<' {
            // `<<<` is a herestring, not a heredoc.
            i = j + 1;
            continue;
        }
        let dash = j < len && bytes[j] == b'-';
        if dash {
            j += 1;
        }
        while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        let quote = match bytes.get(j) {
            Some(b'\'') | Some(b'"') => {
                let q = bytes[j];
                j += 1;
                Some(q)
            }
            Some(b'\\') => {
                j += 1;
                None
            }
            _ => None,
        };
        let ident_start = j;
        while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j == ident_start {
            i += 2;
            continue;
        }
        let ident_end = j;
        // A missing/mismatched closing quote (e.g. `<<'EOF` with no closing
        // `'`) doesn't disqualify the match — detection doesn't depend on
        // the shell's own quoting being well-formed, only on the opener and
        // a later matching close line.
        if let Some(q) = quote
            && bytes.get(j) == Some(&q)
        {
            j += 1;
        }
        let delimiter = &command[ident_start..ident_end];
        if has_matching_close_line(&command[j..], delimiter, dash) {
            return true;
        }
        i = j;
    }
    false
}

/// `rest` is everything after a heredoc opener's delimiter, starting on the
/// opener's own line. A real heredoc requires `delimiter` alone on one of
/// the following lines (leading tabs allowed only for `<<-`).
fn has_matching_close_line(rest: &str, delimiter: &str, dash: bool) -> bool {
    let Some((_opener_line_tail, following_lines)) = rest.split_once('\n') else {
        return false;
    };
    following_lines.lines().any(|line| {
        let candidate = if dash {
            line.trim_start_matches('\t')
        } else {
            line
        };
        candidate == delimiter
    })
}

fn extract_file_path(payload: &HookPayload) -> Option<String> {
    let input = &payload.tool_input;
    match payload.tool_name.as_str() {
        "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        "NotebookEdit" => input
            .get("notebook_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        _ => None,
    }
}

fn is_within_sandbox(file_path: &str, cwd: &str, sandbox: &str, allowlist: &[String]) -> bool {
    let path = if Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        Path::new(cwd).join(file_path)
    };

    let normalized = normalize_path(&path, Path::new(cwd));
    let sandbox_normalized = normalize_path(Path::new(sandbox), Path::new("/"));

    if normalized.starts_with(&sandbox_normalized) {
        return true;
    }

    let home = std::env::var("HOME").unwrap_or_default();
    allowlist.iter().any(|entry| {
        let expanded = entry.replace('~', &home);
        if expanded.trim().is_empty() {
            // An empty entry would normalize to "/" (normalize_path falls
            // back to its relative-to base, "/", when given an empty,
            // non-absolute path), matching every absolute path.
            return false;
        }
        if expanded.contains('*') {
            // Glob entries (e.g. `~/.claude.json*`, covering the file plus
            // its atomic-write tmp siblings) can't be canonicalized whole —
            // no real path literally contains a `*` — and component-wise
            // starts_with can't express a wildcard either. Match the
            // pattern's resolved static prefix against the normalized path
            // string instead. Resolving the prefix matters because
            // `normalized` above gets symlink-resolved by `canonicalize()`
            // whenever the target file already exists (e.g. anything under
            // /tmp, which is a symlink to /private/tmp on macOS) — without
            // this, the pattern and the path it's meant to match diverge and
            // silently never match, the same failure mode this glob support
            // exists to fix.
            let resolved_pattern = crate::config::resolve_glob_prefix(&expanded);
            return glob_match(&resolved_pattern, &normalized.to_string_lossy());
        }
        let allowed = normalize_path(Path::new(&expanded), Path::new("/"));
        normalized.starts_with(&allowed)
    })
}

// Simple shell-style glob match supporting only the `*` wildcard (matches
// any run of characters, including none). Used for allowlist entries like
// `~/.claude.json*` that can't be expressed as a literal path component.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !text[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            return text[pos..].ends_with(part);
        } else {
            match text[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

// Lexical path normalization that doesn't require paths to exist.
// relative_to is used as the base when path is relative and canonicalize fails.
//
// Once the path is lexically joined/collapsed, its longest existing ancestor
// (if any) is canonicalized too — e.g. a not-yet-created file under /tmp
// still resolves through the /tmp -> /private/tmp symlink on macOS. This
// keeps target-path resolution consistent with `config::resolve_glob_prefix`,
// which does the same for glob allowlist patterns; comparing an unresolved
// pattern against a resolved path (or vice versa) would silently never
// match.
fn normalize_path(path: &Path, relative_to: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut result = if path.is_absolute() {
        PathBuf::new()
    } else {
        relative_to.to_path_buf()
    };
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            c => result.push(c),
        }
    }
    crate::config::resolve_existing_prefix(&result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use crate::sandbox::tests::run_via_outer_shell;

    #[test]
    fn is_within_sandbox_allows_path_within_sandbox() {
        assert!(is_within_sandbox(
            "/tmp/wtclaude-test-sandbox/file.txt",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &[]
        ));
    }

    #[test]
    fn is_within_sandbox_denies_path_outside_sandbox_and_allowlist() {
        assert!(!is_within_sandbox(
            "/etc/passwd",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &[]
        ));
    }

    #[test]
    fn is_within_sandbox_allows_path_within_allowlist_entry() {
        let allowlist = vec!["/tmp/wtclaude-test-allowed".to_string()];
        assert!(is_within_sandbox(
            "/tmp/wtclaude-test-allowed/nested/file.txt",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_denies_when_no_allowlist_entry_matches() {
        let allowlist = vec![
            "/tmp/wtclaude-test-allowed-one".to_string(),
            "/tmp/wtclaude-test-allowed-two".to_string(),
        ];
        assert!(!is_within_sandbox(
            "/etc/passwd",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_treats_allowlist_entry_as_a_path_component_not_a_string_prefix() {
        // A lexical string prefix match would wrongly let "/tmp/allow" permit
        // "/tmp/allowed-other"; PathBuf::starts_with is component-wise, so it
        // must not.
        let allowlist = vec!["/tmp/wtclaude-test-allow".to_string()];
        assert!(!is_within_sandbox(
            "/tmp/wtclaude-test-allow-other/file.txt",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_ignores_empty_allowlist_entries() {
        // A blank line or trailing comma in wtclaude.yml's allowlist yields an
        // empty string. normalize_path() on an empty, non-absolute path falls
        // back to its relative-to base ("/"), which would match every
        // absolute path if not filtered out explicitly.
        let allowlist = vec!["".to_string()];
        assert!(!is_within_sandbox(
            "/etc/passwd",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_expands_tilde_in_allowlist_entries() {
        // Both sides of the comparison use a path that doesn't exist on disk,
        // so normalize_path()'s canonicalize() fails for both and falls back
        // to identical lexical joining — the check doesn't depend on the
        // real $HOME's filesystem contents.
        let home = std::env::var("HOME").expect("HOME must be set");
        let allowlist = vec!["~/wtclaude-nonexistent-allowlist-test".to_string()];
        let path = format!("{home}/wtclaude-nonexistent-allowlist-test/nested/file.md");
        assert!(is_within_sandbox(
            &path,
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_allows_glob_allowlist_entry_for_the_base_file() {
        // Regression test: `~/.claude.json*` style entries must permit
        // writes to `~/.claude.json` itself, not just its tmp siblings.
        let allowlist = vec!["/tmp/wtclaude-test-glob.json*".to_string()];
        assert!(is_within_sandbox(
            "/tmp/wtclaude-test-glob.json",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_allows_glob_allowlist_entry_for_tmp_siblings() {
        // Atomic config writers create a `<file>.tmpXXXX` sibling before
        // renaming it over the real file — the glob must cover that too.
        let allowlist = vec!["/tmp/wtclaude-test-glob.json*".to_string()];
        assert!(is_within_sandbox(
            "/tmp/wtclaude-test-glob.json.tmp1234",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_glob_allowlist_entry_does_not_match_unrelated_path() {
        let allowlist = vec!["/tmp/wtclaude-test-glob.json*".to_string()];
        assert!(!is_within_sandbox(
            "/tmp/wtclaude-test-glob-other.txt",
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist
        ));
    }

    #[test]
    fn is_within_sandbox_allows_glob_allowlist_entry_when_target_file_already_exists() {
        // Regression test: the earlier three glob tests all use paths that
        // don't exist on disk, so normalize_path()'s canonicalize() fails
        // for the target too and both sides fall back to identical lexical
        // joining — masking the symlink-resolution asymmetry that occurs
        // once a target file is real. /tmp is a symlink to /private/tmp on
        // macOS, so creating a real file here and matching it against an
        // un-canonicalized glob pattern exercises exactly that asymmetry.
        let path = format!("/tmp/wtclaude-test-glob-exists-{}.json", std::process::id());
        std::fs::write(&path, "{}").expect("write test fixture");
        let allowlist = vec![format!("{path}*")];
        let result = is_within_sandbox(
            &path,
            "/tmp/wtclaude-test-sandbox",
            "/tmp/wtclaude-test-sandbox",
            &allowlist,
        );
        let _ = std::fs::remove_file(&path);
        assert!(result);
    }

    #[test]
    fn glob_match_matches_prefix_and_siblings() {
        assert!(glob_match("/a/.claude.json*", "/a/.claude.json"));
        assert!(glob_match("/a/.claude.json*", "/a/.claude.json.tmp1234"));
        assert!(!glob_match("/a/.claude.json*", "/a/.claude-other"));
    }

    #[test]
    fn glob_match_matches_wildcard_in_the_middle() {
        assert!(glob_match("/a/foo*bar", "/a/fooXXbar"));
        assert!(!glob_match("/a/foo*bar", "/a/fooXXbaz"));
    }

    #[test]
    fn glob_match_matches_leading_wildcard() {
        assert!(glob_match("*.log", "/a/b/app.log"));
        assert!(!glob_match("*.log", "/a/b/app.txt"));
    }

    #[test]
    fn glob_match_matches_multiple_wildcards() {
        assert!(glob_match("/a/*/b*c", "/a/x/bYYc"));
        assert!(!glob_match("/a/*/b*c", "/a/x/bYYd"));
    }

    #[test]
    fn normalize_path_resolves_symlinked_ancestor_for_a_nonexistent_target() {
        // Regression test for making target-path resolution consistent with
        // glob-pattern prefix resolution (config::resolve_glob_prefix),
        // independent of the glob machinery: a not-yet-existing path under
        // /tmp must resolve through whatever /tmp really is — a symlink to
        // /private/tmp on macOS, a real directory on Linux — even though the
        // exact target doesn't exist.
        let tmp = std::fs::canonicalize("/tmp").unwrap();
        let resolved = normalize_path(
            Path::new("/tmp/wtclaude-test-normalize-path-nonexistent-target"),
            Path::new("/"),
        );
        assert_eq!(
            resolved,
            tmp.join("wtclaude-test-normalize-path-nonexistent-target")
        );
    }

    #[test]
    fn contains_heredoc_detects_plain_heredoc() {
        assert!(contains_heredoc("cat <<EOF\nhello\nEOF"));
        assert!(contains_heredoc("cat <<'EOF'\nhello\nEOF"));
        assert!(contains_heredoc("cat <<\"EOF\"\nhello\nEOF"));
        assert!(contains_heredoc("cat <<\\EOF\nhello\nEOF"));
        assert!(contains_heredoc("cat <<-EOF\n\thello\n\tEOF"));
    }

    #[test]
    fn contains_heredoc_detects_nested_command_substitution_shape() {
        // The exact shape from the resolved bash-3.2 parsing bug report:
        // git commit -m "$(cat <<'EOF' ... EOF)"
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>";
        let command = format!("git commit -m \"$(cat <<'EOF'\n{body}\nEOF\n)\"");
        assert!(contains_heredoc(&command));
    }

    #[test]
    fn contains_heredoc_ignores_bitshift_and_herestring() {
        // No newline at all, so there's no possible matching close line.
        assert!(!contains_heredoc("python -c \"print(1<<2)\""));
        // Herestring (`<<<`), not a heredoc.
        assert!(!contains_heredoc("cat <<< \"just a string\""));
        // `<<` with a delimiter but no line consisting solely of it.
        assert!(!contains_heredoc("cat <<EOF\nhello\nnot the delimiter"));
    }

    #[test]
    fn contains_heredoc_detects_unterminated_quote_around_delimiter() {
        // Missing closing quote right after the delimiter — still a real
        // heredoc opener as far as detection is concerned.
        assert!(contains_heredoc("cat <<'EOF\nhi\nEOF"));
        assert!(contains_heredoc("cat <<\"EOF\nhi\nEOF"));
    }

    #[test]
    fn contains_heredoc_requires_exact_close_line_for_plain_heredoc() {
        // Only `<<-` allows the close line to be tab-indented; plain `<<`
        // requires an exact match.
        assert!(!contains_heredoc("cat <<EOF\n\tEOF"));
        assert!(contains_heredoc("cat <<-EOF\n\tEOF"));
    }

    fn bash_payload(command: &str) -> HookPayload {
        HookPayload {
            tool_name: "Bash".to_string(),
            cwd: String::new(),
            tool_input: serde_json::json!({ "command": command }),
        }
    }

    fn deny_reason(response: Option<HookResponse>) -> String {
        let out = response.expect("expected a response").hook_specific_output;
        assert_eq!(out.permission_decision.as_deref(), Some("deny"));
        assert!(out.updated_input.is_none());
        out.permission_decision_reason.expect("a reason")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn heredocs_are_denied_under_seatbelt() {
        // The ban belongs to Apple's bash 3.2, so it is asserted against the
        // backend that has that shell rather than as a property of the hook.
        let reason = deny_reason(
            sandboxed_bash_response(
                &bash_payload("cat <<'EOF'\nhi\nEOF"),
                &crate::sandbox::seatbelt::Seatbelt,
                Path::new("/tmp"),
                Path::new("/tmp"),
            )
            .expect("sandboxed_bash_response"),
        );
        assert_eq!(reason, HEREDOC_DENY_MESSAGE);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn heredocs_are_allowed_under_landlock() {
        // Linux bash parses the shape that breaks bash 3.2, so the same
        // command must be wrapped rather than refused.
        let dir =
            std::env::temp_dir().join(format!("wtclaude-test-heredoc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let response = sandboxed_bash_response(
            &bash_payload("cat <<'EOF'\nhi\nEOF"),
            &crate::sandbox::landlock::Landlock,
            &dir,
            &dir,
        )
        .expect("sandboxed_bash_response")
        .expect("a response");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(response.hook_specific_output.permission_decision.is_none());
        assert!(response.hook_specific_output.updated_input.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn privilege_escalation_is_denied_with_a_reminder_under_landlock() {
        let reason = deny_reason(
            sandboxed_bash_response(
                &bash_payload("sudo dnf install -y jq"),
                &crate::sandbox::landlock::Landlock,
                Path::new("/tmp"),
                Path::new("/tmp"),
            )
            .expect("sandboxed_bash_response"),
        );
        assert!(
            reason.contains("as the system prompt said"),
            "reason: {reason}"
        );
        assert!(reason.contains("NO_NEW_PRIVS"), "reason: {reason}");
    }

    #[test]
    fn privilege_escalation_command_matches_only_command_position() {
        assert_eq!(
            privilege_escalation_command("sudo dnf install x"),
            Some("sudo")
        );
        assert_eq!(privilege_escalation_command("  sudo -A true"), Some("sudo"));
        assert_eq!(
            privilege_escalation_command("/usr/bin/sudo true"),
            Some("sudo")
        );
        assert_eq!(
            privilege_escalation_command("SUDO_ASKPASS=/x sudo -A true"),
            Some("sudo")
        );
        assert_eq!(
            privilege_escalation_command("make && sudo make install"),
            Some("sudo")
        );
        assert_eq!(
            privilege_escalation_command("true; pkexec id"),
            Some("pkexec")
        );
        assert_eq!(privilege_escalation_command("cat f | su -"), Some("su"));

        // The false positive this guards against: `sudo` as an argument or
        // inside quoted prose, which is ordinary when writing documentation.
        assert_eq!(
            privilege_escalation_command("git commit -m 'document sudo'"),
            None
        );
        assert_eq!(
            privilege_escalation_command("echo 'run sudo dnf install x' > README"),
            None
        );
        assert_eq!(privilege_escalation_command("grep -rn sudo docs/"), None);
        assert_eq!(privilege_escalation_command("echo pseudonym"), None);
        assert_eq!(privilege_escalation_command("./sudoku"), None);
    }

    #[test]
    fn is_env_assignment_accepts_only_shell_variable_prefixes() {
        assert!(is_env_assignment("FOO=bar"));
        assert!(is_env_assignment("_x=1"));
        assert!(!is_env_assignment("1FOO=bar"));
        assert!(!is_env_assignment("--flag=value"));
        assert!(!is_env_assignment("plain"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "sandbox-exec cannot nest inside an already-sandboxed process; run from a terminal outside a wtclaude sandbox"]
    fn wrap_bash_in_sandbox_survives_real_sandbox_exec() {
        // Exercises the actual production path — wrap_bash_in_sandbox() building
        // "/usr/bin/sandbox-exec -f <policy> sh -c '<escaped>'" — instead of just
        // shell_single_quote() in isolation, using a wide-open policy so the
        // sandbox itself can't be the reason anything fails.
        //
        // macOS refuses sandbox_apply from within an already-sandboxed process,
        // so this can't pass while running inside a wtclaude sandbox itself
        // (e.g. `cargo test` run by Claude Code in a wtclaude-managed session).
        // #[ignore] keeps a plain `cargo test` green; if this is force-run via
        // `--ignored` while still sandboxed, fail loudly rather than silently
        // passing, so the misuse is obvious.
        assert!(
            std::env::var("WTCLAUDE_SANDBOX").is_err(),
            "WTCLAUDE_SANDBOX is set: sandbox-exec would nest and always fail. \
             Run this test from a terminal outside a wtclaude sandbox."
        );

        let session_dir =
            std::env::temp_dir().join(format!("wtclaude-test-session-{}", std::process::id()));
        std::fs::create_dir_all(&session_dir).expect("create session dir");

        // No heredoc here (contains_heredoc() now denies those before this
        // point is ever reached) — nested command substitution inside double
        // quotes is kept to still exercise the same quoting shape through the
        // real sandbox-exec wrapping.
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>";
        let original = format!("printf '%s\\n' \"$(printf '%s' '{body}')\"");

        let response = sandboxed_bash_response(
            &bash_payload(&original),
            &crate::sandbox::seatbelt::Seatbelt,
            &session_dir,
            Path::new("/tmp"),
        )
        .expect("sandboxed_bash_response")
        .expect("Some(response) for a valid Bash command");
        let wrapped = response
            .hook_specific_output
            .updated_input
            .expect("updatedInput")
            .get("command")
            .and_then(|v| v.as_str())
            .expect("command string")
            .to_string();

        let stdout = run_via_outer_shell(&wrapped);
        let _ = std::fs::remove_dir_all(&session_dir);

        assert_eq!(stdout, format!("{body}\n"));
    }
}
