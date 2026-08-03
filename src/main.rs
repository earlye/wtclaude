mod config;
mod hook;
mod launch;
mod sessions;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use launch::{Args as LaunchArgs, HeadlessArgs, PathArgs};

#[derive(Parser)]
#[command(name = "wtclaude", disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Invoked internally as a PreToolUse hook
    Hook,
    /// List recent sessions for completion
    Sessions,
    /// List worktrees for completion
    Worktrees,
    /// List modes for completion
    Modes,
    /// Sandboxed, non-interactive run against cwd (no worktree/branch created;
    /// reads PROMPT from stdin if omitted)
    Headless(HeadlessArgs),
    /// Sandboxed, interactive run against DIRECTORY (no worktree/branch created)
    Path(PathArgs),
    /// Print the worktree name for a session
    SessionWorktree {
        /// Session ID to resolve to a worktree name
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        session_id: String,
    },
}

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let first = raw_args.get(1).map(|s| s.as_str());

    if first == Some("--completions") {
        match raw_args.get(2).map(|s| s.as_str()) {
            Some("zsh") => print_completions_zsh(),
            Some(other) => {
                eprintln!("unsupported shell: {}. Supported: zsh", other);
                std::process::exit(1);
            }
            None => {
                eprintln!("usage: wtclaude --completions zsh");
                std::process::exit(1);
            }
        }
        return;
    }

    // Peek at the first token to decide whether this looks like one of the
    // known subcommand keywords (in which case route through the Cli
    // subcommand tree) or the bare `wtclaude WORKTREE_NAME [PROMPT]` launch
    // form (which has no keyword and must fall through to LaunchArgs).
    // Derived from Cli itself so the keyword list can't drift out of sync
    // with `enum Commands`. Deliberately excludes --help/-h: those fall
    // through to LaunchArgs so `wtclaude --help` documents the primary,
    // most-frequently-used invocation form (see Args's after_help for the
    // rest of the command surface).
    let known_subcommand = Cli::command()
        .get_subcommands()
        .any(|c| first == Some(c.get_name()));

    if known_subcommand {
        match Cli::parse().command {
            Commands::Hook => {
                if let Err(e) = hook::run() {
                    eprintln!("hook error: {}", e);
                    std::process::exit(1);
                }
            }
            Commands::Sessions => {
                if let Err(e) = sessions::run_sessions() {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
            Commands::Worktrees => {
                if let Err(e) = sessions::run_worktrees() {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
            Commands::Modes => {
                if let Err(e) = sessions::run_modes() {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
            Commands::Headless(parsed) => match launch::run_headless(parsed) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("error: {:#}", e);
                    std::process::exit(1);
                }
            },
            Commands::Path(parsed) => match launch::run_path(parsed) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("error: {:#}", e);
                    std::process::exit(1);
                }
            },
            Commands::SessionWorktree { session_id } => {
                if let Err(e) = sessions::run_session_worktree(&session_id) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        return;
    }

    let launch_command = LaunchArgs::command().after_help(build_after_help());
    match launch_command
        .try_get_matches_from(&raw_args)
        .and_then(|matches| LaunchArgs::from_arg_matches(&matches))
    {
        Ok(parsed) => match launch::run(parsed) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        },
        Err(e) => e.exit(),
    }
}

/// Lists the other subcommands (name + about, pulled live from `Commands`)
/// so `wtclaude --help` documents the full command surface without
/// hand-duplicating it — the one exception is `--completions zsh`, which
/// isn't a `Commands` variant (it's intercepted before clap ever runs).
fn build_after_help() -> String {
    let entries: Vec<(String, String)> = Cli::command()
        .get_subcommands()
        .map(|sub| {
            (
                sub.get_name().to_string(),
                sub.get_about().map(|a| a.to_string()).unwrap_or_default(),
            )
        })
        .chain(std::iter::once((
            "--completions zsh".to_string(),
            "Print the zsh completion script".to_string(),
        )))
        .collect();
    let width = entries
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);

    let mut help = String::from("Other commands:\n");
    for (name, about) in &entries {
        help.push_str(&format!("  wtclaude {name:width$}  {about}\n"));
    }
    help.push_str("\nRun `wtclaude <command> --help` for details on a specific command.");
    help
}

fn print_completions_zsh() {
    print!(
        r#"
_wtclaude_sessions() {{
  local -a session_ids descriptions
  local session_id worktree epoch date_str

  while IFS=$'\t' read -r session_id worktree epoch; do
    [[ -z $session_id ]] && continue
    date_str=$(date -r "$epoch" '+%Y-%m-%d %H:%M' 2>/dev/null || echo "$epoch")
    session_ids+=("$session_id")
    descriptions+=("$session_id:$worktree  $date_str")
  done < <(wtclaude sessions 2>/dev/null)

  (( ${{#session_ids}} )) || return 1
  compadd -d descriptions -a session_ids
}}

_wtclaude_worktrees() {{
  local -i i
  for (( i = 1; i <= $#words - 1; i++ )); do
    if [[ ${{words[$i]}} == '--resume' && -n ${{words[$((i+1))]}} ]]; then
      local worktree
      worktree=$(wtclaude session-worktree "${{words[$((i+1))]}}" 2>/dev/null)
      if [[ -n $worktree ]]; then
        compadd -- "$worktree"
        return
      fi
    fi
  done

  local -a worktrees
  worktrees=( ${{(f)"$(wtclaude worktrees 2>/dev/null)"}} )
  compadd -a worktrees
}}

_wtclaude_modes() {{
  local -a modes
  modes=( ${{(f)"$(wtclaude modes 2>/dev/null)"}} )
  (( ${{#modes}} )) && compadd -a modes
}}

_wtclaude() {{
  local context state line
  typeset -A opt_args

  _arguments -s -S \
    '(--help -h)'{{--help,-h}}'[show usage]' \
    '--mode[operation mode]:MODE:_wtclaude_modes' \
    '--resume[resume a previous session]:SESSION_ID:_wtclaude_sessions' \
    '--no-pull[skip git pull before launch]' \
    '--test-sbpl-breakage[inject sandbox policy breakage for testing]:TYPE:(hide missing)' \
    ':WORKTREE_NAME:_wtclaude_worktrees' \
    '*:INITIAL_PROMPT: '
}}

compdef _wtclaude wtclaude
"#
    );
}
