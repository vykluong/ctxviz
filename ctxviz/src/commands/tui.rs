//! `ctxviz tui`: the live side pane. `--chat`/`--cwd`/`--latest` pin a store;
//! with none of them it follows `<state dir>/current.json`.

use super::{StoreArgs, ThemeArgs};

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    #[command(flatten)]
    pub store: StoreArgs,
    #[command(flatten)]
    pub theme: ThemeArgs,
    /// Explorer width as a percent of the pane (default: max(36 cols, 35%)).
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(1..=100))]
    pub split: Option<u8>,
    /// Timeline height in rows (default: 35% of the body, 10..=16).
    #[arg(long, value_name = "ROWS")]
    pub timeline_rows: Option<u16>,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    // Best effort: drop pager files a previous run's overlay never cleaned up,
    // and state temp files a killed writer never renamed.
    crate::tui::app::sweep_stale_pager_files();
    crate::session::sweep_stale_tmp_files();
    args.theme.init_with_probe(&probe_background);
    let s = &args.store;
    let pinned = if s.chat.is_some() || s.cwd.is_some() || s.latest { Some(s.resolve()?) } else { None };
    let opts = crate::tui::TuiOptions {
        split_pct: args.split,
        timeline_rows: args.timeline_rows,
        state_path: Some(crate::session::tui_state_path()),
    };
    crate::tui::run(pinned, opts)?;
    Ok(0)
}

/// Ask the terminal for its background (OSC 11) before ratatui takes over the
/// tty. `None` when stdin or stdout is not a terminal, or on any error or
/// timeout. Only the TUI probes; the status line's stdin is Cursor's payload.
fn probe_background() -> Option<crate::theme::Mode> {
    use std::io::IsTerminal;
    use terminal_colorsaurus::{QueryOptions, ThemeMode};
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    let mut opts = QueryOptions::default();
    opts.timeout = std::time::Duration::from_millis(200);
    match terminal_colorsaurus::theme_mode(opts).ok()? {
        ThemeMode::Dark => Some(crate::theme::Mode::Dark),
        ThemeMode::Light => Some(crate::theme::Mode::Light),
    }
}
