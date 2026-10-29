//! `ctxviz statusline`: the Cursor CLI status line command.
//!
//! Reads the payload JSON from stdin, reads the token breakdown from the
//! session's chat store (one meta row, one root blob, field 5 only), records
//! the session in `<state dir>/current.json` and prints the rows.
//!
//! Error policy (docs/plan.md):
//! - store missing, no root yet, or root without token details: render from
//!   the payload's `context_window` only, exit 0 (the row is never blank);
//! - a root blob that fails to decode (corrupt, too deeply nested, unknown
//!   wire type): log the error to stderr and render the payload-only row,
//!   exit 0, so a bad root never blanks the row;
//! - transient errors (busy, unreadable store, root id present but its blob
//!   not yet visible): exit 1 with empty stdout, so Cursor keeps the previous
//!   text;
//! - a panic anywhere in the store path still yields the payload-only row.

use crate::kitty::{self, Band};
use crate::model::breakdown::{self, TokenDetails};
use crate::model::timeline::{self, Timeline};
use crate::render::statusline::{self as sl, BarStyle, Layout, RenderOpts, Rows, StatusPayload, Trend, Usage};
use crate::session::CurrentSession;
use crate::store::{self, Store};
use crate::text;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// Debug-only test hook: compiled into debug builds only, so release builds
/// ignore it. Overrides the percent used for band tracking (not the rendered
/// percent), e.g. `CTXVIZ_FORCE_PERCENT=85`.
#[cfg(debug_assertions)]
const FORCE_PERCENT_ENV: &str = "CTXVIZ_FORCE_PERCENT";

/// `CTXVIZ_NO_GIT=1`: same as `--no-git`.
pub const NO_GIT_ENV: &str = "CTXVIZ_NO_GIT";

/// Largest payload read from stdin; a longer one is rejected.
pub const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    #[command(flatten)]
    pub theme: super::ThemeArgs,
    /// Number of rows: 2 (default) or 1.
    #[arg(long, default_value = "2")]
    pub rows: Rows,
    /// Bar style: plain (default) or labeled (category names inside wide segments).
    #[arg(long, default_value = "plain")]
    pub bar: BarStyle,
    /// Two-row layout: wide (default; full-width bar, top-3 legend) or classic
    #[arg(long, default_value = "wide")]
    pub layout: Layout,
    /// Never run git or read `.git` in the workspace: no branch or diff
    /// segment. Env: CTXVIZ_NO_GIT=1.
    #[arg(long)]
    pub no_git: bool,
}

