use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
pub struct Config {
    #[serde(rename = "default-mode")]
    pub default_mode: String,
    pub modes: HashMap<String, Mode>,
}

#[derive(Deserialize, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub socket_allowlist: Vec<String>,
}

#[derive(Deserialize)]
pub struct Mode {
    #[serde(rename = "claude-flags", default)]
    pub claude_flags: Vec<String>,
}

const DEFAULT_CONFIG: &str = include_str!("default_config.yml");

pub fn load_user() -> Result<UserConfig> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = PathBuf::from(&home).join(".config/wtclaude/wtclaude.yml");
    let config: UserConfig = match std::fs::read_to_string(&path) {
        Ok(s) => serde_yaml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => UserConfig::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    validate_allowlist(&config.allowlist, &home)?;
    Ok(config)
}

/// Rejects allowlist entries with no real literal path prefix before their
/// wildcard (e.g. `*`, `**`, `/*`, `*.log`, or `/./*`/`/../*` — checked
/// against the *resolved* prefix, the same one `resolve_glob_prefix`
/// produces downstream, since `.`/`..` collapse away to nothing there too).
/// Such an entry would silently match every path — or every path under
/// root, or every path with that suffix anywhere on disk — in both sandbox
/// enforcement paths, effectively disabling the sandbox with no indication
/// anything is wrong. A non-empty prefix below root (e.g.
/// `~/.claude.json*`) is fine.
fn validate_allowlist(allowlist: &[String], home: &str) -> Result<()> {
    for entry in allowlist {
        let expanded = entry.replace('~', home);
        if !expanded.contains('*') {
            continue;
        }
        let resolved = resolve_glob_prefix(&expanded);
        let star_idx = resolved
            .find('*')
            .expect("resolve_glob_prefix preserves the wildcard and everything after it");
        let prefix = resolved[..star_idx].trim_end_matches('/');
        if prefix.is_empty() {
            bail!(
                "wtclaude.yml allowlist entry {:?} has no literal path prefix before its \
                 wildcard (resolves to {:?}) and would allow writes to (almost) any path — \
                 refusing to start. Anchor it to a specific path, e.g. `~/.claude.json*` \
                 rather than `*.log`, `/*`, or `/./*`.",
                entry,
                resolved
            );
        }
    }
    Ok(())
}

/// Resolve as many leading path components as exist on disk (canonicalizing
/// away symlinks along the way), then re-append any remaining, not-yet-existing
/// components literally.
pub(crate) fn resolve_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    // Collapse `.`/`..` lexically before walking up: `Path::file_name()`
    // returns `None` for a path ending in `..`, which would otherwise
    // short-circuit the walk-up below and return the `..` unresolved —
    // permanently, since nothing later in the pipeline collapses it either.
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            c => lexical.push(c),
        }
    }
    match (lexical.parent(), lexical.file_name()) {
        (Some(parent), Some(name)) if parent != lexical => {
            resolve_existing_prefix(parent).join(name)
        }
        _ => lexical,
    }
}

/// Canonicalize the static (pre-`*`) prefix of a glob allowlist pattern, so
/// glob entries get the same symlink resolution as literal entries (e.g.
/// `/tmp` -> `/private/tmp` on macOS) instead of silently failing to match
/// a kernel- or canonicalize-resolved real path. A pattern with no `*` is
/// returned unchanged.
pub fn resolve_glob_prefix(pattern: &str) -> String {
    match pattern.find('*') {
        Some(star_idx) => {
            let prefix = resolve_existing_prefix(Path::new(&pattern[..star_idx]));
            format!("{}{}", prefix.to_string_lossy(), &pattern[star_idx..])
        }
        None => pattern.to_string(),
    }
}

