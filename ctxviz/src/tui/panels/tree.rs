//! Explorer tree: markers, indentation, label, right-aligned ~tokens.

use crate::theme::Role;
use crate::tui::app::{App, Focus};
use crate::tui::panels::{panel, th, thousands};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let title = if app.filter_input || !app.filter.is_empty() {
        let filter = crate::text::sanitize_single_line(&app.filter);
        format!("Explorer  /{filter}{}", if app.filter_input { "_" } else { "" })
    } else {
        "Explorer (j/k, enter, h/l, / filter)".into()
    };
    let block = panel(&title, app.focus == Focus::Tree);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.tree().is_none() {
        f.render_widget(Paragraph::new("no snapshot yet").style(th().fg(Role::Ink2)), inner);
        return;
    }
    if app.rows().is_empty() {
        let msg = if app.filter.is_empty() { "empty tree" } else { "no match" };
        f.render_widget(Paragraph::new(msg).style(th().fg(Role::Ink2)), inner);
        return;
    }
    f.render_widget(Paragraph::new(lines(app, inner.width as usize, inner.height as usize)), inner);
}

/// Visible explorer lines for a `width` x `height` viewport; scrolls
/// `app.scroll` so the selection stays in view.
pub fn lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    let Some(tree) = app.tree() else { return Vec::new() };
    let rows = app.rows();
    let sel = app.selected_row();
    let mut top = app.scroll.get().min(rows.len().saturating_sub(1));
    if let Some(s) = sel {
        if s < top {
            top = s;
        } else if height > 0 && s >= top + height {
            top = s + 1 - height;
        }
    }
    app.scroll.set(top);
    rows.iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(i, r)| {
            let n = &tree.nodes[r.idx];
            let marker = match (r.has_children, r.expanded) {
                (true, true) => "▾ ",
                (true, false) => "▸ ",
                _ => "  ",
            };
            let indent = "  ".repeat(r.depth);
            let tokens = format!(" ~{}", thousands(n.estimated_tokens));
            // Top-level rows carry a category swatch after the marker.
            let swatch = if r.depth == 0 { "■ " } else { "" };
            let used = indent.chars().count() + 2 + swatch.chars().count() + tokens.chars().count();
            let room = width.saturating_sub(used);
            let label = truncate(&n.label, room);
            let pad = room.saturating_sub(label.chars().count());
            let cat = Style::default().fg(th().category(&n.category_id).color());
            let line = Line::from(vec![
                Span::raw(indent),
                Span::styled(marker, cat),
                Span::styled(swatch, cat),
                Span::raw(label),
                Span::raw(" ".repeat(pad)),
                Span::styled(tokens, th().fg(Role::Ink2)),
            ]);
            if Some(i) == sel {
                line.style(th().selection(app.focus == Focus::Tree))
            } else {
                line
            }
        })
        .collect()
}

/// First line of `s`, cut to `room` cells with an ellipsis. Control
/// characters become U+FFFD and tabs spaces (labels are sanitized in the
/// model too; this keeps the sink safe on its own).
pub(crate) fn truncate(s: &str, room: usize) -> String {
    let s = crate::text::sanitize_single_line(s.lines().next().unwrap_or(""));
    if s.chars().count() <= room {
        return s.to_string();
    }
    let mut out: String = s.chars().take(room.saturating_sub(1)).collect();
    if room > 0 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::{sample_tree, snapshot};

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn top_level_rows_have_swatch() {
        let mut app = App::new();
        app.set_snapshot(snapshot(sample_tree()));
        let ls: Vec<String> = lines(&app, 40, 10).iter().map(text).collect();
        assert!(ls[0].starts_with("▾ ■ Conversation"), "{ls:?}");
        assert!(ls[0].ends_with(" ~100"), "{ls:?}");
        assert_eq!(ls[0].chars().count(), 40);
        // Deeper rows keep the plain layout.
        assert!(ls[1].starts_with("  ▸ Turn 1"), "{ls:?}");
        assert!(ls.iter().all(|l| l.chars().count() == 40), "{ls:?}");
    }
}
