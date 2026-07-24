mod config;
mod hook;
mod launch;
mod sessions;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("hook") => {
            if let Err(e) = hook::run() {
                eprintln!("hook error: {}", e);
                std::process::exit(1);
            }
        }
        Some("sessions") => {
            if let Err(e) = sessions::run_sessions() {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Some("worktrees") => {
            if let Err(e) = sessions::run_worktrees() {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Some("modes") => {
            if let Err(e) = sessions::run_modes() {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Some("headless") => {
            let rest = args[1..].to_vec();
            match launch::parse_headless_args(rest) {
                Ok(parsed) => match launch::run_headless(parsed) {
                    Ok(code) => std::process::exit(code),
                    Err(e) => {
                        eprintln!("error: {:#}", e);
                        std::process::exit(1);
                    }
                },
                Err(e) => {
                    eprintln!("error: {:#}", e);
                    print_usage();
                    std::process::exit(1);
                }
            }
        }
        Some("session-worktree") => {
            let session_id = args.get(1).map(|s| s.as_str()).unwrap_or("");
            if session_id.is_empty() {
                eprintln!("usage: wtclaude session-worktree SESSION_ID");
                std::process::exit(1);
            }
            if let Err(e) = sessions::run_session_worktree(session_id) {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        Some("--completions") => {
            match args.get(1).map(|s| s.as_str()) {
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
        }
        Some("--help") | Some("-h") => {
            print_usage();
        }
        _ => match launch::parse_args(args) {
            Ok(parsed) => match launch::run(parsed) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("error: {}", e);
                print_usage();
                std::process::exit(1);
            }
        },
    }
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

fn print_usage() {
    eprintln!(
        "usage: wtclaude [--mode MODE] [--resume SESSION_ID] [--test-sbpl-breakage hide|missing] WORKTREE_NAME [INITIAL_PROMPT]"
    );
    eprintln!(
        "       wtclaude headless [--mode MODE] [--resume SESSION_ID] [--show-policy] [--output-format FORMAT] [PROMPT]"
    );
    eprintln!(
        "                     (sandboxed, non-interactive run against cwd; no worktree/branch created; reads PROMPT from stdin if omitted)"
    );
    eprintln!("       wtclaude --completions zsh   (print zsh completion script)");
    eprintln!("       wtclaude hook                (invoked internally as a PreToolUse hook)");
    eprintln!("       wtclaude sessions            (list recent sessions for completion)");
    eprintln!("       wtclaude worktrees           (list worktrees for completion)");
    eprintln!("       wtclaude session-worktree SESSION_ID  (print worktree name for a session)");
}
