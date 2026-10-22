//! `ctxviz snapshot`: one-shot report, mirrors `context_usage.py` static mode.

use super::{StoreArgs, ThemeArgs};
use crate::model::snapshot::{Snapshot, SnapshotOptions};
use crate::render;
use crate::render::statusline::{to_ansi, Span};
use crate::store::Store;
use crate::text::sanitize_single_line;
use crate::theme::{self, Mode, Role, Theme};
use std::io::IsTerminal;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    #[command(flatten)]
    pub store: StoreArgs,
    #[command(flatten)]
    pub theme: ThemeArgs,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
    /// Explorer tree depth (0 hides it).
    #[arg(long, default_value_t = 2)]
    pub depth: usize,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    args.theme.init();
    let store = Store::open(args.store.resolve()?)?;
    let opts = SnapshotOptions { tree: args.json || args.depth > 0, timeline: args.json };
    let Some(snap) = Snapshot::load(&store, opts)? else {
        eprintln!("chat has no checkpoint yet");
        return Ok(1);
    };
    if args.json {
        println!("{}", render::json::to_string(&snap)?);
        return Ok(0);
    }
    print!("{}{}", render_text(&snap, &text_theme(&args.theme)), render_tree(&snap, args.depth));
    Ok(0)
}

/// Theme for text output (snapshot, watch); call after `ThemeArgs::init`.
/// The resolved theme on a terminal or when `--theme`/`--theme-file` was
/// passed, otherwise plain. `NO_COLOR` already resolves to plain.
pub fn text_theme(args: &ThemeArgs) -> Theme {
    let explicit = args.theme.is_some() || args.theme_file.is_some();
    if explicit || std::io::stdout().is_terminal() {
        theme::current().clone()
    } else {
        Theme::plain()
    }
}

/// Whether `theme` writes any escapes.
pub fn is_colored(theme: &Theme) -> bool {
    theme.mode != Mode::Plain
}

pub fn human(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Totals and breakdown table (Python `render` header). Bar runs and row
/// markers use the category roles, the empty remainder `BarEmpty`; the plain
/// theme writes no escapes. Store-derived strings (title, labels) are
/// sanitized here even though the model already did it: this text goes
/// straight to a terminal.
pub fn render_text(snap: &Snapshot, theme: &Theme) -> String {
    let mut out = String::new();
    let title = sanitize_single_line(snap.name.as_deref().or(snap.agent_id.as_deref()).unwrap_or("?"));
    out += &format!("Context Usage · {title}\n");
    match snap.percent {
        Some(p) => out += &format!("{p}% Full{:<10}~{} / {} Tokens\n", "", human(snap.used), human(snap.max)),
        None => out += "No token data yet.\n",
    }
    let width = 60u64;
    let mut bar = Vec::new();
    let mut filled = 0;
    for cat in &snap.breakdown {
        let w = if snap.max > 0 {
            (cat.estimated_tokens as f64 / snap.max as f64 * width as f64).round_ties_even() as usize
        } else {
            0
        };
        filled += w;
        if w > 0 {
            bar.push(Span::colored("█".repeat(w), Role::category(&cat.id)));
        }
    }
    let rest = (width as usize).saturating_sub(filled);
    if rest > 0 {
        bar.push(Span::colored("░".repeat(rest), Role::BarEmpty));
    }
    out += &to_ansi(&bar, theme);
    out.push('\n');
    for cat in snap.breakdown.iter().filter(|c| c.estimated_tokens > 0) {
        let marker = to_ansi(&[Span::colored("■", Role::category(&cat.id))], theme);
        out += &format!("  {marker} {:<26}{:>8}\n", sanitize_single_line(&cat.label), human(cat.estimated_tokens));
    }
    out
}

/// Python `f"{n:,}"`.
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Context Explorer section of the Python `render` (empty when depth is 0).
pub fn render_tree(snap: &Snapshot, depth: usize) -> String {
    if depth == 0 {
        return String::new();
    }
    let tree = &snap.tree;
    let kids = tree.children();
    let nonzero = |id: &str| {
        kids.get(&Some(id)).map_or(0, |ch| ch.iter().filter(|&&i| tree.nodes[i].estimated_tokens > 0).count())
    };
    let mut out = String::from("\nContext Explorer  (children are local ~chars/4 estimates)\n");
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let push = |stack: &mut Vec<(usize, usize)>, parent: Option<&str>, level: usize| {
        let ch = kids.get(&parent).map_or(Vec::new(), |ch| tree.ordered(parent, ch));
        stack.extend(ch.into_iter().rev().map(|i| (i, level)));
    };
    push(&mut stack, None, 0);
    while let Some((i, level)) = stack.pop() {
        let n = &tree.nodes[i];
        if n.estimated_tokens == 0 {
            continue;
        }
        let count = nonzero(&n.id);
        let label = sanitize_single_line(&n.label);
        let label = if count > 0 { format!("{label} {count}") } else { label.into_owned() };
        let marker = if count > 0 && level + 1 < depth {
            '▾'
        } else if count > 0 {
            '▸'
        } else {
            ' '
        };
        let line = format!("{}{marker} {label}", "  ".repeat(level));
        out += &format!("{line:<64}~{}\n", thousands(n.estimated_tokens));
        if level + 1 < depth {
            push(&mut stack, Some(&n.id), level + 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::breakdown::BreakdownEntry;
    use crate::model::tree::Tree;
    use crate::theme::Family;

    fn snap() -> Snapshot {
        let c = |id: &str, t| BreakdownEntry { id: id.into(), label: id.into(), estimated_tokens: t };
        Snapshot {
            store: "store.db".into(),
            agent_id: Some("a".into()),
            name: Some("Chat".into()),
            root_blob_id: None,
            used: 33_000,
            max: 300_000,
            percent: Some(11.0),
            breakdown: vec![c("system_prompt", 3_300), c("tools", 7_000), c("rules", 616), c("conversation", 18_900)],
            message_ids: Vec::new(),
            created_at: None,
            tree: Tree::default(),
            timeline: None,
            meta: Default::default(),
        }
    }

    /// Every SGR escape in `s` (without the leading ESC).
    fn escapes(s: &str) -> Vec<&str> {
        s.split('\x1b').skip(1).map(|e| &e[..=e.find('m').unwrap()]).collect()
    }

    #[test]
    fn ansi_theme_uses_only_16_color_codes() {
        let out = render_text(&snap(), &Theme::ansi());
        let esc = escapes(&out);
        assert!(!esc.is_empty());
        for e in esc {
            let ok = e == "[0m" || (e.len() == 4 && (e.starts_with("[3") || e.starts_with("[9")));
            assert!(ok, "unexpected escape {e:?}");
        }
    }

    #[test]
    fn none_theme_has_no_escapes() {
        let out = render_text(&snap(), &Theme::plain());
        assert!(!out.contains('\x1b'));
        assert!(out.contains("██") && out.contains("░░") && out.contains("  ■ tools"));
    }

    #[test]
    fn dark_and_light_differ_in_truecolor() {
        let light = render_text(&snap(), &Theme::preset(Family::Modus, Mode::Light));
        let dark = render_text(&snap(), &Theme::preset(Family::Modus, Mode::Dark));
        assert!(light.contains("\x1b[38;2;") && dark.contains("\x1b[38;2;"));
        assert_ne!(light, dark);
        let strip = crate::render::statusline::strip_ansi;
        assert_eq!(strip(&light), strip(&dark));
        assert_eq!(strip(&light), render_text(&snap(), &Theme::plain()));
    }
}
