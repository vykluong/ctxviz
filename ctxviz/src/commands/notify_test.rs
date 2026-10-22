//! `ctxviz notify-test --percent N`: force a band (tab color through remote
//! control, OSC 99 notification and OSC 9;4 progress on /dev/tty).

use crate::kitty::{self, ProgressState};
use crate::session::CurrentSession;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    /// Usage percent to simulate.
    #[arg(long, default_value_t = 85.0)]
    pub percent: f64,
    #[command(flatten)]
    pub theme: super::ThemeArgs,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    args.theme.init();
    let mut k = match kitty::detect() {
        Ok(k) => k,
        Err(reason) => {
            eprintln!("ctxviz notify-test: not in Kitty ({reason})");
            return Ok(1);
        }
    };
    // Own window first, then the agent window the status line recorded.
    if k.window_id.as_deref().is_none_or(str::is_empty) {
        k.window_id = CurrentSession::read().and_then(|c| c.kitty_window_id);
    }
    let band = kitty::band_for(args.percent, None);
    let target = k.window_id.clone().unwrap_or_else(|| "?".into());
    let mut code = 0;
    match kitty::set_tab_color(&k, band) {
        Ok(()) => println!("tab color: window {target} -> {band:?}"),
        Err(e) => {
            println!("tab color: window {target} failed: {e:#}");
            code = 1;
        }
    }
    let body = format!("context at {}%", args.percent);
    match kitty::emit_osc99("ctxviz", &body) {
        Ok(()) => println!("notification: \"ctxviz\" / \"{body}\" -> /dev/tty"),
        Err(e) => println!("notification failed: {e:#}"),
    }
    match kitty::emit_progress(ProgressState::Indeterminate) {
        Ok(()) => {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let _ = kitty::emit_progress(ProgressState::Clear);
            println!("progress: indeterminate for 1 s, then clear");
        }
        Err(e) => println!("progress failed: {e:#}"),
    }
    Ok(code)
}