/// True when `CTXVIZ_NO_GIT` is set to anything but empty, `0` or `false`.
pub fn no_git_env(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Read the whole payload, failing when it is over [`MAX_PAYLOAD_BYTES`].
pub fn read_payload(r: impl Read) -> Result<String> {
    let mut input = String::new();
    r.take(MAX_PAYLOAD_BYTES + 1).read_to_string(&mut input).context("reading payload from stdin")?;
    if input.len() as u64 > MAX_PAYLOAD_BYTES {
        anyhow::bail!("status line payload is larger than {MAX_PAYLOAD_BYTES} bytes");
    }
    Ok(input)
}

/// What the store lookup produced.
pub enum Lookup {
    /// Breakdown from the latest root.
    Details(TokenDetails),
    /// Store or root not there (yet): payload-only row.
    Missing,
    /// The latest root blob exists but does not decode: payload-only row,
    /// with the error logged.
    Undecodable(anyhow::Error),
}

/// `<cursor dir>/chats/<md5(abs cwd)>/<session_id>/store.db`, else the exact
/// session id under any project dir. A path that exists but is not a file is
/// still returned so opening it fails as an error, not as "missing".
pub fn resolve_store(p: &StatusPayload) -> Option<PathBuf> {
    let sid = p.session_id.as_deref().filter(|s| text::valid_id(s))?;
    if let Some(cwd) = p.work_dir() {
        let direct = store::chats_dir().join(store::project_hash(Path::new(cwd))).join(sid).join("store.db");
        if direct.exists() {
            return Some(direct);
        }
    }
    store::store_for_agent(sid)
}

/// Fast path: meta → latestRootBlobId → root blob → field 5. No tree, no timeline.
pub fn lookup(path: &Path) -> Result<Lookup> {
    lookup_in(&Store::open(path)?)
}

/// [`lookup`] on an open store.
pub fn lookup_in(store: &Store) -> Result<Lookup> {
    let Some(root_id) = store.latest_root_id().context("reading meta")? else {
        return Ok(Lookup::Missing);
    };
    // The meta update and the root insert can be separate transactions:
    // treat a dangling root id as transient and keep the previous row.
    let blob = store.blob(&root_id)?.with_context(|| format!("root blob {root_id} not visible yet"))?;
    match breakdown::token_details_only(&blob) {
        Ok(Some(td)) => Ok(Lookup::Details(td)),
        Ok(None) => Ok(Lookup::Missing),
        Err(e) => Ok(Lookup::Undecodable(e.context("decoding root token details"))),
    }
}

pub fn run(args: Args) -> Result<i32> {
    args.theme.init();
    // Silence the default panic message; panics are handled below.
    std::panic::set_hook(Box::new(|_| {}));
    let input = read_payload(std::io::stdin().lock())?;
    let payload = StatusPayload::parse(&input).context("status line payload is not JSON")?;

    let store_path = catch_unwind(|| resolve_store(&payload)).unwrap_or(None);
    let mut store = None;
    let looked_up = match &store_path {
        None => Ok(Lookup::Missing),
        Some(path) => catch_unwind(AssertUnwindSafe(|| {
            let s = Store::open(path)?;
            let r = lookup_in(&s);
            store = Some(s);
            r
        }))
        .unwrap_or(Ok(Lookup::Missing)),
    };

    record_session(&payload, store_path.as_deref());

    let details = match looked_up {
        Ok(Lookup::Details(td)) => Some(td),
        Ok(Lookup::Missing) => None,
        Ok(Lookup::Undecodable(e)) => {
            eprintln!("ctxviz statusline: {e:#}; showing the payload-only row");
            None
        }
        Err(e) => {
            eprintln!("ctxviz statusline: {e:#}");
            return Ok(1);
        }
    };

    // Timeline: row 2 trend (sparkline + last delta) and band tracking. Any
    // failure only drops the trend; the rows still render.
    let usage = match &details {
        Some(td) => Usage::from_details(td, &payload),
        None => Usage::from_payload(&payload),
    };
    let (trend, band_change) = match (&store, &store_path) {
        (Some(s), Some(path)) => match catch_unwind(AssertUnwindSafe(|| timeline_extras(s, path, &usage))) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                eprintln!("ctxviz statusline: timeline: {e:#}");
                (Trend::default(), None)
            }
            Err(_) => (Trend::default(), None),
        },
        _ => (Trend::default(), None),
    };
    drop(store);

    let no_git = args.no_git || no_git_env(std::env::var(NO_GIT_ENV).ok().as_deref());
    let opts = RenderOpts { rows: args.rows, bar: args.bar, layout: args.layout, no_git };
    let rendered = catch_unwind(|| sl::render_with(&payload, details.as_ref(), &trend, opts))
        .or_else(|_| catch_unwind(|| sl::render_with(&payload, None, &Trend::default(), opts)));
    let Ok(text) = rendered else { return Ok(1) };

    let mut out = std::io::stdout().lock();
    if writeln!(out, "{text}").and_then(|_| out.flush()).is_err() {
        return Ok(1);
    }
    drop(out);

    let _ = catch_unwind(AssertUnwindSafe(|| after_render(band_change)));
    Ok(0)
}

/// A band crossing to persist and show once the rows are printed.
pub struct BandChange {
    pub band: Band,
    timeline: Timeline,
    cache: PathBuf,
}

/// Load the timeline (extending the rowid cache), collect the row 2 trend
/// data (the last checkpoints' used tokens and compaction flags, and the last
/// delta) and detect a band crossing against `last_notified_band`.
fn timeline_extras(store: &Store, store_path: &Path, usage: &Usage) -> Result<(Trend, Option<BandChange>)> {
    // The store's directory name is the agent id, also on the glob fallback.
    let agent_id = store_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .context("store path has no agent id dir")?;
    let tl = Timeline::load_or_scan(store, agent_id)?;

    let start = tl.checkpoints.len().saturating_sub(sl::SPARK_LEN);
    let window = &tl.checkpoints[start..];
    let now_ms =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let trend = Trend {
        values: window.iter().map(|c| c.used_tokens).collect(),
        compaction: window.iter().map(|c| c.compaction).collect(),
        delta: tl.last_delta(),
        // Root field 26 (createdAt) is milliseconds since the epoch.
        duration_secs: tl.store_created_at.filter(|c| *c > 0 && *c <= now_ms).map(|c| (now_ms - c) / 1000),
    };

    // Debug-only test hook; see FORCE_PERCENT_ENV.
    #[cfg(debug_assertions)]
    let forced = std::env::var(FORCE_PERCENT_ENV).ok().and_then(|v| v.trim().parse::<f64>().ok());
    #[cfg(not(debug_assertions))]
    let forced: Option<f64> = None;
    let change = match forced.or(usage.percent) {
        Some(pct) => {
            let prev = tl.last_notified_band.and_then(Band::from_u8);
            let band = kitty::band_for(pct, prev);
            (prev != Some(band)).then(|| BandChange { band, timeline: tl, cache: timeline::cache_path(agent_id) })
        }
        None => None,
    };
    Ok((trend, change))
}

/// EXTENSION(unit 7): side effects after the rows are printed. Stdout is
/// already flushed, so nothing here delays the status line text. On a band
/// crossing: record it in the timeline cache, then color the Kitty tab.
fn after_render(change: Option<BandChange>) {
    let Some(BandChange { band, mut timeline, cache }) = change else { return };
    timeline.last_notified_band = Some(band.as_u8());
    if let Err(e) = timeline.save(&cache) {
        eprintln!("ctxviz statusline: {e:#}");
        return;
    }
    if let Ok(k) = kitty::detect() {
        let _ = kitty::set_tab_color(&k, band);
    }
}

