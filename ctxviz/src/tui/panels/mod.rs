//! Panels and small drawing helpers shared between them.
pub mod detail;
pub mod header;
pub mod timeline;
pub mod tree;

use crate::theme::{Role, Theme};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

/// The process-wide theme.
pub fn th() -> &'static Theme {
    crate::theme::current()
}

/// Bordered panel; the focused one gets an accent border and bold title.
pub fn panel(title: &str, focused: bool) -> Block<'static> {
    let (border, title_style) = if focused {
        (th().fg(Role::Accent), th().fg(Role::Accent).add_modifier(Modifier::BOLD))
    } else {
        (th().fg(Role::Border), Style::default())
    };
    let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(border);
    if title.is_empty() {
        block
    } else {
        // Titles can carry store-derived names; sanitize as an extra layer.
        let title = crate::text::sanitize_single_line(title);
        block.title(ratatui::text::Span::styled(format!(" {title} "), title_style))
    }
}

/// Status line spans to a ratatui line: each span in `theme.fg(role)` (the
/// default style when it has no role), bold where marked. Raw spans carry
/// escapes, which ratatui would print literally, so they are stripped.
pub fn spans_to_line(spans: &[crate::render::statusline::Span], theme: &Theme) -> ratatui::text::Line<'static> {
    use crate::render::statusline::strip_ansi;
    let spans: Vec<ratatui::text::Span<'static>> = spans
        .iter()
        .map(|s| {
            let mut style = s.color.map_or(Style::default(), |r| theme.fg(r));
            if s.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            let text = if s.raw { strip_ansi(&s.text) } else { s.text.clone() };
            ratatui::text::Span::styled(text, style)
        })
        .collect();
    ratatui::text::Line::from(spans)
}

/// `111600 -> "111,600"`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_panel_uses_accent_border() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 6, 3));
        ratatui::widgets::Widget::render(panel("X", true), buf.area, &mut buf);
        assert_eq!(buf[(0, 0)].fg, th().paint(Role::Accent).color());
    }

    #[test]
    fn selection_differs_by_focus() {
        assert_ne!(th().selection(true), th().selection(false));
    }

    #[test]
    fn spans_to_line_maps_roles_and_bold() {
        use crate::render::statusline::Span;
        let t = Theme::preset(crate::theme::Family::Modus, crate::theme::Mode::Light);
        let spans = vec![
            Span::colored("ab", Role::Accent).bold(),
            Span::new(" ", None),
            Span::colored("\x1b[31mcd\x1b[0m", Role::Text),
        ];
        let mut raw = spans.clone();
        raw[2] = Span { color: Some(Role::Text), ..Span::raw("\x1b[31mcd\x1b[0m".into(), 2) };
        let line = spans_to_line(&raw, &t);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "ab");
        assert_eq!(line.spans[0].style, t.fg(Role::Accent).add_modifier(Modifier::BOLD));
        assert_eq!(line.spans[1].style, Style::default());
        assert_eq!(line.spans[2].content, "cd", "raw escapes stripped");
        assert_eq!(line.spans[2].style, t.fg(Role::Text));
    }

    #[test]
    fn thousands_groups() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(111_600), "111,600");
        assert_eq!(super::thousands(1_234_567), "1,234,567");
    }
}
