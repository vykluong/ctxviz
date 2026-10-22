//! Stacked bar builder shared by the status line and the TUI.
//!
//! `segments` splits the filled part of a bar across categories with
//! largest-remainder rounding, so the segment cells always sum exactly to the
//! filled width. Segments come out in the fixed category order
//! (`categories::order`), never by size.

use crate::model::categories;
use crate::theme::{Paint, Role, Theme};

/// Bars are never narrower than this many cells.
pub const MIN_WIDTH: usize = 10;
pub const FILLED: char = '█';
pub const EMPTY: char = '░';

/// One colored run of the bar: category id and cell count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub category_id: String,
    pub cells: usize,
}

/// Effective bar width: `width`, but at least `MIN_WIDTH`.
pub fn effective_width(width: usize) -> usize {
    width.max(MIN_WIDTH)
}

/// Filled cells for `used` out of `max` on a bar of `width` cells: rounded,
/// at least one cell when anything is used, never more than `width`.
pub fn filled_cells(used: u64, max: u64, width: usize) -> usize {
    if used == 0 || width == 0 {
        return 0;
    }
    if max == 0 || used >= max {
        return width;
    }
    let cells = ((used as u128 * width as u128 * 2 + max as u128) / (max as u128 * 2)) as usize;
    cells.clamp(1, width)
}

/// Split a bar of `width` cells (at least `MIN_WIDTH`) across
/// `(category id, tokens)` scaled to `max`. The filled width is the token sum
/// scaled to `max`; it is distributed by largest remainder so the returned
/// cells sum exactly to it. Zero-cell segments are omitted.
pub fn segments(parts: &[(String, u64)], max: u64, width: usize) -> Vec<Segment> {
    let width = effective_width(width);
    let mut parts: Vec<&(String, u64)> = parts.iter().filter(|(_, t)| *t > 0).collect();
    parts.sort_by_key(|(id, _)| categories::order(id)); // stable: unknown ids keep input order
    let total: u64 = parts.iter().map(|(_, t)| *t).sum();
    let filled = filled_cells(total, max, width);
    if filled == 0 {
        return Vec::new();
    }
    // floor(tokens * filled / total) plus one extra cell for the largest remainders.
    let mut cells: Vec<(usize, u128)> = parts
        .iter()
        .map(|(_, t)| {
            let num = *t as u128 * filled as u128;
            ((num / total as u128) as usize, num % total as u128)
        })
        .collect();
    let assigned: usize = cells.iter().map(|c| c.0).sum();
    let mut by_rem: Vec<usize> = (0..cells.len()).collect();
    by_rem.sort_by(|&a, &b| cells[b].1.cmp(&cells[a].1).then(a.cmp(&b)));
    for &i in by_rem.iter().take(filled - assigned) {
        cells[i].0 += 1;
    }
    parts
        .iter()
        .zip(cells)
        .filter(|(_, (n, _))| *n > 0)
        .map(|((id, _), (n, _))| Segment { category_id: id.clone(), cells: n })
        .collect()
}

pub fn filled(segs: &[Segment]) -> usize {
    segs.iter().map(|s| s.cells).sum()
}

fn push_run(out: &mut String, ch: char, n: usize) {
    out.extend(std::iter::repeat_n(ch, n));
}

/// ANSI bar: each segment in `color(category_id)`, the remainder up to
/// `effective_width(width)` as `░` in the theme's `BarEmpty` paint. Ends with
/// a reset. `Paint::Default` writes no color escape.
pub fn ansi_with(segs: &[Segment], width: usize, theme: &Theme, color: impl Fn(&str) -> Paint) -> String {
    let width = effective_width(width);
    let mut out = String::with_capacity(width * 4 + segs.len() * 20 + 24);
    let mut used = 0;
    for s in segs {
        let n = s.cells.min(width - used);
        if n == 0 {
            continue;
        }
        color(&s.category_id).sgr_fg(&mut out);
        push_run(&mut out, FILLED, n);
        used += n;
    }
    if used < width {
        theme.paint(Role::BarEmpty).sgr_fg(&mut out);
        push_run(&mut out, EMPTY, width - used);
    }
    out.push_str("\x1b[0m");
    out
}

/// ANSI bar with the theme's category paints.
pub fn ansi(segs: &[Segment], width: usize, theme: &Theme) -> String {
    ansi_with(segs, width, theme, |id| theme.category(id))
}

