//! ratatui side pane: header, Explorer | Timeline, Detail, footer.

pub mod app;
pub mod panels;
pub mod watch;

use crate::kitty::{self, Band, ProgressState};
use crate::model::snapshot::Snapshot;
use crate::model::timeline::{self, Timeline};
use crate::render::statusline::{compact, percent_text};
use crate::theme::Role;
use app::{Action, App};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Key event poll interval; the watch channel is drained between polls.
pub const TICK: Duration = Duration::from_millis(50);

/// Restores the terminal on drop (normal exit, error, or unwind). The panic
/// hook installed by `ratatui::init` restores before the message prints.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
        let _ = kitty::emit_progress(ProgressState::Clear);
    }
}

/// Debug-only test hook: compiled into debug builds only, so release builds
/// ignore it. Overrides the percent used for the notification band (same
/// variable the status line honors), e.g. `CTXVIZ_FORCE_PERCENT=92`. The
/// header still shows the real percent.
#[cfg(debug_assertions)]
const FORCE_PERCENT_ENV: &str = "CTXVIZ_FORCE_PERCENT";

/// How long the Kitty progress indicator stays on after a new checkpoint.
pub const PROGRESS_WINDOW: Duration = Duration::from_secs(2);

/// Progress state for a checkpoint that arrived `age` ago (`None`: none yet).
pub fn progress_for(age: Option<Duration>) -> ProgressState {
    match age {
        Some(a) if a < PROGRESS_WINDOW => ProgressState::Indeterminate,
        _ => ProgressState::Clear,
    }
}

/// Outcome of re-evaluating the usage band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandDecision {
    pub band: Band,
    /// Band differs from `prev`: record it in the timeline cache.
    pub persist: bool,
    /// Rising crossing into Warn/Critical at or above `threshold`.
    pub notify: bool,
}

/// `prev` is the band already recorded (cache or memory); `threshold` is the
/// `n` override (`None` = notifications off).
pub fn band_decision(prev: Option<Band>, percent: f64, threshold: Option<f64>) -> BandDecision {
    let band = kitty::band_for(percent, prev);
    let rising = band != Band::Ok && prev.is_none_or(|p| band.as_u8() > p.as_u8());
    let notify = rising && threshold.is_some_and(|t| percent >= t);
    BandDecision { band, persist: prev != Some(band), notify }
}

/// Kitty side effects: OSC 99 band notifications and OSC 9;4 progress. All
/// writes are no-ops (errors swallowed) outside Kitty.
#[derive(Default)]
struct KittyFx {
    /// Band last recorded; seeded from the cache on the first snapshot.
    band: Option<Band>,
    seeded: bool,
    last_root: Option<String>,
    arrived_at: Option<Instant>,
    progress: Option<ProgressState>,
}

impl KittyFx {
    fn reset(&mut self) {
        *self = KittyFx { progress: self.progress, ..KittyFx::default() };
    }

