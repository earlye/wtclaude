use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

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
        if let Some(response) = wrap_bash_in_sandbox(&payload)? {
            println!("{}", serde_json::to_string(&response)?);
        }
        return Ok(());
    }

    let allowlist = crate::config::load_user()
        .map(|c| c.allowlist)
        .unwrap_or_default();

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

fn wrap_bash_in_sandbox(payload: &HookPayload) -> Result<Option<HookResponse>> {
    let command = match payload.tool_input.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Ok(Some(deny_bash(
                "Bash tool_input missing 'command'; cannot sandbox. Blocked.",
            )));
        }
    };

    if contains_heredoc(command) {
        return Ok(Some(deny_bash(HEREDOC_DENY_MESSAGE)));
    }

    let sbpl_path = std::env::var("WTCLAUDE_SBPL").unwrap_or_default();
    if sbpl_path.is_empty() {
        return Ok(Some(deny_bash(
            "WTCLAUDE_SBPL is not set; sandbox policy unavailable. Bash is blocked.",
        )));
    }
    if !std::path::Path::new(&sbpl_path).exists() {
        if let Err(e) = regenerate_sbpl_policy(&sbpl_path) {
            return Ok(Some(deny_bash(&format!(
                "Sandbox policy file missing and could not be regenerated ({}): {}. Bash is blocked.",
                sbpl_path, e
            ))));
        }
    }

    let wrapped = format!(
        "/usr/bin/sandbox-exec -f {} sh -c {}",
        sbpl_path,
        shell_single_quote(command)
    );

    Ok(Some(HookResponse {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse".to_string(),
            permission_decision: None,
            permission_decision_reason: None,
            updated_input: Some(serde_json::json!({ "command": wrapped })),
        },
    }))
}

fn regenerate_sbpl_policy(sbpl_path: &str) -> anyhow::Result<()> {
    let sandbox = std::env::var("WTCLAUDE_SANDBOX")
        .map_err(|_| anyhow::anyhow!("WTCLAUDE_SANDBOX not set"))?;
    let repo_root = std::env::var("WTCLAUDE_REPO_ROOT")
        .map_err(|_| anyhow::anyhow!("WTCLAUDE_REPO_ROOT not set"))?;
    let policy = crate::launch::generate_sbpl_policy(
        std::path::Path::new(&sandbox),
        std::path::Path::new(&repo_root),
    )?;
    std::fs::write(sbpl_path, policy)
        .map_err(|e| anyhow::anyhow!("writing regenerated policy: {e}"))?;
    Ok(())
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

fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
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
        let allowed = normalize_path(Path::new(&expanded), Path::new("/"));
        normalized.starts_with(&allowed)
    })
}