/// Self-labeling ANSI bar (`--bar labeled`), same geometry and display width
/// as `ansi`. Each segment is a background run in its category fill; when
/// the run is at least `label + 2` cells wide, the category's short label is
/// centered in it in black or white ink (by the fill's luminance). Every
/// background run ends with a reset. The remainder is drawn exactly like
/// `ansi`. Segments whose paint is `Default` (the `none` theme) fall back to
/// the plain `█` run, so an uncolored theme renders byte-identical to `ansi`.
pub fn ansi_labeled(segs: &[Segment], width: usize, theme: &Theme) -> String {
    let width = effective_width(width);
    let mut out = String::with_capacity(width * 4 + segs.len() * 48 + 24);
    let mut used = 0;
    for s in segs {
        let n = s.cells.min(width - used);
        if n == 0 {
            continue;
        }
        used += n;
        let fill = theme.category(&s.category_id);
        if fill == Paint::Default {
            push_run(&mut out, FILLED, n);
            continue;
        }
        fill.sgr_bg(&mut out);
        // Unknown ids come from the store; sanitize on top of `short_label`'s check.
        let label = crate::text::sanitize_single_line(categories::short_label(&s.category_id));
        let len = label.chars().count();
        if n >= len + 2 {
            let left = (n - len) / 2;
            fill.ink_on().sgr_fg(&mut out);
            push_run(&mut out, ' ', left);
            out.push_str(&label);
            push_run(&mut out, ' ', n - len - left);
        } else {
            push_run(&mut out, ' ', n);
        }
        out.push_str("\x1b[0m");
    }
    if used < width {
        theme.paint(Role::BarEmpty).sgr_fg(&mut out);
        push_run(&mut out, EMPTY, width - used);
    }
    out.push_str("\x1b[0m");
    out
}