    fn on_snapshot(&mut self, snap: &Snapshot, threshold: Option<f64>, now: Instant) {
        if snap.root_blob_id.is_some() && self.last_root.is_some() && self.last_root != snap.root_blob_id {
            self.arrived_at = Some(now);
        }
        self.last_root = snap.root_blob_id.clone();

        if !self.seeded {
            self.seeded = true;
            self.band = snap.timeline.as_ref().and_then(|t| t.last_notified_band).and_then(Band::from_u8);
        }
        // Debug-only test hook; see FORCE_PERCENT_ENV.
        #[cfg(debug_assertions)]
        let forced = std::env::var(FORCE_PERCENT_ENV).ok().and_then(|v| v.trim().parse::<f64>().ok());
        #[cfg(not(debug_assertions))]
        let forced: Option<f64> = None;
        let Some(pct) = forced.or(snap.percent) else { return };
        let d = band_decision(self.band, pct, threshold);
        if !d.persist {
            return;
        }
        self.band = Some(d.band);
        if d.notify {
            let name = crate::text::sanitize_single_line(snap.name.as_deref().unwrap_or("chat"));
            let body =
                format!("{name} context at {} ({}/{})", percent_text(pct), compact(snap.used), compact(snap.max));
            let _ = kitty::emit_osc99("ctxviz", &body);
        }
        if let Some(agent) = snap.agent_id.as_deref() {
            // Load fresh so the status line's newer `last_rowid` survives.
            let path = timeline::cache_path(agent);
            if let Some(mut tl) = Timeline::load(&path).or_else(|| snap.timeline.clone()) {
                tl.last_notified_band = Some(d.band.as_u8());
                let _ = tl.save(&path);
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        let want = progress_for(self.arrived_at.map(|t| now.saturating_duration_since(t)));
        if self.progress != Some(want) {
            // Startup: nothing to clear unless a checkpoint is in flight.
            if self.progress.is_some() || want != ProgressState::Clear {
                let _ = kitty::emit_progress(want);
            }
            self.progress = Some(want);
        }
    }
}

/// Header, Explorer, Timeline, Detail, footer areas.
pub struct Areas {
    pub header: Rect,
    pub tree: Rect,
    pub timeline: Rect,
    pub detail: Rect,
    pub footer: Rect,
}

/// Layout overrides from the CLI and the `[`/`]` keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutOpts {
    /// Exact explorer width in columns (wins over `explorer_pct`).
    pub explorer_cols: Option<u16>,
    /// Explorer width as a percent of the body width (`--split`).
    pub explorer_pct: Option<u8>,
    /// Exact timeline height in rows (`--timeline-rows`).
    pub timeline_rows: Option<u16>,
}

/// Narrowest explorer an override may request.
pub const EXPLORER_MIN_COLS: u16 = 24;
/// Default explorer floor (before the 35% share takes over).
const EXPLORER_DEFAULT_COLS: u16 = 36;
/// Columns the right side (timeline, detail) keeps by default.
const RIGHT_MIN_COLS: u16 = 20;
/// Rows the detail panel keeps below the timeline.
const DETAIL_MIN_ROWS: u16 = 3;

fn pct_of(n: u16, pct: u32) -> u16 {
    (n as u32 * pct / 100) as u16
}

/// Clamp a requested explorer width to 24..=80% of `body_w`.
pub fn clamp_explorer(cols: u16, body_w: u16) -> u16 {
    cols.max(EXPLORER_MIN_COLS).min(pct_of(body_w, 80))
}

/// Effective explorer width for a body `body_w` columns wide.
pub fn explorer_width(body_w: u16, opts: &LayoutOpts) -> u16 {
    if let Some(c) = opts.explorer_cols {
        return clamp_explorer(c, body_w);
    }
    if let Some(p) = opts.explorer_pct {
        return clamp_explorer(pct_of(body_w, p as u32), body_w);
    }
    // max(36, 35%), but the right side keeps 20 columns when it can.
    EXPLORER_DEFAULT_COLS.max(pct_of(body_w, 35)).min(body_w.saturating_sub(RIGHT_MIN_COLS))
}

/// Effective timeline height for a right column `body_h` rows tall.
pub fn timeline_height(body_h: u16, opts: &LayoutOpts) -> u16 {
    let max = body_h.saturating_sub(DETAIL_MIN_ROWS);
    match opts.timeline_rows {
        Some(r) => r.min(max),
        None => pct_of(body_h, 35).clamp(10, 16).min(max),
    }
}

pub fn layout(area: Rect, opts: &LayoutOpts) -> Areas {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(8), Constraint::Length(1)]).areas(area);
    let ew = explorer_width(body.width, opts);
    let [tree, right] = Layout::horizontal([Constraint::Length(ew), Constraint::Min(0)]).areas(body);
    let th = timeline_height(right.height, opts);
    let [timeline, detail] = Layout::vertical([Constraint::Length(th), Constraint::Min(0)]).areas(right);
    Areas { header, tree, timeline, detail, footer }
}