// Lexical path normalization that doesn't require paths to exist.
// relative_to is used as the base when path is relative and canonicalize fails.
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
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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

    // Runs `command` the way the sandbox wrapper's output is ultimately run: as
    // the text of an outer `sh -c`. This reproduces the double shell-parse from
    // production (`sandbox-exec ... sh -c '<escaped>'` invoked as one command
    // line by the harness), not just a single argv-level exec.
    fn run_via_outer_shell(command: &str) -> String {
        eprintln!("--- running via sh -c ---\n{command}\n--- end command ---");
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("failed to spawn sh");
        eprintln!(
            "--- exit status: {:?} ---\n--- stdout ---\n{}--- stderr ---\n{}--- end output ---",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "command failed (status {:?}):\ncommand: {command}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn shell_single_quote_literal_output_for_known_inputs() {
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_single_quote("''"), "''\\'''\\'''");
    }

    #[test]
    fn shell_single_quote_reconstructs_adversarial_bodies_byte_for_byte() {
        // shell_single_quote() is the only barrier between an arbitrary Bash
        // command and a second `sh -c` re-parse inside sandbox-exec. Rather
        // than executing adversarial-looking content directly (risky if a
        // real escaping bug let something run), assert that the outer
        // `sh -c '<escaped>'` reconstructs the original bytes exactly via
        // `printf '%s'`. Exact byte-for-byte reconstruction is what
        // guarantees the wrapped command behaves identically to running the
        // original directly — i.e. nothing can "escape" the outer quoting.
        let adversarial_bodies = [
            "already just plain text",
            "leading quote: 'text",
            "trailing quote: text'",
            "adjacent quotes: ''",
            "looks like a breakout attempt: '; echo INJECTED; echo '",
            "odd count: it's a 'test' of 'quoting",
        ];

        for original in adversarial_bodies {
            let escaped = shell_single_quote(original);
            let probe = format!("printf '%s' {escaped}");
            let reconstructed = run_via_outer_shell(&probe);
            assert_eq!(
                reconstructed, original,
                "failed to reconstruct: {original:?}"
            );
        }
    }

    #[test]
    fn shell_single_quote_survives_quoted_heredoc() {
        // Minimal repro: a heredoc whose quoted delimiter and body both
        // contain literal single quotes for shell_single_quote() to escape.
        let original = "cat <<'EOF'\nHello 'World'\nFrom Heredoc\nEOF";
        let escaped = shell_single_quote(original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, "Hello 'World'\nFrom Heredoc\n");
    }

    #[test]
    fn shell_single_quote_survives_real_world_commit_command() {
        // Body copied verbatim from the original bug report. Keep the
        // apostrophe in "run()'s" — it's the one embedded single quote that
        // makes this test exercise shell_single_quote()'s escaping at all.
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Moves run()'s pty/reader/writer/emulator/cols/rows locals into an App\n\
struct, with App::new() owning setup and run_loop() holding the event\n\
loop, so future state (e.g. a session tree) has somewhere to live.\n\
Also adds a term_emulator::new_emulator() factory so app.rs no longer\n\
names the concrete AlacrittyTerminalEmulator type directly.\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>\n";
        let original = format!("cat <<'EOF'\n{body}EOF");

        let escaped = shell_single_quote(&original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, body);
    }

    #[test]
    fn shell_single_quote_survives_command_substitution_around_heredoc() {
        // Structural shape matches the real failing command — a double-quoted
        // command substitution wrapping a heredoc with a single-quoted
        // delimiter: git commit -m "$(cat <<'EOF' ... EOF)". The body
        // intentionally omits the apostrophe (see
        // shell_single_quote_survives_real_world_commit_command) that
        // triggers the separate, unrelated bash 3.2 heredoc-in-$()-in-""
        // parsing bug (see the issue file) — that bug is not fixable here,
        // so this test isolates shell_single_quote()'s own correctness from
        // it. Swap `git commit -m` for `printf '%s\n'` so this runs
        // standalone without needing a git repo/staged changes.
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>";
        let original = format!("printf '%s\\n' \"$(cat <<'EOF'\n{body}\nEOF\n)\"");

        let escaped = shell_single_quote(&original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, format!("{body}\n"));
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

    #[test]
    fn wrap_bash_in_sandbox_denies_heredoc_before_touching_sbpl() {
        // No WTCLAUDE_SBPL set up here on purpose: the heredoc check must
        // reject the command before wrap_bash_in_sandbox() ever looks at the
        // sandbox policy.
        let payload = HookPayload {
            tool_name: "Bash".to_string(),
            cwd: String::new(),
            tool_input: serde_json::json!({ "command": "cat <<'EOF'\nhi\nEOF" }),
        };

        let response = wrap_bash_in_sandbox(&payload)
            .expect("wrap_bash_in_sandbox")
            .expect("Some(response) denying the command");

        assert_eq!(
            response.hook_specific_output.permission_decision.as_deref(),
            Some("deny")
        );
        assert_eq!(
            response
                .hook_specific_output
                .permission_decision_reason
                .as_deref(),
            Some(HEREDOC_DENY_MESSAGE)
        );
        assert!(response.hook_specific_output.updated_input.is_none());
    }

    // Cleans up the test-only SBPL policy file and WTCLAUDE_SBPL env var on
    // drop, including on unwind from a failed assertion — otherwise a test
    // failure would leak the temp file and leave WTCLAUDE_SBPL set for the
    // rest of the process.
    struct SbplPolicyGuard {
        path: PathBuf,
    }

    impl Drop for SbplPolicyGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            unsafe {
                std::env::remove_var("WTCLAUDE_SBPL");
            }
        }
    }

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

        // WTCLAUDE_SBPL is a process-global env var; this is the only test that
        // touches it today, so there's no cross-test race, but that'd need
        // revisiting if a second test starts setting it.
        let policy_path =
            std::env::temp_dir().join(format!("wtclaude-test-sbpl-{}.sb", std::process::id()));
        std::fs::write(&policy_path, "(version 1)\n(allow default)\n").expect("write policy");
        unsafe {
            std::env::set_var("WTCLAUDE_SBPL", &policy_path);
        }
        let _guard = SbplPolicyGuard {
            path: policy_path.clone(),
        };

        // No heredoc here (contains_heredoc() now denies those before this
        // point is ever reached) — nested command substitution inside double
        // quotes is kept to still exercise the same quoting shape through the
        // real sandbox-exec wrapping.
        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>";
        let original = format!("printf '%s\\n' \"$(printf '%s' '{body}')\"");

        let payload = HookPayload {
            tool_name: "Bash".to_string(),
            cwd: String::new(),
            tool_input: serde_json::json!({ "command": original }),
        };

        let response = wrap_bash_in_sandbox(&payload)
            .expect("wrap_bash_in_sandbox")
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

        assert_eq!(stdout, format!("{body}\n"));
    }
}
