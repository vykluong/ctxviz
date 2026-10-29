use clap::{Parser, Subcommand};
use ctxviz::commands;

#[derive(Parser)]
#[command(name = "ctxviz", version = ctxviz::build_info::LONG,about = "Live context visualizer for Cursor CLI sessions")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render the agent status line from the payload on stdin.
    Statusline(commands::statusline::Args),
    /// Interactive explorer and timeline.
    Tui(commands::tui::Args),
    /// Open the TUI in a Kitty split next to the agent window.
    Open(commands::open::Args),
    /// Print the context usage report once.
    Snapshot(commands::snapshot::Args),
    /// Stream snapshots on every checkpoint.
    Watch(commands::watch::Args),
    /// Diagnose store layout and dump the latest root's field tree.
    Doctor(commands::doctor::Args),
    /// Fire a threshold notification and tab color for testing.
    NotifyTest(commands::notify_test::Args),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Statusline(a) => commands::statusline::run(a),
        Command::Tui(a) => commands::tui::run(a),
        Command::Open(a) => commands::open::run(a),
        Command::Snapshot(a) => commands::snapshot::run(a),
        Command::Watch(a) => commands::watch::run(a),
        Command::Doctor(a) => commands::doctor::run(a),
        Command::NotifyTest(a) => commands::notify_test::run(a),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("ctxviz: {e:#}");
            std::process::exit(1);
        }
    }
}