pub fn draw(f: &mut Frame, app: &App) {
    let a = layout(f.area(), &app.layout_opts());
    app.body_width.set(f.area().width);
    panels::header::draw(f, a.header, app);
    panels::tree::draw(f, a.tree, app);
    panels::timeline::draw(f, a.timeline, app);
    panels::detail::draw(f, a.detail, app);
    let help = if app.filter_input {
        " type to filter · enter keep · esc clear".to_string()
    } else {
        format!(
            " q quit · tab panel · j/k move · enter/l expand · h collapse · / filter · g/G · [ ] width · o pager · n notify {}",
            app.notify_threshold.label()
        )
    };
    let mut spans = vec![Span::styled(help, panels::th().fg(Role::Ink2))];
    if let Some(s) = &app.status {
        spans.push(Span::raw("  "));
        // Status text can echo labels and subprocess stderr.
        spans.push(Span::styled(crate::text::sanitize_single_line(s).into_owned(), panels::th().fg(Role::Accent)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), a.footer);
}

/// Startup options from `ctxviz tui`.
#[derive(Debug, Clone, Default)]
pub struct TuiOptions {
    /// `--split`: explorer width as a percent of the body (1..=100).
    pub split_pct: Option<u8>,
    /// `--timeline-rows`: exact timeline height.
    pub timeline_rows: Option<u16>,
    /// `tui.json` to restore and save the `[`/`]` width; `None` disables it.
    pub state_path: Option<PathBuf>,
}

/// Build the app from startup options. `--split` wins over a width saved in
/// `state_path`, which wins over the default; a missing or corrupt file is
/// ignored. The saved width is re-clamped by `explorer_width` on every layout.
pub fn startup_app(store: Option<PathBuf>, opts: &TuiOptions) -> App {
    let mut app = App::new();
    app.store = store;
    app.explorer_pct = opts.split_pct;
    app.timeline_rows = opts.timeline_rows;
    app.state_path = opts.state_path.clone();
    if opts.split_pct.is_none() {
        if let Some(path) = &opts.state_path {
            app.explorer_cols = crate::session::TuiState::read_from(path).and_then(|s| s.explorer_cols);
        }
    }
    app
}

/// Run the app on `store`, or follow `current.json` when `None`.
pub fn run(store: Option<PathBuf>, opts: TuiOptions) -> anyhow::Result<()> {
    let rx = watch::spawn(store.clone());
    let mut app = startup_app(store, &opts);
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    // Clear progress on panic too (runs before ratatui's restoring hook).
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = kitty::emit_progress(ProgressState::Clear);
        prev_hook(info);
    }));
    let mut fx = KittyFx::default();
    loop {
        while let Ok(ev) = rx.try_recv() {
            match &ev {
                watch::WatchEvent::Snapshot(s) => fx.on_snapshot(s, app.notify_threshold.percent(), Instant::now()),
                watch::WatchEvent::Switched(_) => fx.reset(),
                watch::WatchEvent::Error(_) => {}
            }
            app.on_watch(ev);
        }
        fx.tick(Instant::now());
        terminal.draw(|f| draw(f, &app))?;
        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Release {
                    match app.on_key(k) {
                        Action::Quit => break,
                        Action::OpenPager { label, text } => app.status = Some(app::open_in_pager(&label, &text)),
                        Action::None => {}
                    }
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::snapshot::{Snapshot, SnapshotOptions};
    use crate::store::Store;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect()
    }

    /// (cached band, percent, threshold, expected band, persist, notify).
    type BandCase = (Option<Band>, f64, Option<f64>, Band, bool, bool);

    #[test]
    fn band_crossing_table() {
        use Band::*;
        // (cached band, percent, threshold) -> (band, persist, notify)
        let cases: &[BandCase] = &[
            (None, 50.0, Some(80.0), Ok, true, false),
            (None, 92.0, Some(80.0), Critical, true, true),
            (Some(Ok), 50.0, Some(80.0), Ok, false, false),
            (Some(Ok), 81.0, Some(80.0), Warn, true, true),
            (Some(Ok), 91.0, Some(80.0), Critical, true, true),
            (Some(Warn), 85.0, Some(80.0), Warn, false, false),
            (Some(Warn), 90.0, Some(80.0), Critical, true, true),
            // Already notified (status line or previous run): no repeat.
            (Some(Critical), 92.0, Some(80.0), Critical, false, false),
            (Some(Critical), 87.0, Some(80.0), Critical, false, false),
            // Falling bands are recorded but never notify.
            (Some(Critical), 80.0, Some(80.0), Warn, true, false),
            (Some(Warn), 70.0, Some(80.0), Ok, true, false),
            // Threshold override gates the popup, not the band.
            (Some(Ok), 85.0, Some(90.0), Warn, true, false),
            (Some(Warn), 96.0, Some(95.0), Critical, true, true),
            (Some(Warn), 92.0, Some(95.0), Critical, true, false),
            (Some(Ok), 99.0, None, Critical, true, false),
        ];
        for &(prev, pct, th, band, persist, notify) in cases {
            let d = band_decision(prev, pct, th);
            assert_eq!(d, BandDecision { band, persist, notify }, "prev {prev:?} pct {pct} th {th:?}");
        }
    }

    #[test]
    fn progress_timing() {
        assert_eq!(progress_for(None), ProgressState::Clear);
        assert_eq!(progress_for(Some(Duration::ZERO)), ProgressState::Indeterminate);
        assert_eq!(progress_for(Some(Duration::from_millis(1999))), ProgressState::Indeterminate);
        assert_eq!(progress_for(Some(PROGRESS_WINDOW)), ProgressState::Clear);
        assert_eq!(progress_for(Some(Duration::from_secs(60))), ProgressState::Clear);
    }

    #[test]
    fn n_cycles_threshold() {
        let mut app = App::new();
        let mut seen = vec![app.notify_threshold.label()];
        for _ in 0..4 {
            app.on_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('n')));
            seen.push(app.notify_threshold.label());
        }
        assert_eq!(seen, ["80%", "90%", "95%", "off", "80%"]);
    }

    fn opts() -> LayoutOpts {
        LayoutOpts::default()
    }

    #[test]
    fn layout_defaults() {
        let a = layout(Rect::new(0, 0, 100, 30), &opts());
        assert_eq!((a.header.height, a.footer.height), (2, 1));
        assert_eq!(a.tree.width, 36);
        assert_eq!(a.tree.x, 0);
        assert_eq!(a.timeline.x, 36);
        assert_eq!(a.timeline.width, 64);
        assert_eq!(a.tree.height, 27);
        // 35% of 27 = 9, clamped up to 10.
        assert_eq!((a.timeline.height, a.detail.height), (10, 17));
        let a = layout(Rect::new(0, 0, 118, 40), &opts());
        assert_eq!(a.tree.width, 41);
        let a = layout(Rect::new(0, 0, 120, 40), &opts());
        assert_eq!(a.timeline.height, 12);
        assert_eq!(a.detail.y, a.timeline.y + 12);
        // Tall body: capped at 16.
        let a = layout(Rect::new(0, 0, 120, 80), &opts());
        assert_eq!(a.timeline.height, 16);
    }

    #[test]
    fn layout_tiny_terminal() {
        // Right side keeps 20 columns; explorer gets what is left.
        let a = layout(Rect::new(0, 0, 50, 12), &opts());
        assert_eq!((a.tree.width, a.timeline.width), (30, 20));
        // Body of 9 rows: timeline capped so detail keeps 3.
        assert_eq!((a.timeline.height, a.detail.height), (6, 3));
        let a = layout(Rect::new(0, 0, 10, 5), &opts());
        assert_eq!(a.tree.width, 0);
    }

    #[test]
    fn layout_overrides_clamped() {
        let area = Rect::new(0, 0, 100, 30);
        let o = LayoutOpts { explorer_cols: Some(50), timeline_rows: Some(5), ..opts() };
        let a = layout(area, &o);
        assert_eq!((a.tree.width, a.timeline.height), (50, 5));
        let o = LayoutOpts { explorer_cols: Some(3), timeline_rows: Some(500), ..opts() };
        let a = layout(area, &o);
        assert_eq!((a.tree.width, a.timeline.height, a.detail.height), (24, 24, 3));
        let o = LayoutOpts { explorer_cols: Some(99), ..opts() };
        assert_eq!(layout(area, &o).tree.width, 80);
        // Percent split, clamped the same way; exact columns win.
        let o = LayoutOpts { explorer_pct: Some(50), ..opts() };
        assert_eq!(layout(area, &o).tree.width, 50);
        let o = LayoutOpts { explorer_pct: Some(100), ..opts() };
        assert_eq!(layout(area, &o).tree.width, 80);
        let o = LayoutOpts { explorer_pct: Some(1), ..opts() };
        assert_eq!(layout(area, &o).tree.width, 24);
        let o = LayoutOpts { explorer_cols: Some(30), explorer_pct: Some(50), ..opts() };
        assert_eq!(layout(area, &o).tree.width, 30);
    }

    #[test]
    fn footer_lists_keys() {
        let app = App::new();
        let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let s = screen(&term);
        assert_eq!(
            s.last().unwrap().trim_end(),
            " q quit · tab panel · j/k move · enter/l expand · h collapse · / filter · g/G · [ ] width · o pager · n notify 80%"
        );
        assert_eq!(app.body_width.get(), 140);
    }

    #[test]
    fn draws_sample_frame() {
        let mut app = App::new();
        app.set_snapshot(app::tests::snapshot(app::tests::sample_tree()));
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let s = screen(&term);
        assert!(s[0].contains('█') && s[0].contains("75%"), "{}", s[0]);
        assert!(s[1].contains("Test chat"), "{}", s[1]);
        assert!(s.iter().any(|l| l.contains("▾ ■ Conversation") && l.contains("~100")));
        assert!(s.iter().any(|l| l.contains("Timeline")));
        assert!(s.last().unwrap().contains("q quit"));
    }

    /// Draw a full frame of `store` and check the header, title, and tree.
    fn check_store_frame(store: &Store) -> Vec<String> {
        // timeline: false keeps the test from writing a cache in the state dir.
        let snap = Snapshot::load(store, SnapshotOptions { tree: true, timeline: false }).unwrap().unwrap();
        let pct = crate::render::statusline::percent_text(snap.percent.unwrap());
        let mut app = App::new();
        app.set_snapshot(snap);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let s = screen(&term);
        assert!(s[0].contains(&pct), "header {:?} lacks {pct}", s[0]);
        assert!(s[0].contains('█'), "no bar: {}", s[0]);
        let want = app.snapshot.as_ref().and_then(|s| s.name.clone()).unwrap_or_else(|| "(unnamed)".into());
        assert!(s[1].contains(&want), "title row {:?} lacks {want}", s[1]);
        let tree_rows = s.iter().filter(|l| l.contains('▾') || l.contains('▸')).count();
        assert!(tree_rows >= 1, "no tree rows:\n{}", s.join("\n"));
        assert!(s.iter().any(|l| l.contains("Detail")));
        // Moving the selection keeps drawing.
        app.on_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('j')));
        term.draw(|f| draw(f, &app)).unwrap();
        s
    }

    #[test]
    fn draws_synthetic_store_frame() {
        use crate::test_support::{synthetic, SyntheticStore};
        let fx = SyntheticStore::new("tui-frame");
        let s = check_store_frame(&Store::open(&fx.path).unwrap());
        let all = s.join("\n");
        assert!(s[1].contains(synthetic::NAME), "{all}");
        // Every category with tokens is listed in the explorer.
        for label in ["Conversation", "Tool definitions", "System prompt", "Rules", "Skills"] {
            assert!(s.iter().any(|l| l.contains(&format!("■ {label}"))), "no {label} row:\n{all}");
        }
    }

    /// Opt-in: `CTXVIZ_REAL_FIXTURE=1` requires `tests/fixtures/store.db`.
    #[test]
    fn draws_real_fixture_frame() {
        let Some(db) = crate::test_support::real_fixture("store.db") else { return };
        check_store_frame(&Store::open(&db).unwrap());
    }

    /// `tui.json` holding `contents` in a fresh temp dir (removed with the guard).
    fn state_file(name: &str, contents: &str) -> (crate::test_support::TempDir, PathBuf) {
        let dir = crate::test_support::TempDir::new(&format!("tui-{name}"));
        let path = dir.join("tui.json");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn startup_restores_saved_width() {
        let (_dir, path) = state_file("restore", r#"{"explorer_cols": 50, "future_field": 1}"#);
        let opts = TuiOptions { state_path: Some(path.clone()), ..TuiOptions::default() };
        let app = startup_app(None, &opts);
        assert_eq!((app.explorer_cols, app.explorer_pct), (Some(50), None));
        assert_eq!(app.state_path.as_deref(), Some(path.as_path()));
        // Re-clamped against the current width on layout.
        assert_eq!(explorer_width(40, &app.layout_opts()), 32);
    }

    #[test]
    fn split_beats_saved_width() {
        let (_dir, path) = state_file("split", r#"{"explorer_cols": 50}"#);
        let opts = TuiOptions { split_pct: Some(40), state_path: Some(path.clone()), ..TuiOptions::default() };
        let app = startup_app(None, &opts);
        assert_eq!((app.explorer_cols, app.explorer_pct), (None, Some(40)));
    }

    #[test]
    fn corrupt_or_missing_state_is_ignored() {
        let (dir, path) = state_file("corrupt", "{not json");
        let opts = TuiOptions { state_path: Some(path.clone()), ..TuiOptions::default() };
        let app = startup_app(None, &opts);
        assert_eq!((app.explorer_cols, app.explorer_pct), (None, None));
        drop(dir);
        let app = startup_app(None, &opts);
        assert_eq!(app.explorer_cols, None);
    }
}
