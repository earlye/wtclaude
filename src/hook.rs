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

    if let Some(path_str) = extract_file_path(&payload)
        && !is_within_sandbox(&path_str, &payload.cwd, &sandbox)
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

fn wrap_bash_in_sandbox(payload: &HookPayload) -> Result<Option<HookResponse>> {
    let command = match payload.tool_input.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Ok(Some(deny_bash(
                "Bash tool_input missing 'command'; cannot sandbox. Blocked.",
            )));
        }
    };

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

fn is_within_sandbox(file_path: &str, cwd: &str, sandbox: &str) -> bool {
    let path = if Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        Path::new(cwd).join(file_path)
    };

    let normalized = normalize_path(&path, Path::new(cwd));
    let sandbox_normalized = normalize_path(Path::new(sandbox), Path::new("/"));

    normalized.starts_with(&sandbox_normalized)
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
    fn shell_single_quote_survives_quoted_heredoc() {
        let original = "cat <<'EOF'\nHello 'World'\nFrom Heredoc\nEOF";
        let escaped = shell_single_quote(original);
        let outer_cmd = format!("sh -c {escaped}");

        let stdout = run_via_outer_shell(&outer_cmd);

        assert_eq!(stdout, "Hello 'World'\nFrom Heredoc\n");
    }

    #[test]
    fn shell_single_quote_survives_real_world_commit_command() {
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
        // Mirrors the exact shape of the failing real-world command:
        // git commit -m "$(cat <<'EOF' ... EOF)" — a double-quoted command
        // substitution wrapping a heredoc with a single-quoted delimiter.
        // Swap `git commit -m` for `printf '%s\n'` so this runs standalone
        // without needing a git repo/staged changes.
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
    fn wrap_bash_in_sandbox_survives_real_sandbox_exec() {
        // Exercises the actual production path — wrap_bash_in_sandbox() building
        // "/usr/bin/sandbox-exec -f <policy> sh -c '<escaped>'" — instead of just
        // shell_single_quote() in isolation, using a wide-open policy so the
        // sandbox itself can't be the reason anything fails.
        //
        // macOS refuses sandbox_apply from within an already-sandboxed process,
        // so this can't pass while running inside a wtclaude sandbox itself
        // (e.g. `cargo test` run by Claude Code in a wtclaude-managed session).
        // Skip in that case; run this from a plain, unsandboxed terminal.
        if std::env::var("WTCLAUDE_SANDBOX").is_ok() {
            eprintln!(
                "skipping wrap_bash_in_sandbox_survives_real_sandbox_exec: \
                 WTCLAUDE_SANDBOX is set, so sandbox-exec would nest and always fail. \
                 Run this test from a terminal outside a wtclaude sandbox."
            );
            return;
        }

        // WTCLAUDE_SBPL is a process-global env var; this is the only test that
        // touches it today, so there's no cross-test race, but that'd need
        // revisiting if a second test starts setting it.
        let policy_path =
            std::env::temp_dir().join(format!("wtclaude-test-sbpl-{}.sb", std::process::id()));
        std::fs::write(&policy_path, "(version 1)\n(allow default)\n").expect("write policy");
        unsafe {
            std::env::set_var("WTCLAUDE_SBPL", &policy_path);
        }

        let body = "Introduce App struct for introduce-app-struct\n\
\n\
Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>";
        let original = format!("printf '%s\\n' \"$(cat <<'EOF'\n{body}\nEOF\n)\"");

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

        let _ = std::fs::remove_file(&policy_path);
        unsafe {
            std::env::remove_var("WTCLAUDE_SBPL");
        }

        assert_eq!(stdout, format!("{body}\n"));
    }
}
