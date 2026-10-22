//! Detail: selected node metadata in the title, scrollable text below.

use crate::model::categories;
use crate::model::tree::Node;
use crate::text::{sanitize_single_line, sanitize_terminal_text};
use crate::theme::Role;
use crate::tui::app::{App, Focus};
use crate::tui::panels::tree::truncate;
use crate::tui::panels::{panel, th, thousands};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
    Frame,
};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(n) = app.selected_node() else {
        let block = panel("Detail", app.focus == Focus::Detail);
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(Paragraph::new("nothing selected").style(th().fg(Role::Ink2)), inner);
        return;
    };
    let block = panel(&title(n, area.width), app.focus == Focus::Detail);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    if let Some(local) = n.local_estimate {
        lines.push(Line::styled(format!("(server total; local estimate ~{})", thousands(local)), th().fg(Role::Ink2)));
    }
    match &n.text {
        // Defense in depth: the model sanitizes node text, but the buffer
        // must never hold a control character whatever built the node.
        Some(t) => lines.extend(t.lines().map(|l| Line::raw(sanitize_terminal_text(l).into_owned()))),
        None => lines.push(Line::styled("(no text: group node)", th().fg(Role::Ink2))),
    }
    // Clamp (on unwrapped lines) so G lands near the end rather than past it.
    let max = (lines.len() as u16).saturating_sub(inner.height);
    let scroll = app.detail_scroll.min(max);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), inner);
}

/// `Detail · <label> · <kind> · <category> · N chars · ~N tokens`, with the
/// label truncated so the title fits a panel `width` cells wide.
fn title(n: &Node, width: u16) -> String {
    let cat_label = categories::label(&n.category_id).map_or_else(|| sanitize_single_line(&n.category_id), Into::into);
    let rest = format!(
        " · {} · {} · {} chars · ~{} tokens",
        sanitize_single_line(&n.kind),
        cat_label,
        thousands(n.character_count),
        thousands(n.estimated_tokens)
    );
    let head = "Detail · ";
    let room = (width as usize).saturating_sub(4 + head.chars().count() + rest.chars().count());
    format!("{head}{}{rest}", truncate(&n.label, room))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::{sample_tree, snapshot};

    #[test]
    fn title_carries_meta_and_fits() {
        let t = sample_tree();
        let n = &t.nodes[3];
        let full = title(n, 200);
        assert!(full.starts_with("Detail · Conversation · category · "), "{full}");
        assert!(full.ends_with(" · 400 chars · ~100 tokens"), "{full}");
        let cut = title(n, 70);
        assert!(cut.chars().count() <= 66, "{cut}");
        assert!(cut.contains('…') && cut.ends_with("~100 tokens"), "{cut}");
    }

    #[test]
    fn body_starts_with_text() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new();
        app.set_snapshot(snapshot(sample_tree()));
        let mut term = Terminal::new(TestBackend::new(80, 6)).unwrap();
        term.draw(|f| draw(f, f.area(), &app)).unwrap();
        let buf = term.backend().buffer();
        let row = |y: u16| (0..80).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        assert!(row(0).contains("Detail · Conversation · "), "{}", row(0));
        assert!(row(1).contains("text of Conversation"), "{}", row(1));
    }
}
