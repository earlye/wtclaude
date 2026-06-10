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
        return Ok(Some(deny_bash(&format!(
            "Sandbox policy file missing: {}. Bash is blocked.",
            sbpl_path
        ))));
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
    format!("'{}'", s.replace('\'', "'\\''"))
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
