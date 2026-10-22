//! Timeline panel: used tokens per checkpoint as a line chart (compactions and
//! the selected checkpoint marked), a summary of the selected checkpoint, and a
//! compact per-checkpoint table. Short panels fall back to a sparkline.
//! Data: `app.timeline()`, `app.selected_checkpoint()`, `app.timeline_selected`.
//!
//! Layout: at `SIDE_BY_SIDE_MIN_WIDTH` inner columns and up, the summary and
//! chart share the left half and the table fills the right half, separated by
//! a one-cell rule. Narrower panels stack summary, chart, and table.
//!
//! Compactions are drawn as `▼`: in the summary, in the table, as a scatter
//! point on the chart, and on the chart's x axis row. Ratatui's `Chart` cannot
//! place glyphs on its axes, so `axis_marker_cells` mirrors its layout (y-label
//! width, axis column, braille x mapping) and the axis cells are overwritten
//! after the chart renders. The overwrite is skipped below `MIN_AXIS_MARK_WIDTH`.

use crate::commands::snapshot::human;
use crate::model::categories;
use crate::model::timeline::{Checkpoint, Timeline};
use crate::theme::Role;
use crate::tui::app::{App, Focus};
use crate::tui::panels::{panel, th};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Cell, Chart, Dataset, GraphType, Paragraph, Row, Sparkline, Table, TableState},
    Frame,
};

