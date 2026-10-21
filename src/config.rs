use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_yaml::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
pub struct Config {
    #[serde(rename = "default-mode")]
    pub default_mode: String,
    pub modes: HashMap<String, Mode>,
}

/// The per-machine config's sandbox settings. Its `default-mode` and `modes`
/// are read separately, as `ModeOverrides`, so a mistake there can't break
/// the sandbox of a running session.
#[derive(Deserialize, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub socket_allowlist: Vec<String>,
    /// Which sandbox backend enforces the plan: `auto` (the default) picks
    /// this platform's preferred available backend, or name one explicitly
    /// (`seatbelt`, `landlock`).
    #[serde(rename = "sandbox-backend", default)]
    pub sandbox_backend: Option<String>,
}

impl UserConfig {
    /// What the launcher should ask `sandbox::select` for. Deliberately does
    /// *not* consult `WTCLAUDE_BACKEND`: that variable is the launcher's
    /// instruction *to* the hook, and the hook must fail closed when it is
    /// absent rather than re-deriving a preference of its own.
    pub fn backend_preference(&self) -> String {
        self.sandbox_backend
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "auto".to_string())
    }
}

/// The per-machine config's `default-mode` and `modes`, overlaid on the
/// binary-adjacent (or built-in) config by `merge`.
#[derive(Deserialize, Default)]
struct ModeOverrides {
    #[serde(rename = "default-mode", default)]
    default_mode: Option<String>,
    #[serde(default)]
    modes: HashMap<String, Mode>,
}

#[derive(Deserialize)]
pub struct Mode {
    #[serde(rename = "claude-flags", default)]
    pub claude_flags: Vec<String>,
}

const DEFAULT_CONFIG: &str = include_str!("default_config.yml");

const CONFIG_KEYS: &[&str] = &["default-mode", "modes"];
const USER_CONFIG_KEYS: &[&str] = &[
    "default-mode",
    "modes",
    "allowlist",
    "socket_allowlist",
    "sandbox-backend",
];
const MODE_KEYS: &[&str] = &["claude-flags"];

/// Unknown keys are not reported here: the hook calls this on every tool
/// call and would repeat them each time. `load`, which every launch runs,
/// reports them instead.
pub fn load_user() -> Result<UserConfig> {
    let (home, path) = user_config_path()?;
    let (config, _unknown): (UserConfig, _) = read_user(&path)?;
    validate_allowlist(&config.allowlist, &home)?;
    Ok(config)
}

fn user_config_path() -> Result<(String, PathBuf)> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = PathBuf::from(&home).join(".config/wtclaude/wtclaude.yml");
    Ok((home, path))
}

