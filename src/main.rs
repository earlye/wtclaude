mod config;
mod hook;
mod launch;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(|s| s == "hook").unwrap_or(false) {
        if let Err(e) = hook::run() {
            eprintln!("hook error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    if args
        .first()
        .map(|s| s == "--help" || s == "-h")
        .unwrap_or(false)
    {
        print_usage();
        return;
    }

    match launch::parse_args(args) {
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
    }
}

fn print_usage() {
    eprintln!(
        "usage: wtclaude [--mode MODE] [--test-sbpl-breakage hide|missing] WORKTREE_NAME [INITIAL_PROMPT]"
    );
    eprintln!("       wtclaude hook  (invoked internally as a PreToolUse hook)");
}