/// Below this many inner rows the chart and table give way to a sparkline.
const MIN_CHART_ROWS: u16 = 8;
/// From this many inner columns on, the chart and table sit side by side.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 60;
/// Charts narrower than this get no `▼` overwrite on the x axis.
const MIN_AXIS_MARK_WIDTH: u16 = 12;
/// Compaction glyph used across the panel.
const COMPACTION: &str = "▼";
/// Selected-row marker in the table.
const SELECTED: &str = "◀";

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Timeline;
    let Some(tl) = app.timeline().filter(|t| !t.checkpoints.is_empty()) else {
        let block = panel("Timeline", focused);
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(Span::styled("no checkpoints", th().fg(Role::Ink2))), inner);
        return;
    };
    let block = panel(&title(tl), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let sel = selected_index(app, tl);
    let summary = |w: u16| summary_line(tl, sel, w);

    if inner.height < MIN_CHART_ROWS {
        // Summary on top when there is room, sparkline below.
        let spark_area = if inner.height >= 2 {
            let [top, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
            f.render_widget(Paragraph::new(summary(top.width)), top);
            rest
        } else {
            inner
        };
        let data = tl.recent_used(spark_area.width as usize);
        let max = tl.checkpoints.iter().map(|c| c.max_tokens.max(c.used_tokens)).max().unwrap_or(1).max(1);
        f.render_widget(Sparkline::default().data(&data).max(max).style(th().fg(Role::Text)), spark_area);
        return;
    }

    if inner.width >= SIDE_BY_SIDE_MIN_WIDTH {
        let [left, sep, right] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        let [top, chart_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(left);
        f.render_widget(Paragraph::new(summary(top.width)), top);
        draw_chart(f, chart_area, tl, sel);
        draw_separator(f, sep, area);
        // One blank cell between the rule and the table.
        let right = Rect { x: right.x + 1, width: right.width.saturating_sub(1), ..right };
        draw_table(f, right, tl, sel, focused);
        return;
    }

    // Table: header plus up to all rows, but leave the chart at least 6 rows.
    let body = inner.height - 1; // minus summary
    let table_h = (tl.checkpoints.len() as u16 + 1).min(body / 2).max(2);
    let [top, chart_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(table_h)]).areas(inner);
    f.render_widget(Paragraph::new(summary(top.width)), top);
    draw_chart(f, chart_area, tl, sel);
    draw_table(f, table_area, tl, sel, focused);
}

/// Vertical rule in `sep` (one column wide), joined to the panel's bottom border.
fn draw_separator(f: &mut Frame, sep: Rect, panel_area: Rect) {
    let style = th().fg(Role::Border);
    let buf = f.buffer_mut();
    for y in sep.top()..sep.bottom() {
        buf[(sep.x, y)].set_symbol("│").set_style(style);
    }
    let bottom = panel_area.bottom().saturating_sub(1);
    if bottom >= sep.bottom() && buf.area.contains(ratatui::layout::Position::new(sep.x, bottom)) {
        buf[(sep.x, bottom)].set_symbol("┴");
    }
}

fn title(tl: &Timeline) -> String {
    let n = tl.checkpoints.len();
    let k = tl.checkpoints.iter().filter(|c| c.compaction).count();
    format!("Timeline · {n} ckpt{} · {k} compaction{}", if n == 1 { "" } else { "s" }, if k == 1 { "" } else { "s" })
}

fn selected_index(app: &App, tl: &Timeline) -> usize {
    let last = tl.checkpoints.len().saturating_sub(1);
    app.timeline_selected.map_or(last, |i| i.min(last))
}

/// `+1.2K` / `-340` / `0`.
fn signed(d: i64) -> String {
    let mag = human(d.unsigned_abs());
    match d.signum() {
        1 => format!("+{mag}"),
        -1 => format!("-{mag}"),
        _ => mag,
    }
}

fn delta_at(tl: &Timeline, i: usize) -> Option<i64> {
    i.checked_sub(1).and_then(|j| tl.deltas().get(j).copied())
}

fn delta_style(d: i64) -> Style {
    if d < 0 {
        th().fg(Role::Compaction)
    } else {
        Style::default()
    }
}

/// `#7 15.6K/200.0K 8% Δ +1.2K · Conversation 9.1K · Rules 3.0K · ...`,
/// fitted to `width` columns: category groups are appended whole while they
/// fit, and `…` marks anything dropped or cut.
fn summary_line(tl: &Timeline, sel: usize, width: u16) -> Line<'static> {
    let width = width as usize;
    let c: &Checkpoint = &tl.checkpoints[sel];
    let mut spans = vec![Span::styled(format!("#{} ", sel + 1), th().fg(Role::Accent).add_modifier(Modifier::BOLD))];
    spans.push(Span::raw(format!("{}/{}", human(c.used_tokens), human(c.max_tokens))));
    if c.max_tokens > 0 {
        spans.push(Span::raw(format!(" {:.0}%", c.used_tokens as f64 / c.max_tokens as f64 * 100.0)));
    }
    if let Some(d) = delta_at(tl, sel) {
        spans.push(Span::styled(" Δ ", th().fg(Role::Ink2)));
        spans.push(Span::styled(signed(d), delta_style(d)));
    }
    if c.compaction {
        spans.push(Span::styled(" ▼ compaction", th().fg(Role::Compaction)));
    }
    let ellipsis = || Span::styled(ELLIPSIS, th().fg(Role::Ink2));
    let mut used: usize = spans.iter().map(Span::width).sum();
    if used > width {
        return Line::from(truncate_spans(spans, width));
    }
    let mut cats: Vec<&(String, u64)> = c.categories.iter().filter(|(_, n)| *n > 0).collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| categories::order(&a.0).cmp(&categories::order(&b.0))));
    for (id, n) in cats.into_iter().take(3) {
        let label =
            categories::label(id).map_or_else(|| crate::text::sanitize_single_line(id).into_owned(), str::to_string);
        let group = [
            Span::styled(" · ", th().fg(Role::Ink2)),
            Span::styled(label, Style::default().fg(th().category(id).color())),
            Span::raw(format!(" {}", human(*n))),
        ];
        let w: usize = group.iter().map(Span::width).sum();
        if used + w > width {
            if used + 2 <= width {
                spans.push(Span::raw(" "));
                spans.push(ellipsis());
            } else if used < width {
                spans.push(ellipsis());
            }
            break;
        }
        used += w;
        spans.extend(group);
    }
    Line::from(spans)
}

const ELLIPSIS: &str = "…";

/// Cut `spans` to `width` display columns, ending with `…`.
fn truncate_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    let budget = width - 1; // room for the ellipsis
    let mut used = 0;
    'outer: for span in spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let w = Span::raw(ch.to_string()).width();
            if used + w > budget {
                if !text.is_empty() {
                    out.push(Span::styled(text, span.style));
                }
                break 'outer;
            }
            used += w;
            text.push(ch);
        }
        out.push(Span::styled(text, span.style));
    }
    out.push(Span::styled(ELLIPSIS, th().fg(Role::Ink2)));
    out
}