/// Reads the per-machine config as `T`, returning it along with any keys the
/// file has that it doesn't recognise. A missing file is an empty config.
fn read_user<T: DeserializeOwned + Default>(path: &Path) -> Result<(T, Vec<String>)> {
    match std::fs::read_to_string(path) {
        Ok(s) => parse_checked(&s, USER_CONFIG_KEYS)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((T::default(), vec![])),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Parses `content` as `T`, also returning the keys serde would silently
/// ignore: top-level keys not in `known`, and keys inside a `modes` entry
/// other than `claude-flags`, as dotted paths (`modes.safe.flags`).
fn parse_checked<T: DeserializeOwned>(content: &str, known: &[&str]) -> Result<(T, Vec<String>)> {
    let value: Value = serde_yaml::from_str(content)?;
    let unknown = unknown_keys(&value, known);
    // Deserialize from the text rather than `value`: only that keeps the field
    // path and line/column in type errors. An empty file parses as null; read
    // it as an empty mapping so structs whose fields all default deserialize.
    let config = if value.is_null() {
        serde_yaml::from_str("{}")?
    } else {
        serde_yaml::from_str(content)?
    };
    Ok((config, unknown))
}

fn unknown_keys(value: &Value, known: &[&str]) -> Vec<String> {
    let mut unknown = Vec::new();
    let Some(map) = value.as_mapping() else {
        return unknown;
    };
    for (key, val) in map {
        let key = key_name(key);
        if !known.contains(&key.as_str()) {
            unknown.push(key);
            continue;
        }
        if key == "modes"
            && let Some(modes) = val.as_mapping()
        {
            for (name, mode) in modes {
                let Some(mode) = mode.as_mapping() else {
                    continue;
                };
                for mode_key in mode.keys().map(key_name) {
                    if !MODE_KEYS.contains(&mode_key.as_str()) {
                        unknown.push(format!("modes.{}.{}", key_name(name), mode_key));
                    }
                }
            }
        }
    }
    unknown
}

fn key_name(key: &Value) -> String {
    match key {
        Value::String(s) => s.clone(),
        other => serde_yaml::to_string(other)
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default(),
    }
}

fn warn_unknown_keys(source: &str, unknown: &[String]) {
    for key in unknown {
        eprintln!("wtclaude: warning: {source}: ignoring unknown key `{key}`");
    }
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

/// The binary-adjacent `wtclaude.yml` (or the built-in default) with the
/// per-machine config's `default-mode` and `modes` overlaid on it. The
/// binary-adjacent file only accepts `default-mode` and `modes`: sessions can
/// write to `~/.cargo/bin`, so the sandbox settings must not come from there.
pub fn load() -> Result<Config> {
    let exe = std::env::current_exe().context("resolving executable path")?;
    let path = exe
        .parent()
        .context("executable has no parent directory")?
        .join("wtclaude.yml");
    let (_, user_path) = user_config_path()?;
    load_from(&path, &user_path)
}

fn load_from(path: &Path, user_path: &Path) -> Result<Config> {
    let base: Config = match std::fs::read_to_string(path) {
        Ok(s) => {
            let (config, unknown) = parse_checked(&s, CONFIG_KEYS)
                .with_context(|| format!("parsing {}", path.display()))?;
            warn_unknown_keys(&path.display().to_string(), &unknown);
            config
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_yaml::from_str(DEFAULT_CONFIG).context("parsing built-in default config")?
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    let (user, unknown) = read_user(user_path)?;
    warn_unknown_keys(&user_path.display().to_string(), &unknown);
    Ok(merge(base, user))
}

/// Overlays the per-machine config on the base config: its `default-mode`
/// replaces the base one if set, and each mode it names replaces the base
/// mode of that name whole (`claude-flags` are not concatenated).
fn merge(mut base: Config, user: ModeOverrides) -> Config {
    if let Some(mode) = user.default_mode {
        base.default_mode = mode;
    }
    base.modes.extend(user.modes);
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> Config {
        serde_yaml::from_str(DEFAULT_CONFIG).unwrap()
    }

    fn flags(config: &Config, mode: &str) -> Vec<String> {
        config.modes[mode].claude_flags.clone()
    }

    #[test]
    fn merge_without_user_settings_keeps_the_base() {
        let merged = merge(base_config(), ModeOverrides::default());
        assert_eq!(merged.default_mode, "safe");
        assert_eq!(
            flags(&merged, "dangerous"),
            ["--dangerously-skip-permissions"]
        );
        assert!(flags(&merged, "safe").is_empty());
    }

    #[test]
    fn merge_user_default_mode_wins() {
        let (user, _): (ModeOverrides, _) =
            parse_checked("default-mode: dangerous", USER_CONFIG_KEYS).unwrap();
        assert_eq!(merge(base_config(), user).default_mode, "dangerous");
    }

    #[test]
    fn merge_user_mode_replaces_the_whole_entry_and_keeps_the_others() {
        let (user, _): (ModeOverrides, _) = parse_checked(
            "modes:\n  dangerous:\n    claude-flags: [a, b]\n  verbose:\n    claude-flags: [--verbose]\n",
            USER_CONFIG_KEYS,
        )
        .unwrap();
        let merged = merge(base_config(), user);
        // Replaced, not concatenated with `--dangerously-skip-permissions`.
        assert_eq!(flags(&merged, "dangerous"), ["a", "b"]);
        assert_eq!(flags(&merged, "verbose"), ["--verbose"]);
        assert!(flags(&merged, "safe").is_empty());
        assert_eq!(merged.default_mode, "safe");
    }

    #[test]
    fn built_in_default_config_has_no_unknown_keys() {
        let (_, unknown): (Config, _) = parse_checked(DEFAULT_CONFIG, CONFIG_KEYS).unwrap();
        assert!(unknown.is_empty(), "{unknown:?}");
    }

    #[test]
    fn parse_checked_reports_unknown_top_level_and_mode_keys() {
        let (_, unknown): (UserConfig, _) = parse_checked(
            "default-mode: safe\nbogus: 1\nmodes:\n  safe:\n    claude-flag: []\n",
            USER_CONFIG_KEYS,
        )
        .unwrap();
        assert_eq!(unknown, ["bogus", "modes.safe.claude-flag"]);
    }

    #[test]
    fn parse_checked_treats_sandbox_settings_as_unknown_in_the_binary_adjacent_file() {
        let (config, unknown): (Config, _) = parse_checked(
            "default-mode: safe\nmodes: {}\nallowlist: [/x]\nsocket_allowlist: [/y]\nsandbox-backend: auto\n",
            CONFIG_KEYS,
        )
        .unwrap();
        assert_eq!(config.default_mode, "safe");
        assert_eq!(
            unknown,
            ["allowlist", "socket_allowlist", "sandbox-backend"]
        );
    }

    #[test]
    fn parse_checked_accepts_an_empty_user_file() {
        let (config, unknown): (ModeOverrides, _) = parse_checked("", USER_CONFIG_KEYS).unwrap();
        assert!(config.default_mode.is_none());
        assert!(config.modes.is_empty());
        assert!(unknown.is_empty());
    }

    #[test]
    fn parse_checked_type_errors_name_the_field_and_position() {
        let err = parse_checked::<ModeOverrides>(
            "modes:\n  safe:\n    claude-flags: 5\n",
            USER_CONFIG_KEYS,
        )
        .err()
        .expect("a number is not a flag list");
        let message = format!("{err:#}");
        assert!(message.contains("modes.safe.claude-flags"), "{message}");
        assert!(message.contains("line 3"), "{message}");
    }

    #[test]
    fn user_sandbox_settings_survive_a_broken_mode() {
        let (config, _): (UserConfig, _) = parse_checked(
            "allowlist: [/x]\nmodes:\n  safe:\n    claude-flags: --verbose\n",
            USER_CONFIG_KEYS,
        )
        .unwrap();
        assert_eq!(config.allowlist, ["/x"]);
    }

    /// A fresh directory for one `load_from` test; removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wtclaude-test-load-{}-{}",
                label,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creating temp dir");
            TempDir(dir)
        }

        fn file(&self, name: &str, content: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, content).expect("writing config file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn load_from_without_either_file_uses_the_built_in_default() {
        let dir = TempDir::new("absent");
        let config = load_from(&dir.0.join("base.yml"), &dir.0.join("user.yml")).unwrap();
        assert_eq!(config.default_mode, "safe");
        assert_eq!(
            flags(&config, "dangerous"),
            ["--dangerously-skip-permissions"]
        );
    }

    #[test]
    fn load_from_overlays_the_user_file_on_the_binary_adjacent_file() {
        let dir = TempDir::new("overlay");
        let base = dir.file(
            "base.yml",
            "default-mode: a\nmodes:\n  a:\n    claude-flags: [x]\n  b:\n    claude-flags: [y]\nallowlist: [/z]\n",
        );
        let user = dir.file(
            "user.yml",
            "default-mode: b\nmodes:\n  b:\n    claude-flags: [w]\n",
        );
        let config = load_from(&base, &user).unwrap();
        assert_eq!(config.default_mode, "b");
        assert_eq!(flags(&config, "a"), ["x"]);
        assert_eq!(flags(&config, "b"), ["w"]);
        assert!(!config.modes.contains_key("safe"));
    }

    #[test]
    fn load_from_names_a_malformed_user_file() {
        let dir = TempDir::new("malformed");
        let user = dir.file("user.yml", "modes: [x]\n");
        let err = load_from(&dir.0.join("base.yml"), &user)
            .err()
            .expect("a list is not a modes map");
        assert!(
            format!("{err:#}").contains(&user.display().to_string()),
            "{err:#}"
        );
    }

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
        // `/tmp` is a symlink to `/private/tmp` on macOS and a real directory
        // on Linux. Either way, a pattern rooted at /tmp whose exact file
        // doesn't exist must still have its existing ancestor (/tmp itself)
        // canonicalized, not left as the unresolved path — so the expectation
        // is whatever /tmp actually resolves to here, not a hardcoded macOS
        // path. Hardcoding it made this assert platform-specific while the
        // function under test was already correct on both.
        let tmp = std::fs::canonicalize("/tmp").unwrap();
        let resolved = resolve_glob_prefix("/tmp/wtclaude-test-nonexistent-glob-target*");
        assert_eq!(
            resolved,
            format!("{}/wtclaude-test-nonexistent-glob-target*", tmp.display())
        );
    }

    #[test]
    fn resolve_glob_prefix_walks_up_multiple_nonexistent_levels() {
        let tmp = std::fs::canonicalize("/tmp").unwrap();
        let resolved =
            resolve_glob_prefix("/tmp/wtclaude-test-a-nonexistent/b-nonexistent/c-nonexistent*");
        assert_eq!(
            resolved,
            format!(
                "{}/wtclaude-test-a-nonexistent/b-nonexistent/c-nonexistent*",
                tmp.display()
            )
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

    #[test]
    fn resolve_existing_prefix_resolves_a_real_symlinked_ancestor() {
        // The /tmp-based tests above can only observe symlink resolution on a
        // platform where /tmp *is* a symlink (macOS). On Linux /tmp is a real
        // directory, so there they'd still pass even if canonicalization were
        // dropped entirely. Build an actual symlink so the property under test
        // — a nonexistent target resolving through a symlinked ancestor — is
        // asserted the same way on every platform. This is the invariant that
        // keeps the two enforcement layers (policy rules vs. the PreToolUse
        // path check) agreeing on one path.
        let base = std::env::temp_dir().join(format!(
            "wtclaude-test-symlink-ancestor-{}",
            std::process::id()
        ));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).expect("creating target dir");
        std::os::unix::fs::symlink(&target, &link).expect("creating symlink");

        // Resolve before cleanup, and assert after, so a failure can't leak
        // the temp tree.
        let resolved = resolve_existing_prefix(&link.join("not-created-yet"));
        let expected = target
            .canonicalize()
            .expect("canonicalizing target")
            .join("not-created-yet");
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(resolved, expected);
    }
}