/// Best effort: `<state dir>/current.json` for `ctxviz open` and the TUI.
fn record_session(p: &StatusPayload, store: Option<&Path>) {
    let Some(session_id) = p.session_id.clone() else { return };
    let current = CurrentSession {
        session_id,
        store: store.map(Path::to_path_buf),
        cwd: p.work_dir().map(PathBuf::from),
        kitty_window_id: std::env::var("KITTY_WINDOW_ID").ok().filter(|s| !s.is_empty()),
        model: p.model_name().map(str::to_string),
        ts: CurrentSession::now_ts(),
        cursor_dir: None,
        ctxviz_version: None,
    };
    let _ = catch_unwind(|| current.write());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Stand-in for the binary's `Cli`, which lives in main.rs.
    #[derive(Parser)]
    #[command(name = "ctxviz")]
    enum Cli {
        Statusline(Args),
    }

    fn parse(argv: &[&str]) -> Args {
        let Cli::Statusline(a) = Cli::try_parse_from(argv).unwrap();
        a
    }

    #[test]
    fn parses_rows_and_bar() {
        let a = parse(&["ctxviz", "statusline", "--rows", "1", "--bar", "labeled", "--layout", "classic"]);
        assert_eq!((a.rows, a.bar, a.layout), (Rows::One, BarStyle::Labeled, Layout::Classic));
        let a = parse(&["ctxviz", "statusline"]);
        assert_eq!((a.rows, a.bar, a.layout), (Rows::Two, BarStyle::Plain, Layout::Wide));
        assert!(Cli::try_parse_from(["ctxviz", "statusline", "--rows", "3"]).is_err());
        assert!(!a.no_git);
        assert!(parse(&["ctxviz", "statusline", "--no-git"]).no_git);
    }

    #[test]
    fn no_git_env_values() {
        for on in ["1", "yes", "true", " 1 "] {
            assert!(no_git_env(Some(on)), "{on:?}");
        }
        for off in [None, Some(""), Some("0"), Some("false"), Some("FALSE")] {
            assert!(!no_git_env(off), "{off:?}");
        }
    }

    #[test]
    fn payload_over_one_mib_is_rejected() {
        let max = MAX_PAYLOAD_BYTES as usize;
        let ok = format!("{{\"session_id\":\"{}\"}}", "x".repeat(max - 17));
        assert_eq!(ok.len(), max);
        assert_eq!(read_payload(ok.as_bytes()).unwrap().len(), max);
        let big = format!("{{\"session_id\":\"{}\"}}", "x".repeat(max));
        let err = read_payload(big.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err:#}");
        // An endless stream stops at the cap instead of growing forever.
        assert!(read_payload(std::io::repeat(b' ')).is_err());
    }

    #[test]
    fn undecodable_root_is_payload_only_not_an_error() {
        let dir = crate::test_support::TempDir::new("statusline-corrupt-root");
        let b = crate::test_support::StoreBuilder::create(&dir.join("store.db"));
        b.blob("r1", &[0x0b; 1000]);
        b.meta(&serde_json::json!({ "agentId": "abc", "latestRootBlobId": "r1" }));
        let store = Store::open(b.finish()).unwrap();
        match lookup_in(&store).unwrap() {
            Lookup::Undecodable(e) => assert!(format!("{e:#}").contains("decoding root token details"), "{e:#}"),
            Lookup::Details(_) => panic!("decoded a corrupt root"),
            Lookup::Missing => panic!("corrupt root read as missing"),
        }
        // The payload-only row (no breakdown) still renders.
        let p = StatusPayload::parse(r#"{"session_id":"abc","context_window":{"used_percentage":42}}"#).unwrap();
        let opts =
            RenderOpts { rows: Rows::default(), bar: BarStyle::default(), layout: Layout::default(), no_git: true };
        assert!(!sl::render_with(&p, None, &Trend::default(), opts).trim().is_empty());
    }

    #[test]
    fn dangling_root_is_still_transient() {
        let dir = crate::test_support::TempDir::new("statusline-dangling-root");
        let b = crate::test_support::StoreBuilder::create(&dir.join("store.db"));
        b.meta(&serde_json::json!({ "agentId": "abc", "latestRootBlobId": "r1" }));
        let store = Store::open(b.finish()).unwrap();
        assert!(lookup_in(&store).is_err());
    }

    #[test]
    fn resolve_store_rejects_bad_session_ids() {
        for sid in ["..", "../../etc", "a/b", ".hidden", "a\\b", "", "x\u{1b}"] {
            let p = StatusPayload { session_id: Some(sid.into()), ..Default::default() };
            assert_eq!(resolve_store(&p), None, "{sid:?}");
        }
    }
}