pub fn load() -> Result<Config> {
    let exe = std::env::current_exe().context("resolving executable path")?;
    let path = exe
        .parent()
        .context("executable has no parent directory")?
        .join("wtclaude.yml");

    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    serde_yaml::from_str(&content).context("parsing config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allowlist_rejects_bare_wildcard() {
        assert!(validate_allowlist(&["*".to_string()], "/Users/x").is_err());
        assert!(validate_allowlist(&["**".to_string()], "/Users/x").is_err());
    }

    #[test]
    fn validate_allowlist_rejects_root_anchored_wildcard() {
        // `/*` and `/**` have no literal prefix below root, so they're just
        // as allow-everything as a bare `*` despite not being pure stars.
        assert!(validate_allowlist(&["/*".to_string()], "/Users/x").is_err());
        assert!(validate_allowlist(&["/**".to_string()], "/Users/x").is_err());
    }

    #[test]
    fn validate_allowlist_rejects_leading_wildcard_with_no_prefix() {
        // `*.log` has no literal prefix at all before the wildcard — it
        // would match any `.log` file anywhere on the filesystem.
        assert!(validate_allowlist(&["*.log".to_string()], "/Users/x").is_err());
    }

    #[test]
    fn validate_allowlist_rejects_dot_and_dotdot_prefixes_that_resolve_to_root() {
        // Regression test: a raw-string check on the unresolved prefix would
        // see "/." or "/.." as non-empty and let these through, but
        // resolve_glob_prefix collapses both down to "/" before the pattern
        // is ever matched — making these exactly as dangerous as `/*`.
        assert!(validate_allowlist(&["/./*".to_string()], "/Users/x").is_err());
        assert!(validate_allowlist(&["/../*".to_string()], "/Users/x").is_err());
    }

    #[test]
    fn validate_allowlist_allows_anchored_glob() {
        assert!(validate_allowlist(&["~/.claude.json*".to_string()], "/Users/x").is_ok());
    }

    #[test]
    fn resolve_glob_prefix_is_a_no_op_without_a_wildcard() {
        assert_eq!(resolve_glob_prefix("/a/b/c"), "/a/b/c");
    }

    #[test]
    fn resolve_glob_prefix_resolves_a_symlinked_existing_ancestor() {
        // wtclaude is macOS-only (Seatbelt/sandbox-exec); /tmp is always a
        // symlink to /private/tmp there. A pattern rooted at /tmp whose
        // exact file doesn't exist should still have its existing ancestor
        // (/tmp itself) canonicalized, not left as the symlink path.
        let resolved = resolve_glob_prefix("/tmp/wtclaude-test-nonexistent-glob-target*");
        assert_eq!(
            resolved,
            "/private/tmp/wtclaude-test-nonexistent-glob-target*"
        );
    }

    #[test]
    fn resolve_glob_prefix_walks_up_multiple_nonexistent_levels() {
        let resolved =
            resolve_glob_prefix("/tmp/wtclaude-test-a-nonexistent/b-nonexistent/c-nonexistent*");
        assert_eq!(
            resolved,
            "/private/tmp/wtclaude-test-a-nonexistent/b-nonexistent/c-nonexistent*"
        );
    }

    #[test]
    fn resolve_glob_prefix_handles_a_leading_wildcard_with_no_prefix() {
        // Defense in depth: `validate_allowlist` rejects this shape at
        // config-load time, but the pure function itself must still behave
        // sanely (no panic, no infinite recursion) if ever called directly.
        assert_eq!(resolve_glob_prefix("*.log"), "*.log");
    }

    #[test]
    fn resolve_existing_prefix_collapses_parent_dir_even_when_ancestor_is_missing() {
        // Path::file_name() returns None for a trailing "..", which would
        // otherwise short-circuit the walk-up and return the ".." literal
        // and unresolved forever, since nothing downstream collapses it.
        let resolved =
            resolve_existing_prefix(Path::new("/tmp/wtclaude-test-nonexistent-dir-xyz/../etc"));
        assert!(
            !resolved.to_string_lossy().contains(".."),
            "expected \"..\" to be collapsed, got: {}",
            resolved.display()
        );
    }
}