/// Uncolored bar (`█` filled, `░` empty), same geometry as `ansi`.
pub fn plain(segs: &[Segment], width: usize) -> String {
    let width = effective_width(width);
    let f = filled(segs).min(width);
    let mut out = String::with_capacity(width * 3);
    push_run(&mut out, FILLED, f);
    push_run(&mut out, EMPTY, width - f);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Family, Mode};

    fn p(v: &[(&str, u64)]) -> Vec<(String, u64)> {
        v.iter().map(|(id, t)| (id.to_string(), *t)).collect()
    }

    #[test]
    fn sums_exactly_to_filled_width() {
        let parts = p(&[("conversation", 333), ("tools", 333), ("system_prompt", 334)]);
        for width in 10..200 {
            let segs = segments(&parts, 1000, width);
            assert_eq!(filled(&segs), width, "full bar, width {width}");
            let segs = segments(&parts, 3000, width);
            assert_eq!(filled(&segs), filled_cells(1000, 3000, width), "third, width {width}");
        }
        // Real fixture numbers: 15624 / 300000 on 100 cells -> 5 cells.
        let parts = p(&[
            ("system_prompt", 3346),
            ("tools", 6354),
            ("rules", 630),
            ("skills", 1884),
            ("mcp", 456),
            ("subagents", 1513),
            ("summarized_conversation", 0),
            ("conversation", 1441),
        ]);
        let segs = segments(&parts, 300_000, 100);
        assert_eq!(filled(&segs), 5);
        let segs = segments(&parts, 15624, 57);
        assert_eq!(filled(&segs), 57);
    }

    #[test]
    fn order_is_fixed_category_order() {
        let parts = p(&[("conversation", 500), ("tools", 100), ("rules", 300), ("system_prompt", 50)]);
        let segs = segments(&parts, 1000, 40);
        let ids: Vec<&str> = segs.iter().map(|s| s.category_id.as_str()).collect();
        assert_eq!(ids, ["system_prompt", "tools", "rules", "conversation"]);
        // Unknown ids go last.
        let parts = p(&[("zzz", 100), ("tools", 100)]);
        let ids: Vec<String> = segments(&parts, 200, 20).into_iter().map(|s| s.category_id).collect();
        assert_eq!(ids, ["tools", "zzz"]);
    }

    #[test]
    fn width_has_minimum_of_ten() {
        let parts = p(&[("tools", 50)]);
        let segs = segments(&parts, 100, 3);
        assert_eq!(filled(&segs), 5);
        assert_eq!(plain(&segs, 3).chars().count(), MIN_WIDTH);
        assert_eq!(plain(&segs, 3), "█████░░░░░");
        let a = ansi(&segs, 0, &Theme::preset(Family::Modus, Mode::Light));
        assert_eq!(a.matches(FILLED).count() + a.matches(EMPTY).count(), MIN_WIDTH);
    }

    #[test]
    fn ansi_follows_theme_mode() {
        let parts = p(&[("tools", 30), ("conversation", 20)]);
        let segs = segments(&parts, 100, 20);
        let plain = ansi(&segs, 20, &Theme::plain());
        assert!(!plain.contains("\x1b[38"), "{plain:?}");
        assert_eq!(plain.trim_end_matches("\x1b[0m"), "██████████░░░░░░░░░░");
        let a = ansi(&segs, 20, &Theme::ansi());
        assert!(a.contains("\x1b[3") || a.contains("\x1b[9"), "{a:?}");
        assert!(a.contains("\x1b[90m"), "BarEmpty is bright black: {a:?}");
        assert!(!a.contains("38;2"), "{a:?}");
    }

    fn strip(s: &str) -> String {
        crate::render::statusline::strip_ansi(s)
    }

    /// SGR parameter lists (`38;2;1;2;3`, `0`, ...) in `s`.
    fn sgr_codes(s: &str) -> Vec<String> {
        s.split("\x1b[").skip(1).map(|t| t.split('m').next().unwrap().to_string()).collect()
    }

    fn fixture_parts() -> Vec<(String, u64)> {
        p(&[
            ("system_prompt", 3346),
            ("tools", 6354),
            ("rules", 630),
            ("skills", 1884),
            ("mcp", 456),
            ("subagents", 1513),
            ("summarized_conversation", 0),
            ("conversation", 1441),
        ])
    }

    #[test]
    fn labeled_has_plain_width_and_remainder() {
        let themes = [
            Theme::preset(Family::Modus, Mode::Light),
            Theme::preset(Family::Modus, Mode::Dark),
            Theme::ansi(),
            Theme::plain(),
        ];
        for t in &themes {
            for (max, width) in [(15624, 60), (40_000, 80), (300_000, 100), (1000, 3)] {
                let segs = segments(&fixture_parts(), max, width);
                let a = ansi(&segs, width, t);
                let l = ansi_labeled(&segs, width, t);
                assert_eq!(strip(&l).chars().count(), strip(&a).chars().count(), "{width}: {l:?}");
                assert_eq!(strip(&l).chars().count(), effective_width(width));
                // The remainder is byte-identical to the plain bar's.
                let rest = |s: &str| s.find(EMPTY).map(|i| s[i..].to_string());
                assert_eq!(rest(&l), rest(&a));
                assert!(l.ends_with("\x1b[0m"));
            }
        }
    }

    #[test]
    fn labeled_centers_label_in_wide_segment_only() {
        let t = Theme::preset(Family::Modus, Mode::Dark);
        // tools: 12 cells (wide), rules: 3 cells (narrow), on a 20-cell bar.
        let segs =
            vec![Segment { category_id: "tools".into(), cells: 12 }, Segment { category_id: "rules".into(), cells: 3 }];
        let l = ansi_labeled(&segs, 20, &t);
        assert_eq!(strip(&l), format!("   tools    {}{}", " ".repeat(3), EMPTY.to_string().repeat(5)));
        assert!(!strip(&l).contains("rules"));
        // Exactly label + 2 is wide enough; label + 1 is not.
        let fits = |cells| {
            let segs = vec![Segment { category_id: "rules".into(), cells }];
            strip(&ansi_labeled(&segs, 20, &t)).contains("rules")
        };
        assert!(fits(7));
        assert!(!fits(6));
        // Background in the category fill, black/white ink, reset after each run.
        let mut bg = String::new();
        t.category("tools").sgr_bg(&mut bg);
        assert!(bg.contains("48;2;"), "{bg:?}");
        assert!(l.starts_with(&bg), "{l:?}");
        let mut ink = String::new();
        t.category("tools").ink_on().sgr_fg(&mut ink);
        assert!(ink == "\x1b[38;2;0;0;0m" || ink == "\x1b[38;2;255;255;255m", "{ink:?}");
        assert!(l.contains(&format!("{bg}{ink}   tools    \x1b[0m")), "{l:?}");
        assert_eq!(l.matches("\x1b[0m").count(), 3, "{l:?}");
        assert!(!l.contains(FILLED), "{l:?}");
    }

    #[test]
    fn labeled_with_fixture_categories() {
        let t = Theme::preset(Family::Modus, Mode::Light);
        let segs = segments(&fixture_parts(), 15624, 100);
        let l = strip(&ansi_labeled(&segs, 100, &t));
        assert_eq!(l.chars().count(), 100);
        for s in &segs {
            let label = categories::short_label(&s.category_id);
            assert_eq!(l.contains(label), s.cells >= label.len() + 2, "{} {} in {l:?}", s.category_id, s.cells);
        }
        assert!(l.contains("tools") && l.contains("system"), "{l:?}");
    }

    #[test]
    fn labeled_none_theme_equals_plain() {
        let t = Theme::plain();
        for width in [10, 37, 100] {
            let segs = segments(&fixture_parts(), 30_000, width);
            assert_eq!(ansi_labeled(&segs, width, &t), ansi(&segs, width, &t));
        }
    }

    #[test]
    fn labeled_ansi_theme_uses_16_color_codes_only() {
        let t = Theme::ansi();
        let segs = segments(&fixture_parts(), 20_000, 80);
        let l = ansi_labeled(&segs, 80, &t);
        assert!(strip(&l).contains("tools"), "{l:?}");
        for code in sgr_codes(&l) {
            let ok = code == "0"
                || (code.len() == 2 && ["3", "9", "4"].iter().any(|p| code.starts_with(p)))
                || (code.len() == 3 && code.starts_with("10"));
            assert!(ok, "unexpected SGR {code:?} in {l:?}");
        }
        assert!(sgr_codes(&l).iter().any(|c| c.starts_with('4') || c.starts_with("10")), "{l:?}");
        assert!(sgr_codes(&l).iter().any(|c| c == "30" || c == "97"), "{l:?}");
    }

    #[test]
    fn tiny_usage_gets_one_cell_and_zero_gets_none() {
        assert_eq!(filled_cells(1, 1_000_000, 20), 1);
        assert_eq!(filled_cells(0, 1000, 20), 0);
        assert!(segments(&p(&[("tools", 0)]), 1000, 20).is_empty());
        assert_eq!(filled_cells(2000, 1000, 20), 20);
    }
}