fn draw_chart(f: &mut Frame, area: Rect, tl: &Timeline, sel: usize) {
    let cps = &tl.checkpoints;
    let used: Vec<(f64, f64)> = cps.iter().enumerate().map(|(i, c)| (i as f64, c.used_tokens as f64)).collect();
    let compactions: Vec<(f64, f64)> =
        cps.iter().enumerate().filter(|(_, c)| c.compaction).map(|(i, c)| (i as f64, c.used_tokens as f64)).collect();
    let max_tokens = cps.iter().map(|c| c.max_tokens).max().unwrap_or(0);
    let peak = cps.iter().map(|c| c.used_tokens).max().unwrap_or(0);
    let low = cps.iter().map(|c| c.used_tokens).min().unwrap_or(0);
    // Show the max line when it is near the data; otherwise zoom to the used
    // range so turn-by-turn growth stays visible (the summary carries max).
    let show_max = max_tokens > 0 && max_tokens as f64 <= peak as f64 * 1.5;
    let y_top = if show_max { max_tokens.max(peak) as f64 } else { (peak as f64 * 1.05).max(1.0) };
    let y_low = if show_max { 0.0 } else { (low as f64 * 0.9).floor() };
    let x_top = (cps.len().saturating_sub(1)).max(1) as f64;
    let max_line = [(0.0, max_tokens as f64), (x_top, max_tokens as f64)];
    let sel_c = &cps[sel];
    let sel_line = [(sel as f64, y_low), (sel as f64, sel_c.used_tokens as f64)];
    let sel_point = [(sel as f64, sel_c.used_tokens as f64)];

    let mut datasets = Vec::new();
    if show_max {
        datasets.push(
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(th().fg(Role::Muted))
                .data(&max_line),
        );
    }
    datasets.push(
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(th().fg(Role::Accent))
            .data(&sel_line),
    );
    datasets.push(
        Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(th().fg(Role::Text)).data(&used),
    );
    datasets.push(
        Dataset::default()
            .marker(Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(th().fg(Role::Compaction))
            .data(&compactions),
    );
    datasets.push(
        Dataset::default()
            .marker(Marker::Block)
            .graph_type(GraphType::Scatter)
            .style(th().fg(Role::Accent))
            .data(&sel_point),
    );

    let dim = th().fg(Role::Ink2);
    let x_labels = vec![Span::styled("1", dim), Span::styled(cps.len().to_string(), dim)];
    let top_label = if show_max { format!("max {}", human(y_top as u64)) } else { human(y_top as u64) };
    let y_labels = vec![
        Span::styled(human(y_low as u64), dim),
        Span::styled(human(((y_low + y_top) / 2.0) as u64), dim),
        Span::styled(top_label, dim),
    ];
    let label_w = y_labels.iter().map(Span::width).chain(x_labels.first().map(|l| l.width().saturating_sub(1))).max();
    let label_w = label_w.unwrap_or(0) as u16;
    let chart = Chart::new(datasets)
        .x_axis(Axis::default().style(dim).bounds([0.0, x_top]).labels(x_labels))
        .y_axis(Axis::default().style(dim).bounds([y_low, y_top]).labels(y_labels));
    f.render_widget(chart, area);

    let xs: Vec<f64> = cps.iter().enumerate().filter(|(_, c)| c.compaction).map(|(i, _)| i as f64).collect();
    let style = th().fg(Role::Compaction);
    let buf = f.buffer_mut();
    for (x, y) in axis_marker_cells(area, label_w, x_top, &xs) {
        if buf.area.contains(ratatui::layout::Position::new(x, y)) {
            buf[(x, y)].set_symbol(COMPACTION).set_style(style);
        }
    }
}

/// Cells on the x axis row of a `Chart` rendered into `area` (with both axes
/// labeled, the x axis spanning `[0, x_top]`, braille datasets) that sit under
/// data x values `xs`. `label_w` is the widest y label (or first x label minus
/// one). Mirrors ratatui 0.29's chart layout; empty when the chart is narrower
/// than `MIN_AXIS_MARK_WIDTH` or too short to have an axis row.
fn axis_marker_cells(area: Rect, label_w: u16, x_top: f64, xs: &[f64]) -> Vec<(u16, u16)> {
    if area.width < MIN_AXIS_MARK_WIDTH || area.height < 3 || x_top <= 0.0 {
        return Vec::new();
    }
    let axis_y = area.bottom() - 2;
    let graph_x = area.x + label_w.min(area.width / 3) + 1;
    let graph_w = area.right().saturating_sub(graph_x);
    if graph_w == 0 {
        return Vec::new();
    }
    let dots = f64::from(graph_w) * 2.0;
    xs.iter()
        .filter(|x| (0.0..=x_top).contains(*x))
        .map(|x| {
            let dot = (x * (dots - 1.0) / x_top) as u16;
            (graph_x + (dot / 2).min(graph_w - 1), axis_y)
        })
        .collect()
}

fn draw_table(f: &mut Frame, area: Rect, tl: &Timeline, sel: usize, focused: bool) {
    let deltas = tl.deltas();
    let rows: Vec<Row> = tl
        .checkpoints
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let d = i.checked_sub(1).and_then(|j| deltas.get(j).copied());
            let (dt, ds) = match d {
                Some(d) => (signed(d), delta_style(d)),
                None => (String::new(), Style::default()),
            };
            Row::new(vec![
                Cell::from(Span::styled((i + 1).to_string(), th().fg(Role::Ink2))),
                Cell::from(c.message_count.to_string()),
                Cell::from(human(c.used_tokens)),
                Cell::from(Span::styled(dt, ds)),
                Cell::from(Span::styled(if c.compaction { COMPACTION } else { "" }, th().fg(Role::Compaction))),
                Cell::from(Span::styled(if i == sel { SELECTED } else { "" }, th().fg(Role::Accent))),
            ])
        })
        .collect();
    let header =
        Row::new(["#", "msgs", "used", "Δ", COMPACTION, ""]).style(th().fg(Role::Ink2).add_modifier(Modifier::BOLD));
    let widths = [
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(2),
        Constraint::Length(2),
    ];
    let table = Table::new(rows, widths).header(header).row_highlight_style(th().selection(focused));
    let mut state = TableState::default().with_selected(Some(sel));
    f.render_stateful_widget(table, area, &mut state);
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

    /// An app showing `store`. The timeline comes from a direct scan, so no
    /// cache is written and no process-wide state dir is needed.
    fn store_app(store: &Store) -> App {
        let mut snap = Snapshot::load(store, SnapshotOptions { tree: false, timeline: false }).unwrap().unwrap();
        snap.timeline = Some(Timeline::scan(store).unwrap());
        let mut app = App::new();
        app.snapshot = Some(snap);
        app
    }

    fn check_draws_timeline(mut app: App) {
        let tl = app.timeline().unwrap();
        let last_msgs = tl.checkpoints.last().unwrap().message_count.to_string();
        let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
        term.draw(|f| draw(f, f.area(), &app)).unwrap();
        let s = screen(&term);
        assert!(s[0].contains("ckpts"), "title: {}", s[0]);
        assert!(s.iter().any(|l| l.contains("msgs")), "no header:\n{}", s.join("\n"));
        assert!(s.iter().any(|l| l.contains(&format!(" {last_msgs} "))), "no {last_msgs}:\n{}", s.join("\n"));
        // Moving the selection keeps drawing.
        app.timeline_selected = Some(0);
        term.draw(|f| draw(f, f.area(), &app)).unwrap();
        assert!(screen(&term)[1].contains("#1 "));
    }

    fn check_tiny_rects(app: &App) {
        for (w, h) in [(60, 3), (10, 3), (10, 12), (3, 3), (1, 1)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, f.area(), app)).unwrap();
        }
    }

    fn synthetic_store_app(name: &str) -> (crate::test_support::SyntheticStore, App) {
        let fx = crate::test_support::SyntheticStore::new(name);
        let app = store_app(&Store::open(&fx.path).unwrap());
        (fx, app)
    }

    #[test]
    fn draws_synthetic_store_timeline() {
        let (_fx, app) = synthetic_store_app("panel-draw");
        let n = crate::test_support::synthetic::CHECKPOINTS.len();
        assert_eq!(app.timeline().unwrap().checkpoints.len(), n);
        let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
        term.draw(|f| draw(f, f.area(), &app)).unwrap();
        let s = screen(&term);
        assert!(s[0].contains(&format!("{n} ckpts")), "title: {}", s[0]);
        check_draws_timeline(app);
    }

    #[test]
    fn tiny_rects_fall_back() {
        let (_fx, app) = synthetic_store_app("panel-tiny");
        check_tiny_rects(&app);
        let empty = App::new();
        let mut term = Terminal::new(TestBackend::new(10, 3)).unwrap();
        term.draw(|f| draw(f, f.area(), &empty)).unwrap();
    }

    /// Opt-in: `CTXVIZ_REAL_FIXTURE=1` requires `tests/fixtures/store.db`.
    #[test]
    fn draws_real_fixture_timeline() {
        let Some(db) = crate::test_support::real_fixture("store.db") else { return };
        let app = store_app(&Store::open(&db).unwrap());
        check_tiny_rects(&app);
        check_draws_timeline(app);
    }

    /// Six checkpoints growing toward 30K with a compaction at index 3.
    fn synthetic_app() -> App {
        let used = [20_000u64, 24_000, 28_000, 12_000, 15_000, 18_000];
        let checkpoints = used
            .iter()
            .enumerate()
            .map(|(i, &u)| Checkpoint {
                rowid: i as i64,
                message_count: 2 * (i + 1),
                used_tokens: u,
                max_tokens: 200_000,
                compaction: i == 3,
                ..Checkpoint::default()
            })
            .collect();
        let mut snap = crate::tui::app::tests::snapshot(Default::default());
        snap.timeline = Some(Timeline { checkpoints, ..Timeline::default() });
        let mut app = App::new();
        app.snapshot = Some(snap);
        app
    }

    fn render(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, f.area(), app)).unwrap();
        screen(&term)
    }

    /// `synthetic_app` with three categories on every checkpoint.
    fn app_with_categories() -> App {
        let mut app = synthetic_app();
        let tl = app.snapshot.as_mut().unwrap().timeline.as_mut().unwrap();
        for c in &mut tl.checkpoints {
            c.categories = vec![
                ("conversation".into(), 9_000),
                ("summarized_conversation".into(), 5_000),
                ("rules".into(), 3_000),
            ];
        }
        app
    }

    /// The summary row's text inside the panel borders.
    fn summary_row(s: &[String]) -> String {
        s[1].trim_start_matches('│').trim_end_matches('│').trim_end().to_string()
    }

    #[test]
    fn summary_ellipsizes_when_narrow() {
        let app = app_with_categories();
        let s = render(&app, 50, 12);
        let row = summary_row(&s);
        assert!(row.ends_with('…'), "no ellipsis: {row:?}");
        assert!(row.contains("Conversation 9.0K"), "first group missing: {row:?}");
        assert!(!row.contains("Summ"), "partial label: {row:?}");
    }

    #[test]
    fn summary_fits_when_wide() {
        let app = app_with_categories();
        let s = render(&app, 200, 12);
        let row = summary_row(&s);
        assert!(!row.contains('…'), "unexpected ellipsis: {row:?}");
        assert!(row.contains("Rules 3.0K"), "last group missing: {row:?}");
    }

    #[test]
    fn summary_truncates_leading_part() {
        let app = app_with_categories();
        let tl = app.timeline().unwrap();
        for w in [0u16, 1, 5, 10] {
            let line = summary_line(tl, 3, w);
            assert!(line.width() <= w as usize, "width {w}: {line:?}");
            if w > 0 {
                assert!(line.spans.last().unwrap().content.ends_with('…'), "width {w}: {line:?}");
            }
        }
    }

    #[test]
    fn side_by_side_at_100x12() {
        let app = synthetic_app();
        let s = render(&app, 100, 12);
        let all = s.join("\n");
        assert!(
            s.iter().any(|l| l.contains("msgs") && (l.contains('┤') || l[..l.find("msgs").unwrap()].contains('│'))),
            "chart and table do not share rows:\n{all}"
        );
        assert!(s.iter().any(|l| l.contains(SELECTED)), "no selected marker:\n{all}");
        assert!(s.iter().any(|l| l.contains(COMPACTION)), "no compaction marker:\n{all}");
        assert!(!all.contains('●'), "old glyph remains:\n{all}");
    }

    #[test]
    fn compaction_marked_on_x_axis() {
        let app = synthetic_app();
        let s = render(&app, 100, 12);
        // Axis row: the row above the x labels, two above the bottom border.
        let axis = &s[s.len() - 3];
        let left_half: String = axis.chars().take(50).collect();
        assert!(left_half.contains(COMPACTION), "no ▼ on axis row: {axis}");
        assert!(axis_marker_cells(Rect::new(0, 0, 11, 10), 3, 5.0, &[3.0]).is_empty());
        assert_eq!(axis_marker_cells(Rect::new(0, 0, 21, 10), 4, 5.0, &[0.0, 5.0]), vec![(5, 8), (20, 8)]);
    }

    #[test]
    fn stacked_below_60() {
        let app = synthetic_app();
        let s = render(&app, 50, 20);
        let all = s.join("\n");
        let row = s.iter().find(|l| l.contains("msgs")).unwrap_or_else(|| panic!("no header:\n{all}"));
        assert!(!row.contains('┤'), "header shares a row with the chart:\n{all}");
        assert!(all.contains(SELECTED) && all.contains(COMPACTION), "markers missing:\n{all}");
        assert!(!all.contains('●'), "old glyph remains:\n{all}");
    }
}
