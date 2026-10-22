//! Header: a borderless strip. Row 0 is the status line's row 1 (stacked
//! bar, `pct glyph ~used/max`, legend); row 1 is the chat title info.

use crate::render::statusline::{self, Usage};
use crate::text::sanitize_single_line;
use crate::theme::Role;
use crate::tui::app::App;
use crate::tui::panels::{spans_to_line, th};
use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

const SEP: &str = " · ";

/// Row 1 as styled spans: `<chat name> · <model> · <n> ckpts · <k> compactions`.
/// The model and counts are omitted when the snapshot lacks them. Without a
/// snapshot it is the loading/waiting text.
fn title_spans(app: &App) -> Vec<Span<'static>> {
    let text = th().fg(Role::Text);
    let Some(s) = &app.snapshot else {
        let msg = match &app.store {
            Some(p) => format!("ctxviz · loading {}", sanitize_single_line(&p.display().to_string())),
            None => "ctxviz · waiting for a chat".into(),
        };
        return vec![Span::styled(msg, text)];
    };
    // Name and model are sanitized in the snapshot already; sanitize again at
    // the sink so a hand-built snapshot cannot smuggle escapes either.
    let name = s.name.as_deref().map_or_else(|| "(unnamed)".into(), |n| sanitize_single_line(n).into_owned());
    let mut parts = vec![Span::styled(name, text.add_modifier(Modifier::BOLD))];
    let mut push = |t: String| {
        parts.push(Span::styled(SEP, text));
        parts.push(Span::styled(t, text));
    };
    if let Some(m) = s.meta.last_used_model.as_deref().filter(|m| !m.trim().is_empty()) {
        push(sanitize_single_line(m).into_owned());
    }
    if let Some(tl) = &s.timeline {
        push(format!("{} ckpts", tl.checkpoints.len()));
        push(format!("{} compactions", tl.compactions().len()));
    }
    parts
}

/// Row 1 text, e.g. `Test chat · claude-opus-5-5 · 12 ckpts · 1 compactions`.
pub fn title(app: &App) -> String {
    title_spans(app).iter().map(|s| s.content.as_ref()).collect()
}

/// The context usage of the snapshot, as the status line sees it.
pub fn usage(s: &crate::model::snapshot::Snapshot) -> Usage {
    Usage {
        used: Some(s.used),
        max: Some(s.max),
        percent: s.percent,
        categories: s.breakdown.iter().map(|b| (b.id.clone(), b.estimated_tokens)).collect(),
    }
}

/// Row 0: the status line's context row for `app`'s snapshot at `width`.
pub fn context_line(app: &App, width: usize) -> Line<'static> {
    match &app.snapshot {
        Some(s) => spans_to_line(&statusline::context_row(&usage(s), width), th()),
        None => Line::default(),
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let lines = vec![context_line(app, area.width as usize), Line::from(title_spans(app))];
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::statusline::{context_row, strip_ansi, to_ansi};
    use crate::theme::{Family, Mode, Theme};

    fn sample_usage() -> Usage {
        Usage {
            used: Some(121_800),
            max: Some(200_000),
            percent: Some(60.9),
            categories: vec![
                ("system_prompt".into(), 3_346),
                ("tools".into(), 6_354),
                ("rules".into(), 630),
                ("conversation".into(), 111_470),
            ],
        }
    }

    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The header's row 0 shows exactly the status line's row 1 text.
    #[test]
    fn row0_matches_statusline_text() {
        let t = Theme::preset(Family::Modus, Mode::Light);
        for u in [sample_usage(), Usage { categories: Vec::new(), ..sample_usage() }] {
            for width in [8, 30, 49, 60, 80, 100, 120, 200] {
                let spans = context_row(&u, width);
                let status = strip_ansi(&to_ansi(&spans, &t));
                let line = spans_to_line(&spans, &t);
                assert_eq!(line_text(&line), status, "width {width}");
                assert!(spans.iter().all(|s| !s.raw), "no raw spans at width {width}");
            }
        }
    }

    #[test]
    fn bar_cells_use_category_roles() {
        let t = Theme::preset(Family::Modus, Mode::Light);
        let line = spans_to_line(&context_row(&sample_usage(), 120), &t);
        let conv =
            line.spans.iter().find(|s| s.content.contains('█') && s.style == t.fg(Role::category("conversation")));
        assert!(conv.is_some(), "{line:?}");
        assert!(line.spans.iter().any(|s| s.content.contains('░') && s.style == t.fg(Role::BarEmpty)));
    }
}
