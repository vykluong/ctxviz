//! `ctxviz watch`: stream a snapshot on every checkpoint, mirrors
//! `context_usage.py --watch [--json]`.
//!
//! Each poll re-resolves the store (so an unpinned `--cwd`/`--latest` follows
//! new chats), skips the sqlite open while `last_write` is unchanged, and
//! re-renders when `(store path, latestRootBlobId)` changes.

use super::snapshot::{is_colored, render_text, render_tree, text_theme};
use super::{StoreArgs, ThemeArgs};
use crate::model::snapshot::{Snapshot, SnapshotOptions};
use crate::render;
use crate::store::{self, Store, WriteStamp};
use crate::theme::Theme;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    #[command(flatten)]
    pub store: StoreArgs,
    #[command(flatten)]
    pub theme: ThemeArgs,
    /// Emit NDJSON, one snapshot per checkpoint.
    #[arg(long)]
    pub json: bool,
    /// Explorer tree depth (0 hides it).
    #[arg(long, default_value_t = 2)]
    pub depth: usize,
    /// Poll interval in seconds (values below 0.1 are raised to 0.1).
    #[arg(long, default_value_t = 0.5)]
    pub interval: f64,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    args.theme.init();
    let theme = text_theme(&args.theme);
    let opts = SnapshotOptions { tree: args.json || args.depth > 0, timeline: args.json };
    let interval = poll_interval(args.interval);

    let mut path: Option<PathBuf> = None;
    let mut stamp = WriteStamp::default();
    let mut root: Option<String> = None;
    loop {
        let p = args.store.resolve()?;
        let new_path = path.as_ref() != Some(&p);
        if new_path {
            eprintln!("watching {}", crate::text::sanitize_single_line(&p.display().to_string()));
            root = None;
        }
        let st = store::last_write(&p);
        if new_path || st != stamp || root.is_none() {
            stamp = st;
            let s = Store::open(&p)?;
            let id = s.latest_root_id()?;
            if id.is_some() && id != root {
                if let Some(snap) = Snapshot::load(&s, opts)? {
                    if !emit(&args, &snap, &theme)? {
                        return Ok(0);
                    }
                    root = id;
                }
            }
        }
        path = Some(p);
        std::thread::sleep(interval);
    }
}

/// Shortest poll interval `--interval` may request, in seconds.
pub const MIN_INTERVAL_SECS: f64 = 0.1;

/// `--interval` as a duration, clamped to at least [`MIN_INTERVAL_SECS`]
/// (NaN and negative values clamp too) so a tiny value cannot spin the loop.
pub fn poll_interval(secs: f64) -> Duration {
    let secs = if secs.is_finite() {
        secs.max(MIN_INTERVAL_SECS)
    } else if secs > 0.0 {
        3600.0
    } else {
        MIN_INTERVAL_SECS
    };
    Duration::from_secs_f64(secs.min(3600.0))
}

/// Print one snapshot; `Ok(false)` when stdout is closed (e.g. `| head`).
fn emit(args: &Args, snap: &Snapshot, theme: &Theme) -> anyhow::Result<bool> {
    let text = if args.json {
        format!("{}\n", render::json::to_string(snap)?)
    } else {
        let clear = if is_colored(theme) { "\x1b[2J\x1b[H" } else { "" };
        format!("{clear}{}{}", render_text(snap, theme), render_tree(snap, args.depth))
    };
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_has_a_floor() {
        assert_eq!(poll_interval(0.5), Duration::from_millis(500));
        assert_eq!(poll_interval(0.1), Duration::from_millis(100));
        for tiny in [0.0, 0.001, 0.05, -3.0, f64::NAN, f64::NEG_INFINITY] {
            assert_eq!(poll_interval(tiny), Duration::from_millis(100), "{tiny}");
        }
        assert_eq!(poll_interval(f64::INFINITY), Duration::from_secs(3600));
    }
}
