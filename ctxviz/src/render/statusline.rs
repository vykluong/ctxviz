//! Cursor CLI status line renderer; colors come from the current theme's roles.
//!
//! The default two-row layout is the wide one in [`crate::render::wide`]. This
//! module keeps the payload, spans, shared helpers, the classic S2 rows
//! (`--layout classic`) and the one-row mode (`--rows 1`).
//!
//! Classic ("S2"): usage on row 1, session and trend on row 2.
//!
//! ```text
//! ████████░░░░░░░░░░░░░░░░░░░ 11% ● ~33K/300K  ■ system 3.3K ■ tools 7K ■ chat 16.5K ■ +3 more 3.9K
//! Claude Opus 5.5  ‹300K Medium›  ~/code/app  git:main               trend ▁▂▃▅▆▇▇▇▼██ Δ +198
//! ```

use crate::model::breakdown::TokenDetails;
use crate::model::categories;
use crate::render::bar;
use crate::text::sanitize_single_line;
use crate::theme::{self, Paint, Role, Theme};
use serde::{Deserialize, Deserializer};
use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};

// Theme roles for each part of the status line.
pub const TXT_DIM: Role = Role::Ink2;
pub const MODEL_CLR: Role = Role::Text;
pub const PATH_BASE_CLR: Role = Role::Ink2;
pub const PATH_LEAF_CLR: Role = Role::Text;
pub const BRANCH_CLR: Role = Role::Accent;
pub const GOOD_CLR: Role = Role::Good;
pub const WARN_CLR: Role = Role::Warn;
pub const LOW_CLR: Role = Role::Low;
pub const CRIT_CLR: Role = Role::Crit;
pub const TOKEN_CLR: Role = Role::Ink2;
/// Compaction checkpoints in the sparkline.
pub const COMPACTION_CLR: Role = Role::Compaction;

/// Width assumed when the payload has no `render_width_chars`.
pub const DEFAULT_WIDTH: usize = 80;
/// Widest status line rendered. `render_width_chars` comes from the payload,
/// and every row allocates in proportion to the width, so it is clamped here.
pub const MAX_WIDTH: usize = 1024;
const SEP: &str = "  ";

// ── Render options ─────────────────────────────────────────────────────────

/// Number of status line rows (`--rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Rows {
    /// Usage on row 1, session and trend on row 2.
    #[default]
    #[value(name = "2")]
    Two,
    /// Everything on one row.
    #[value(name = "1")]
    One,
}

/// How the context bar is drawn (`--bar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum BarStyle {
    /// Colored blocks only.
    #[default]
    Plain,
    /// Category names inside wide segments.
    Labeled,
}

/// Which two-row layout (`--layout`); `--rows 1` ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Layout {
    /// Model, badge, full-width bar, tokens and duration on row 1; path, git
    /// and the top-3 legend right-aligned on row 2
    #[default]
    Wide,
    /// Bar and legend on row 1, session and trend on row 2
    Classic,
}

/// Layout options for [`render_themed`]; the default is the wide two-row line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOpts {
    pub rows: Rows,
    pub bar: BarStyle,
    pub layout: Layout,
    /// Skip everything git (`--no-git`, `CTXVIZ_NO_GIT=1`): no `.git/HEAD`
    /// read, no `git diff` subprocess, no branch or diff segment.
    pub no_git: bool,
}

// ── Payload ────────────────────────────────────────────────────────────────

/// Deserialize `Option<T>`, mapping a missing, null or mistyped value to None
/// so one odd field never discards the whole payload.
fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

/// Status line stdin payload (Cursor CLI 2026.10: see `544.index.js`). Only the
/// fields ctxviz reads; everything else is ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusPayload {
    #[serde(default, deserialize_with = "lenient")]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub workspace: Option<Workspace>,
    #[serde(default, deserialize_with = "lenient")]
    pub model: Option<ModelInfo>,
    #[serde(default, deserialize_with = "lenient")]
    pub context_window: Option<ContextWindow>,
    /// Terminal columns minus 4.
    #[serde(default, deserialize_with = "lenient")]
    pub render_width_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Workspace {
    #[serde(default, deserialize_with = "lenient")]
    pub current_dir: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub project_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelInfo {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub display_name: Option<String>,
    /// e.g. "300K Medium"; present for parameterized models.
    #[serde(default, deserialize_with = "lenient")]
    pub param_summary: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub max_mode: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextWindow {
    #[serde(default, deserialize_with = "lenient")]
    pub used_percentage: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub remaining_percentage: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub context_window_size: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub total_input_tokens: Option<u64>,
}

impl StatusPayload {
    pub fn parse(s: &str) -> serde_json::Result<StatusPayload> {
        serde_json::from_str(s)
    }

    /// Workspace dir: `cwd`, else `workspace.current_dir`.
    pub fn work_dir(&self) -> Option<&str> {
        self.cwd.as_deref().or_else(|| self.workspace.as_ref()?.current_dir.as_deref()).filter(|s| !s.is_empty())
    }

    pub fn width(&self) -> usize {
        self.render_width_chars.filter(|w| *w > 0).unwrap_or(DEFAULT_WIDTH).min(MAX_WIDTH)
    }

    pub fn model_name(&self) -> Option<&str> {
        let m = self.model.as_ref()?;
        m.display_name.as_deref().or(m.id.as_deref()).filter(|s| !s.is_empty())
    }

    fn cw(&self) -> ContextWindow {
        self.context_window.clone().unwrap_or_default()
    }
}

// ── Styled spans ───────────────────────────────────────────────────────────

/// A run of text in one style. Rows are built from spans so visible width is
/// measured without parsing escapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub color: Option<Role>,
    pub bold: bool,
    /// Pre-rendered ANSI (the bar); `width` holds its visible width.
    pub raw: bool,
    pub width: usize,
}

impl Span {
    /// The text goes through [`sanitize_single_line`]: payload and store
    /// strings (model, path, branch, category ids) can carry escape sequences.
    pub fn new(text: impl Into<String>, color: Option<Role>) -> Span {
        let text = text.into();
        let text = match sanitize_single_line(&text) {
            Cow::Borrowed(_) => text,
            Cow::Owned(clean) => clean,
        };
        let width = text.chars().count();
        Span { text, color, bold: false, raw: false, width }
    }
    pub fn colored(text: impl Into<String>, role: Role) -> Span {
        Span::new(text, Some(role))
    }
    pub fn bold(mut self) -> Span {
        self.bold = true;
        self
    }
    pub(crate) fn raw(text: String, width: usize) -> Span {
        Span { text, color: None, bold: false, raw: true, width }
    }
}

pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.width).sum()
}

/// Spans to SGR-escaped text. Spans whose role paints as `Default` and that
/// are not bold are written bare, so `Theme::plain()` emits no color codes.
///
/// Every non-raw span's text is sanitized again here (`text` is a public
/// field, so [`Span::new`] alone is not a guarantee): the only escape
/// sequences in the output are the SGR codes written below and the bar's
/// pre-rendered ANSI.
pub fn to_ansi(spans: &[Span], theme: &Theme) -> String {
    let mut out = String::new();
    for s in spans {
        if s.raw {
            out.push_str(&s.text);
            continue;
        }
        let text = sanitize_single_line(&s.text);
        let paint = s.color.map_or(Paint::Default, |r| theme.paint(r));
        if paint == Paint::Default && !s.bold {
            out.push_str(&text);
            continue;
        }
        if s.bold {
            out.push_str("\x1b[1m");
        }
        paint.sgr_fg(&mut out);
        out.push_str(&text);
        out.push_str("\x1b[0m");
    }
    out
}

/// Drop trailing text so the spans fit in `width` visible chars, ending with
/// `…` when something was cut. Raw spans are dropped whole.
pub(crate) fn truncate(spans: &mut Vec<Span>, width: usize) {
    if spans_width(spans) <= width {
        return;
    }
    let mut left = width.saturating_sub(1);
    let mut out = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        if left == 0 {
            break;
        }
        if s.width <= left {
            left -= s.width;
            out.push(s);
        } else if !s.raw {
            let text: String = s.text.chars().take(left).collect();
            out.push(Span { bold: s.bold, ..Span::new(text, s.color) });
            left = 0;
        } else {
            break;
        }
    }
    if width > 0 {
        out.push(Span::colored("…", TXT_DIM));
    }
    *spans = out;
}

/// Strip ANSI CSI sequences (tests and width checks). Not a sanitizer: it
/// leaves OSC, DCS and lone control characters behind. Output paths use
/// [`sanitize_single_line`] (see [`to_ansi`]).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.next() == Some('[') {
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

// ── Formatting helpers ─────────────────────────────────────────────────────

/// `15624 -> 15.6K`, `300000 -> 300K`, `1250000 -> 1.25M`, `950 -> 950`.
pub fn compact(n: u64) -> String {
    fn trim(s: String) -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    }
    if n >= 1_000_000 {
        trim(format!("{:.2}", n as f64 / 1e6)) + "M"
    } else if n >= 1000 {
        trim(format!("{:.1}", n as f64 / 1e3)) + "K"
    } else {
        n.to_string()
    }
}

/// One decimal below 10 percent, whole numbers above.
pub fn percent_text(pct: f64) -> String {
    if pct < 9.95 {
        trim_zero(format!("{pct:.1}")) + "%"
    } else {
        format!("{pct:.0}%")
    }
}

fn trim_zero(s: String) -> String {
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Color bands by remaining context: >70% good, >40% warn, >20% low, else
/// critical.
pub fn percent_color(used_pct: f64) -> Role {
    let remaining = 100.0 - used_pct;
    if remaining > 70.0 {
        GOOD_CLR
    } else if remaining > 40.0 {
        WARN_CLR
    } else if remaining > 20.0 {
        LOW_CLR
    } else {
        CRIT_CLR
    }
}

// ── Path and git (zsh-style path shortening) ─────────────────────────────────

/// `$HOME` → `~`, then keep the first component (two when it is `~` or the
/// root) and the leaf; everything between shortens to its first char.
pub fn shorten_path(dir: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    let dir = match dir.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => dir.to_string(),
    };
    let parts: Vec<&str> = dir.split('/').collect();
    let keep = if parts[0] == "~" || parts[0].is_empty() { 2 } else { 1 };
    if parts.len() <= keep + 1 {
        return dir;
    }
    let mut out = parts[0].to_string();
    for (i, p) in parts.iter().enumerate().skip(1) {
        out.push('/');
        if i < keep || i == parts.len() - 1 {
            out.push_str(p);
        } else {
            out.extend(p.chars().next());
        }
    }
    out
}

/// Longest `.git` pointer file or `HEAD` file read by [`git_branch`].
pub const GIT_READ_CAP: u64 = 4096;
/// Longest branch name shown.
pub const BRANCH_MAX_CHARS: usize = 128;

/// Read a small regular file: None when it is not a regular file (a FIFO
/// would block the open), is larger than [`GIT_READ_CAP`], or is unreadable.
fn read_capped(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > GIT_READ_CAP {
        return None;
    }
    let mut s = String::new();
    std::fs::File::open(path).ok()?.take(GIT_READ_CAP + 1).read_to_string(&mut s).ok()?;
    (s.len() as u64 <= GIT_READ_CAP).then_some(s)
}

/// The git dir a `.git` pointer file in `cur` names (`gitdir: <path>`), or
/// None when it resolves outside the workspace. Allowed targets, compared
/// after canonicalizing:
/// - anything inside `cur` itself;
/// - a linked worktree's admin dir whose `gitdir` back-link names this very
///   `.git` file (what `git worktree add` writes);
/// - a submodule git dir under an ancestor's `.git/modules/`.
fn pointer_gitdir(cur: &Path, dotgit: &Path) -> Option<PathBuf> {
    let s = read_capped(dotgit)?;
    let p = s.lines().next()?.trim().strip_prefix("gitdir:")?.trim();
    let target = if Path::new(p).is_absolute() { PathBuf::from(p) } else { cur.join(p) };
    let target = target.canonicalize().ok()?;
    let root = cur.canonicalize().ok()?;
    if target.starts_with(&root) {
        return Some(target);
    }
    let back_link =
        read_capped(&target.join("gitdir")).and_then(|b| PathBuf::from(b.lines().next()?.trim()).canonicalize().ok());
    if back_link.is_some() && back_link == dotgit.canonicalize().ok() {
        return Some(target);
    }
    if root.ancestors().skip(1).any(|a| target.starts_with(a.join(".git").join("modules"))) {
        return Some(target);
    }
    None
}

/// Branch for the repo containing `dir`: reads `.git/HEAD` directly, follows a
/// `gitdir:` pointer file (worktrees, submodules) when it stays inside the
/// workspace (see [`pointer_gitdir`]), walks up to the nearest repo. Detached
/// HEAD yields the 7-char short sha. No subprocesses. Reads are capped at
/// [`GIT_READ_CAP`]; the name is sanitized and cut to [`BRANCH_MAX_CHARS`].
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut d: Option<&Path> = Some(dir);
    while let Some(cur) = d {
        let dotgit = cur.join(".git");
        if let Ok(meta) = std::fs::metadata(&dotgit) {
            let gitdir: PathBuf = if meta.is_dir() { dotgit } else { pointer_gitdir(cur, &dotgit)? };
            let head = read_capped(&gitdir.join("HEAD"))?;
            let head = head.trim();
            let name = match head.strip_prefix("ref:") {
                Some(r) => {
                    let r = r.trim();
                    r.strip_prefix("refs/heads/").unwrap_or(r)
                }
                None if head.len() >= 7 && head.bytes().all(|b| b.is_ascii_hexdigit()) => &head[..7],
                None => return None,
            };
            let name: String = sanitize_single_line(name).chars().take(BRANCH_MAX_CHARS).collect();
            return (!name.is_empty()).then_some(name);
        }
        d = cur.parent();
    }
    None
}

// ── S2 pieces ──────────────────────────────────────────────────────────────

/// Below this width the legend shows swatches and short labels only.
pub const LEGEND_SWATCHES_WIDTH: usize = 60;

/// Glyph that follows the usage percent on row 1, so the band reads without color.
pub fn band_glyph(role: Role) -> char {
    match role {
        Role::Warn => '◐',
        Role::Low => '◑',
        Role::Crit => '○',
        _ => '●',
    }
}

/// One legend item: full (`■ system 3.3K`) or swatch-only (`■sys`).
fn legend_item(id: &str, tokens: u64, swatches: bool) -> Vec<Span> {
    let label = categories::short_label(id);
    if swatches {
        let short = match id {
            "system_prompt" => "sys",
            "subagents" => "sub",
            "summarized_conversation" => "sum",
            _ => label,
        };
        vec![Span::colored("■", Role::category(id)), Span::colored(short, PATH_LEAF_CLR)]
    } else {
        vec![
            Span::colored("■ ", Role::category(id)),
            Span::colored(label, PATH_LEAF_CLR),
            Span::colored(format!(" {}", compact(tokens)), TOKEN_CLR),
        ]
    }
}

/// Legend in fixed category order. When it does not fit in `width`, keep the
/// largest entries that do (still in fixed order) and fold the rest into a
/// `■ +n more <total>` tail (`+n` below [`LEGEND_SWATCHES_WIDTH`]). Empty when
/// not even one entry plus the tail fits.
pub fn legend(cats: &[(String, u64)], width: usize) -> Vec<Span> {
    legend_mode(cats, width, width < LEGEND_SWATCHES_WIDTH)
}

/// [`legend`] with the full or swatch-only form chosen by the caller.
fn legend_mode(cats: &[(String, u64)], width: usize, swatches: bool) -> Vec<Span> {
    let mut items: Vec<&(String, u64)> = cats.iter().filter(|c| c.1 > 0).collect();
    items.sort_by_key(|c| categories::order(&c.0));
    let build = |shown: &[bool]| -> Vec<Span> {
        let mut out: Vec<Span> = Vec::new();
        for (i, (id, tokens)) in items.iter().map(|c| (&c.0, c.1)).enumerate() {
            if !shown[i] {
                continue;
            }
            if !out.is_empty() {
                out.push(Span::new(" ", None));
            }
            out.extend(legend_item(id, tokens, swatches));
        }
        let hidden: Vec<u64> = items.iter().zip(shown).filter(|(_, s)| !**s).map(|(c, _)| c.1).collect();
        if !hidden.is_empty() {
            if !out.is_empty() {
                out.push(Span::new(" ", None));
            }
            if swatches {
                out.push(Span::colored(format!("+{}", hidden.len()), PATH_LEAF_CLR));
            } else {
                out.push(Span::colored("■ ", Role::Muted));
                out.push(Span::colored(format!("+{} more", hidden.len()), PATH_LEAF_CLR));
                out.push(Span::colored(format!(" {}", compact(hidden.iter().sum())), TOKEN_CLR));
            }
        }
        out
    };
    let n = items.len();
    let all = build(&vec![true; n]);
    if spans_width(&all) <= width {
        return all;
    }
    // Indices by size, largest first (ties in fixed order).
    let mut by_size: Vec<usize> = (0..n).collect();
    by_size.sort_by(|&a, &b| items[b].1.cmp(&items[a].1).then(a.cmp(&b)));
    for k in (1..n).rev() {
        let mut shown = vec![false; n];
        for &i in &by_size[..k] {
            shown[i] = true;
        }
        let row = build(&shown);
        if spans_width(&row) <= width {
            return row;
        }
    }
    Vec::new()
}

/// `trend ▁▂▃▅▆▇▇▇▼██ Δ +198`: the sparkline in secondary ink with compaction
/// cells drawn as `▼`, then the last delta. Empty when there are no values.
pub fn trend(values: &[u64], compaction: &[bool], delta: Option<i64>) -> Vec<Span> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut out = vec![Span::colored("trend ", TXT_DIM)];
    for mut s in sparkline(values, compaction, TXT_DIM) {
        if s.color == Some(COMPACTION_CLR) {
            s.text = "▼".repeat(s.width);
        }
        out.push(s);
    }
    if let Some(d) = delta {
        out.push(Span::new(" ", None));
        out.push(Span::colored("Δ ", TXT_DIM));
        out.push(Span::colored(delta_text(d), TXT_DIM));
    }
    out
}

// ── Rows ───────────────────────────────────────────────────────────────────

/// Timeline data for the row 2 trend: the last checkpoints' used tokens
/// (oldest first), their compaction flags and the last delta. Empty values
/// render no trend.
#[derive(Debug, Clone, Default)]
pub struct Trend {
    pub values: Vec<u64>,
    pub compaction: Vec<bool>,
    pub delta: Option<i64>,
    /// Seconds since the chat store was created (the wide layout's duration).
    pub duration_secs: Option<u64>,
}

/// Checkpoints shown in the row 2 sparkline.
pub const SPARK_LEN: usize = 12;
const SPARK_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Glyph for `v` on a `min..=max` scale, rounded to the nearest of the 8
/// levels: `min` is `▁`, `max` is `█`, a flat range is all `▁`.
pub fn spark_glyph(v: u64, min: u64, max: u64) -> char {
    if max <= min {
        return SPARK_GLYPHS[0];
    }
    let (v, range) = ((v.clamp(min, max) - min) as u128, (max - min) as u128);
    SPARK_GLYPHS[((v * 7 + range / 2) / range) as usize]
}

/// Sparkline of the last [`SPARK_LEN`] values (oldest first), scaled to the
/// window's own min..max. Per-turn growth is small next to the context size,
/// so scaling to `max_tokens` (or to 0..max) flattens every line to one glyph;
/// min..max shows the shape, the delta and row 1's numbers give the size.
/// Cells whose `compaction` flag is set use [`COMPACTION_CLR`], the others
/// `color`; adjacent cells of one color share a span. `compaction` is aligned
/// with `values` (missing flags count as false).
pub fn sparkline(values: &[u64], compaction: &[bool], color: Role) -> Vec<Span> {
    let start = values.len().saturating_sub(SPARK_LEN);
    let window = &values[start..];
    let min = window.iter().copied().min().unwrap_or(0);
    let max = window.iter().copied().max().unwrap_or(0);
    let mut spans: Vec<Span> = Vec::new();
    for (i, &v) in values.iter().enumerate().skip(start) {
        let c = if compaction.get(i).copied().unwrap_or(false) { COMPACTION_CLR } else { color };
        let g = spark_glyph(v, min, max);
        match spans.last_mut() {
            Some(last) if last.color == Some(c) => {
                last.text.push(g);
                last.width += 1;
            }
            _ => spans.push(Span::colored(g.to_string(), c)),
        }
    }
    spans
}

/// `+2.5K`, `-1.2K`, `+0`.
pub fn delta_text(d: i64) -> String {
    let sign = if d < 0 { '-' } else { '+' };
    format!("{sign}{}", compact(d.unsigned_abs()))
}

/// Delta span: green when tokens were freed, else the usage color.
pub fn delta_span(d: i64, usage_color: Role) -> Span {
    Span::colored(delta_text(d), if d < 0 { GOOD_CLR } else { usage_color })
}

/// Usage numbers for row 1, from the store or from the payload alone.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub used: Option<u64>,
    pub max: Option<u64>,
    pub percent: Option<f64>,
    /// `(category id, tokens)`; empty when only the payload is known.
    pub categories: Vec<(String, u64)>,
}

impl Usage {
    pub fn from_payload(p: &StatusPayload) -> Usage {
        let cw = p.cw();
        let percent = cw.used_percentage.or(cw.remaining_percentage.map(|r| 100.0 - r));
        let max = cw.context_window_size.filter(|m| *m > 0);
        let used = match (percent, max) {
            (Some(pct), Some(m)) => Some((pct.clamp(0.0, 100.0) / 100.0 * m as f64).round() as u64),
            _ => None,
        };
        Usage { used, max, percent: percent.map(|p| p.clamp(0.0, 100.0)), categories: Vec::new() }
    }

    pub fn from_details(td: &TokenDetails, p: &StatusPayload) -> Usage {
        let max = Some(td.max_tokens).filter(|m| *m > 0).or(p.cw().context_window_size.filter(|m| *m > 0));
        let percent = max.map(|m| (td.used_tokens as f64 * 100.0 / m as f64).clamp(0.0, 100.0));
        Usage {
            used: Some(td.used_tokens),
            max,
            percent,
            // Ids come from the store and reach the legend and the labeled
            // bar (a raw span) verbatim when they are not a known category.
            categories: td
                .categories
                .iter()
                .map(|c| (sanitize_single_line(&c.id).into_owned(), c.estimated_tokens))
                .collect(),
        }
    }
}

/// Row 1 with the ANSI bar as one raw span (the status line's exact bytes).
/// With `inline` it is the whole one-row status line (see [`Inline`]).
fn row1(u: &Usage, width: usize, theme: &Theme, style: BarStyle, inline: Option<&Inline>) -> Vec<Span> {
    context_row_fit(u, width, inline, |segs, bar_w, single| {
        let bar_str = match single {
            Some(role) => bar::ansi_with(segs, bar_w, theme, |_| theme.paint(role)),
            None => match style {
                BarStyle::Plain => bar::ansi(segs, bar_w, theme),
                BarStyle::Labeled => bar::ansi_labeled(segs, bar_w, theme),
            },
        };
        vec![Span::raw(bar_str, bar_w)]
    })
}

/// The status line's row 1 (stacked bar, `pct glyph ~used/max`, legend) as
/// plain styled spans: one span per bar segment in its category role (the
/// percent color when only the payload is known) and one `BarEmpty` span.
/// No span is `raw`, so the result maps directly onto other renderers (the
/// TUI header). Its visible text equals the status line's row 1.
pub fn context_row(u: &Usage, width: usize) -> Vec<Span> {
    context_row_with(u, width, bar_spans)
}

/// Bar spans: `segs` as `█` runs, the rest of the effective width as `░`.
/// `single` overrides every segment's color.
fn bar_spans(segs: &[bar::Segment], bar_w: usize, single: Option<Role>) -> Vec<Span> {
    let width = bar::effective_width(bar_w);
    let mut out = Vec::with_capacity(segs.len() + 1);
    let mut used = 0;
    for s in segs {
        let n = s.cells.min(width - used);
        if n == 0 {
            continue;
        }
        let role = single.unwrap_or_else(|| Role::category(&s.category_id));
        out.push(Span::colored(bar::FILLED.to_string().repeat(n), role));
        used += n;
    }
    if used < width {
        out.push(Span::colored(bar::EMPTY.to_string().repeat(width - used), Role::BarEmpty));
    }
    out
}

/// Row 1 layout; `bar(segments, bar width, single color)` draws the bar.
fn context_row_with(
    u: &Usage,
    width: usize,
    bar: impl FnOnce(&[bar::Segment], usize, Option<Role>) -> Vec<Span>,
) -> Vec<Span> {
    context_row_fit(u, width, None, bar)
}

/// Row 2 pieces placed between used/max and the legend in one-row mode:
/// `<bar> <pct glyph> <used/max>  <model> ‹effort›  <trend> <Δ>  <legend>`.
pub struct Inline {
    /// Bold model name (row 2's).
    pub model: Vec<Span>,
    /// `‹effort›` without a leading separator; may be empty.
    pub effort: Vec<Span>,
    /// Trend with the `Δ` and without it; both empty when there is no trend.
    pub trend: Vec<Span>,
    pub trend_no_delta: Vec<Span>,
}

impl Inline {
    pub fn new(p: &StatusPayload, t: &Trend) -> Inline {
        let (model, effort) = model_badge(p);
        Inline {
            model,
            effort,
            trend: trend(&t.values, &t.compaction, t.delta),
            trend_no_delta: trend(&t.values, &t.compaction, None),
        }
    }

    /// `  model ‹effort›  trend`, with the effort and trend optional.
    fn spans(&self, effort: bool, trend: &[Span]) -> Vec<Span> {
        let mut out = vec![Span::new(SEP, None)];
        out.extend(self.model.iter().cloned());
        if effort && !self.effort.is_empty() {
            out.push(Span::new(" ", None));
            out.extend(self.effort.iter().cloned());
        }
        if !trend.is_empty() {
            out.push(Span::new(SEP, None));
            out.extend(trend.iter().cloned());
        }
        out
    }
}

/// [`context_row_with`], or the one-row status line when `inline` is set.
fn context_row_fit(
    u: &Usage,
    width: usize,
    inline: Option<&Inline>,
    bar: impl FnOnce(&[bar::Segment], usize, Option<Role>) -> Vec<Span>,
) -> Vec<Span> {
    let pct_color = u.percent.map_or(TXT_DIM, percent_color);
    let numbers = vec![
        Span::new(" ", None),
        Span::colored(u.percent.map_or("--%".into(), percent_text), pct_color),
        Span::new(" ", None),
        Span::colored(band_glyph(pct_color).to_string(), pct_color),
    ];
    let tokens: Vec<Span> = match (u.used, u.max) {
        (Some(used), Some(max)) => {
            vec![Span::new(" ", None), Span::colored(format!("~{}/{}", compact(used), compact(max)), TOKEN_CLR)]
        }
        _ => Vec::new(),
    };
    let sep_w = SEP.chars().count();
    let full_bar = (width * 30 / 100).clamp(bar::MIN_WIDTH, 40);
    // Swatches only on narrow terminals; on wider ones fall back to swatches
    // when not even one full entry fits the remaining room.
    let legend_for = |room: usize| -> Vec<Span> {
        if u.categories.is_empty() {
            return Vec::new();
        }
        let swatches = width < LEGEND_SWATCHES_WIDTH;
        let l = legend_mode(&u.categories, room, swatches);
        if l.is_empty() && !swatches {
            legend_mode(&u.categories, room, true)
        } else {
            l
        }
    };

    // Fit: the legend folds and goes swatch-only by itself; when it still
    // gets no room (or the numbers overflow), drop used/max, then shrink the
    // bar toward bar::MIN_WIDTH.
    let nums_w = spans_width(&numbers);
    let tok_w = spans_width(&tokens);
    let (with_tokens, bar_w, middle, legend) = match inline {
        None => {
            let configs = [(true, full_bar), (false, full_bar), (false, bar::MIN_WIDTH)];
            let mut chosen: Option<(bool, usize, Vec<Span>)> = None;
            for (with_tokens, bar_w) in configs {
                if with_tokens && tokens.is_empty() {
                    continue;
                }
                let fixed = bar_w + nums_w + if with_tokens { tok_w } else { 0 };
                if fixed > width {
                    continue;
                }
                let legend = legend_for(width.saturating_sub(fixed + sep_w));
                let done = !legend.is_empty() || u.categories.is_empty();
                if done || chosen.is_none() {
                    chosen = Some((with_tokens, bar_w, legend));
                }
                if done {
                    break;
                }
            }
            let (with_tokens, bar_w, legend) = chosen.unwrap_or((false, bar::MIN_WIDTH, Vec::new()));
            (with_tokens, bar_w, Vec::new(), legend)
        }
        // One row: the legend folds, goes swatch-only, then is dropped (it
        // gets whatever room the rest leaves); then drop the Δ, the trend,
        // the effort and used/max in turn, then shrink the bar. Drops are
        // cumulative: the legend never returns once a later piece is dropped.
        Some(m) => {
            let configs = [
                (true, full_bar, m.spans(true, &m.trend)),
                (true, full_bar, m.spans(true, &m.trend_no_delta)),
                (true, full_bar, m.spans(true, &[])),
                (true, full_bar, m.spans(false, &[])),
                (false, full_bar, m.spans(false, &[])),
                (false, bar::MIN_WIDTH, m.spans(false, &[])),
            ];
            let last = configs.len() - 1;
            let mut chosen = None;
            for (i, (with_tokens, bar_w, middle)) in configs.into_iter().enumerate() {
                let fixed = bar_w + nums_w + if with_tokens { tok_w } else { 0 } + spans_width(&middle);
                if fixed <= width || i == last {
                    let legend = if i == 0 { legend_for(width.saturating_sub(fixed + sep_w)) } else { Vec::new() };
                    chosen = Some((with_tokens, bar_w, middle, legend));
                    break;
                }
            }
            chosen.expect("the last config is always chosen")
        }
    };

    let (segs, single) = if u.categories.is_empty() {
        let cells = match (u.used, u.max, u.percent) {
            (Some(used), Some(max), _) => bar::filled_cells(used, max, bar_w),
            (_, _, Some(pct)) => bar::filled_cells((pct * 100.0).round() as u64, 10_000, bar_w),
            _ => 0,
        };
        (vec![bar::Segment { category_id: String::new(), cells }], Some(pct_color))
    } else {
        let max = u.max.unwrap_or_else(|| u.categories.iter().map(|c| c.1).sum());
        (bar::segments(&u.categories, max, bar_w), None)
    };

    // A raw placeholder holds the bar's place while fitting, so truncation
    // drops the bar whole whichever form `bar` returns.
    let mut row = vec![Span::raw(String::new(), bar_w)];
    row.extend(numbers);
    if with_tokens {
        row.extend(tokens);
    }
    row.extend(middle);
    if !legend.is_empty() {
        row.push(Span::new(SEP, None));
        row.extend(legend);
    }
    truncate(&mut row, width);
    if row.first().is_some_and(|s| s.raw) {
        row.splice(0..1, bar(&segs, bar_w, single));
    }
    row
}

/// The bold model name and the `‹param›` badge (empty without a summary).
pub(crate) fn model_badge(p: &StatusPayload) -> (Vec<Span>, Vec<Span>) {
    let summary = p.model.as_ref().and_then(|m| m.param_summary.clone()).filter(|s| !s.trim().is_empty());
    let mut name = p.model_name().unwrap_or("Cursor").to_string();
    if let Some(s) = &summary {
        // displayName often already ends with the summary ("Claude Opus 5.5 300K Medium").
        if let Some(stripped) = name.strip_suffix(s.as_str()) {
            if !stripped.trim().is_empty() {
                name = stripped.trim_end().to_string();
            }
        }
    }
    let name = vec![Span::colored(name, MODEL_CLR).bold()];
    let badge: Vec<Span> = match summary {
        Some(s) => vec![Span::colored("‹", TXT_DIM), Span::colored(s, Role::Text), Span::colored("›", TXT_DIM)],
        None => Vec::new(),
    };
    (name, badge)
}

/// Row 2: model name, `‹param›` badge, path, `git:branch` (unless `no_git`),
/// and the trend right-aligned to `width`.
fn row2(p: &StatusPayload, trend: &[Span], width: usize, no_git: bool) -> Vec<Span> {
    let (name, mut badge) = model_badge(p);
    if !badge.is_empty() {
        badge.insert(0, Span::new(SEP, None));
    }

    let dir = p
        .work_dir()
        .map(str::to_string)
        .unwrap_or_else(|| std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default());
    let home = std::env::var("HOME").unwrap_or_default();
    let short = shorten_path(&dir, &home);
    let (base, leaf) = match short.rfind('/') {
        Some(i) => (&short[..=i], &short[i + 1..]),
        None => ("", short.as_str()),
    };
    let mut path = Vec::new();
    if !base.is_empty() {
        path.push(Span::colored(base, PATH_BASE_CLR));
    }
    path.push(Span::colored(leaf, PATH_LEAF_CLR).bold());
    let git: Vec<Span> = match (!no_git).then(|| git_branch(Path::new(&dir))).flatten() {
        Some(branch) => {
            vec![Span::new(SEP, None), Span::colored("git:", TXT_DIM), Span::colored(branch, BRANCH_CLR).bold()]
        }
        None => Vec::new(),
    };

    // Fit: drop the trend, then the badge, then truncate the path (keeping
    // the branch when possible), then truncate the whole row.
    let sep_w = SEP.chars().count();
    let left_w = |badge: &[Span], path: &[Span]| {
        spans_width(&name) + spans_width(badge) + sep_w + spans_width(path) + spans_width(&git)
    };
    let mut trend = trend.to_vec();
    if trend.is_empty() || left_w(&badge, &path) + sep_w + spans_width(&trend) > width {
        trend.clear();
        if left_w(&badge, &path) > width {
            badge.clear();
        }
        let over = left_w(&badge, &path).saturating_sub(width);
        // Keep at least two path cells (one char and the ellipsis).
        if over > 0 && spans_width(&path) >= over + 2 {
            let room = spans_width(&path) - over;
            truncate(&mut path, room);
        }
    }

    let mut row = name;
    row.extend(badge);
    row.push(Span::new(SEP, None));
    row.extend(path);
    row.extend(git);
    if !trend.is_empty() {
        let pad = width - spans_width(&row) - spans_width(&trend);
        row.push(Span::new(" ".repeat(pad), None));
        row.extend(trend);
    }
    truncate(&mut row, width);
    row
}

/// Render the rows (no trailing newline). `details` is the store's token
/// breakdown; `None` renders from the payload's `context_window` only.
pub fn render_with(p: &StatusPayload, details: Option<&TokenDetails>, trend: &Trend, opts: RenderOpts) -> String {
    render_themed(p, details, trend, theme::current(), opts)
}

/// [`render_with`] with an explicit theme.
pub fn render_themed(
    p: &StatusPayload,
    details: Option<&TokenDetails>,
    t: &Trend,
    theme: &Theme,
    opts: RenderOpts,
) -> String {
    let width = p.width();
    let usage = match details {
        Some(td) => Usage::from_details(td, p),
        None => Usage::from_payload(p),
    };
    match opts.rows {
        Rows::Two if opts.layout == Layout::Wide => {
            let [r1, r2] = crate::render::wide::rows(p, &usage, t, theme, opts.bar, width, opts.no_git);
            [to_ansi(&r1, theme), to_ansi(&r2, theme)].join("\n")
        }
        Rows::Two => {
            let trend_spans = trend(&t.values, &t.compaction, t.delta);
            let r1 = row1(&usage, width, theme, opts.bar, None);
            let r2 = row2(p, &trend_spans, width, opts.no_git);
            [to_ansi(&r1, theme), to_ansi(&r2, theme)].join("\n")
        }
        // One row: row 1 with row 2's model, effort and trend inlined; no
        // path or branch.
        Rows::One => to_ansi(&row1(&usage, width, theme, opts.bar, Some(&Inline::new(p, t))), theme),
    }
}

/// Payload-only render (no store).
pub fn render(p: &StatusPayload) -> anyhow::Result<String> {
    Ok(render_with(p, None, &Trend::default(), RenderOpts::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::breakdown::BreakdownEntry;
    use crate::theme::{Family, Mode};

    fn details() -> TokenDetails {
        let c = |id: &str, t| BreakdownEntry { id: id.into(), label: id.into(), estimated_tokens: t };
        TokenDetails {
            used_tokens: 15624,
            max_tokens: 300_000,
            categories: vec![
                c("system_prompt", 3346),
                c("tools", 6354),
                c("rules", 630),
                c("skills", 1884),
                c("mcp", 456),
                c("subagents", 1513),
                c("summarized_conversation", 0),
                c("conversation", 1441),
            ],
        }
    }

    fn payload(width: usize) -> StatusPayload {
        StatusPayload::parse(&format!(
            r#"{{"session_id":"x","cwd":"/tmp","model":{{"display_name":"Claude Opus 5.5 300K Medium","param_summary":"300K Medium"}},
               "context_window":{{"used_percentage":5,"context_window_size":300000}},"render_width_chars":{width}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn compact_formats() {
        assert_eq!(compact(15624), "15.6K");
        assert_eq!(compact(300_000), "300K");
        assert_eq!(compact(121_800), "121.8K");
        assert_eq!(compact(1_250_000), "1.25M");
        assert_eq!(compact(999), "999");
        assert_eq!(percent_text(5.208), "5.2%");
        assert_eq!(percent_text(5.0), "5%");
        assert_eq!(percent_text(61.4), "61%");
    }

    #[test]
    fn shorten_matches_zsh() {
        let h = "/Users/u";
        assert_eq!(shorten_path("/Users/u/projects/20261020/work/example-app", h), "~/projects/2/w/example-app");
        assert_eq!(shorten_path("/Users/u", h), "~");
        assert_eq!(shorten_path("/Users/u/a/b", h), "~/a/b");
        assert_eq!(shorten_path("/usr/local/bin", h), "/usr/l/bin");
        assert_eq!(shorten_path("/usr/local", h), "/usr/local");
        assert_eq!(shorten_path("/Users/ux/a/b/c", h), "/Users/u/a/b/c");
    }

    fn trend12() -> Trend {
        let values: Vec<u64> = (0..12).map(|i| 14_000 + i * 150).collect();
        let mut compaction = vec![false; 12];
        compaction[8] = true;
        Trend { values, compaction, delta: Some(2500), duration_secs: None }
    }

    /// The S2 layout these tests were written against.
    fn classic() -> RenderOpts {
        RenderOpts { layout: Layout::Classic, ..RenderOpts::default() }
    }

    fn lines(out: &str) -> Vec<String> {
        strip_ansi(out).lines().map(str::to_string).collect()
    }

    #[test]
    fn rows_fit_width() {
        for w in [20, 40, 60, 80, 100, 120, 200] {
            let p = payload(w);
            for td in [None, Some(details())] {
                let out = render_with(&p, td.as_ref(), &trend12(), classic());
                for line in out.lines() {
                    let n = strip_ansi(line).chars().count();
                    assert!(n <= w, "width {w}: {n} chars: {:?}", strip_ansi(line));
                }
                assert_eq!(out.lines().count(), 2, "{out}");
            }
        }
    }

    #[test]
    fn plain_theme_same_text_no_color() {
        let p = payload(120);
        for td in [None, Some(details())] {
            let light =
                render_themed(&p, td.as_ref(), &trend12(), &Theme::preset(Family::Modus, Mode::Light), classic());
            let plain = render_themed(&p, td.as_ref(), &trend12(), &Theme::plain(), classic());
            assert!(light.contains("\x1b[38;2;"));
            assert!(!plain.contains("\x1b[38"), "{plain:?}");
            assert_eq!(strip_ansi(&plain), strip_ansi(&light));
        }
    }

    #[test]
    fn to_ansi_escapes() {
        let light = Theme::preset(Family::Modus, Mode::Light);
        let spans = [Span::new("a", None), Span::colored("b", Role::Text).bold()];
        assert_eq!(to_ansi(&spans, &light), "a\x1b[1m\x1b[38;2;0;0;0mb\x1b[0m");
        assert_eq!(to_ansi(&[Span::colored("x", Role::Crit)], &Theme::plain()), "x");
    }

    #[test]
    fn context_row_text_matches_row1() {
        let t = Theme::preset(crate::theme::Family::Modus, crate::theme::Mode::Light);
        let p = payload(120);
        for u in [Usage::from_details(&details(), &p), Usage::from_payload(&p), Usage::default()] {
            for width in [5, 12, 30, 49, 50, 80, 90, 120, 200] {
                let spans = context_row(&u, width);
                assert!(spans.iter().all(|s| !s.raw), "width {width}");
                assert_eq!(
                    spans_width(&spans),
                    spans_width(&row1(&u, width, &t, BarStyle::Plain, None)),
                    "width {width}"
                );
                assert_eq!(
                    text(&spans),
                    strip_ansi(&to_ansi(&row1(&u, width, &t, BarStyle::Plain, None), &t)),
                    "width {width}"
                );
            }
        }
    }

    /// `--rows` and `--bar` combine: the defaults are the S2 render, `labeled`
    /// changes only the bar (and only when there is a breakdown to label),
    /// and `--rows 1` yields a single row in either bar style.
    #[test]
    fn render_opts_combine() {
        let t = Theme::preset(Family::Modus, Mode::Light);
        for width in [24, 50, 90, 120] {
            for td in [None, Some(details())] {
                let p = payload(width);
                let base = render_themed(&p, td.as_ref(), &trend12(), &t, classic());
                assert_eq!(
                    render_themed(
                        &p,
                        td.as_ref(),
                        &trend12(),
                        &t,
                        RenderOpts { rows: Rows::Two, bar: BarStyle::Plain, layout: Layout::Classic, no_git: false }
                    ),
                    base
                );
                let labeled = render_themed(
                    &p,
                    td.as_ref(),
                    &trend12(),
                    &t,
                    RenderOpts { rows: Rows::Two, bar: BarStyle::Labeled, layout: Layout::Classic, no_git: false },
                );
                assert_eq!(labeled != base, td.is_some(), "width {width}");
                for bar in [BarStyle::Plain, BarStyle::Labeled] {
                    let one = render_themed(
                        &p,
                        td.as_ref(),
                        &trend12(),
                        &t,
                        RenderOpts { rows: Rows::One, bar, layout: Layout::Classic, no_git: false },
                    );
                    assert_ne!(one, base, "width {width}");
                    assert!(!one.contains('\n'), "width {width}: {one:?}");
                }
            }
        }
    }

    fn one_row(width: usize, td: Option<&TokenDetails>) -> String {
        let opts = RenderOpts { rows: Rows::One, ..classic() };
        render_themed(&payload(width), td, &trend12(), &Theme::preset(Family::Modus, Mode::Light), opts)
    }

    #[test]
    fn one_row_fits_width() {
        for w in [20, 40, 60, 90, 120, 160] {
            for td in [None, Some(details())] {
                let out = one_row(w, td.as_ref());
                assert!(!out.contains('\n'), "width {w}: {out:?}");
                let line = strip_ansi(&out);
                assert!(line.chars().count() <= w, "width {w}: {} chars: {line:?}", line.chars().count());
                assert!(!line.contains("tmp") && !line.contains("git:"), "width {w}: {line:?}");
            }
        }
    }

    #[test]
    fn one_row_all_pieces_at_160() {
        let line = strip_ansi(&one_row(160, Some(&details())));
        assert!(line.starts_with('█'), "{line}");
        assert!(line.contains(" 5.2% ● ~15.6K/300K  Claude Opus 5.5 ‹300K Medium›  trend "), "{line}");
        assert!(line.contains('▼') && line.contains("Δ +2.5K"), "{line}");
        assert!(line.contains("Δ +2.5K  ■ "), "{line}");
        assert!(line.contains("tools 6.4K"), "{line}");
    }

    #[test]
    fn one_row_drops_in_order() {
        let has = |w: usize| {
            let l = strip_ansi(&one_row(w, Some(&details())));
            (l.contains("■"), l.contains('Δ'), l.contains("trend"), l.contains('‹'), l.contains("/300K"))
        };
        // Each piece disappears no later than the one dropped after it.
        let mut prev = (true, true, true, true, true);
        for w in (20..=160).rev() {
            let cur = has(w);
            let v = [cur.0, cur.1, cur.2, cur.3, cur.4];
            for i in 1..v.len() {
                assert!(!v[i - 1] || v[i], "width {w}: {cur:?}");
            }
            let p = [prev.0, prev.1, prev.2, prev.3, prev.4];
            assert!(v.iter().zip(p).all(|(c, p)| !*c || p), "width {w}: {cur:?} after {prev:?}");
            prev = cur;
        }
        let narrow = strip_ansi(&one_row(40, Some(&details())));
        assert!(narrow.starts_with('█') && narrow.contains("Claude"), "{narrow}");
        assert!(narrow.chars().count() <= 40, "{narrow}");
    }

    #[test]
    fn labeled_bar_differs_from_plain_at_same_width() {
        let t = Theme::preset(Family::Modus, Mode::Light);
        let labeled = RenderOpts { bar: BarStyle::Labeled, ..classic() };
        for width in [24, 50, 90, 120] {
            let p = payload(width);
            let td = details();
            let plain = render_themed(&p, Some(&td), &trend12(), &t, classic());
            let lab = render_themed(&p, Some(&td), &trend12(), &t, labeled);
            assert_ne!(lab, plain, "width {width}");
            let (sp, sl) = (strip_ansi(&plain), strip_ansi(&lab));
            assert_eq!(
                sl.lines().map(|l| l.chars().count()).collect::<Vec<_>>(),
                sp.lines().map(|l| l.chars().count()).collect::<Vec<_>>(),
                "width {width}: {sl:?}"
            );
            // Row 2 is untouched.
            assert_eq!(lab.lines().nth(1), plain.lines().nth(1), "width {width}");
        }
        // With only the payload, the bar is single-colored and stays plain.
        let p = payload(90);
        assert_eq!(
            render_themed(&p, None, &trend12(), &t, labeled),
            render_themed(&p, None, &trend12(), &t, classic())
        );
    }

    #[test]
    fn row1_numbers_and_name() {
        let out = lines(&render_with(&payload(120), Some(&details()), &Trend::default(), classic()));
        let (first, second) = (&out[0], &out[1]);
        assert!(first.starts_with('█'), "{first}");
        assert!(first.contains(" 5.2% ● ~15.6K/300K  ■ system 3.3K"), "{first}");
        assert!(first.chars().count() <= 120, "{first}");
        assert!(!first.contains("Claude"), "{first}");
        assert!(second.starts_with("Claude Opus 5.5  ‹300K Medium›  "), "{second}");
        assert!(second.contains("tmp"), "{second}");
        assert!(!second.contains("trend"), "{second}");

        let out = lines(&render_with(&payload(120), Some(&details()), &trend12(), classic()));
        let second = &out[1];
        assert!(second.starts_with("Claude Opus 5.5  ‹300K Medium›  "), "{second}");
        assert!(second.contains("trend "), "{second}");
        assert!(second.ends_with("Δ +2.5K"), "{second}");
        assert_eq!(second.chars().count(), 120, "{second}");
    }

    #[test]
    fn payload_only_and_tolerant_parse() {
        let p = StatusPayload::parse(r#"{"session_id":7,"model":"weird","context_window":{"used_percentage":"x"}}"#)
            .unwrap();
        assert!(p.session_id.is_none() && p.model.is_none());
        let out = strip_ansi(&render(&p).unwrap());
        assert!(out.contains("--%"), "{out}");
        let out = strip_ansi(&render(&payload(80)).unwrap());
        assert!(out.contains("  ~15K/300K"), "{out}");
        let out = strip_ansi(&render_with(&p, None, &Trend::default(), classic()));
        assert!(out.contains("--% ●"), "{out}");
    }

    #[test]
    fn row1_legend_folds_at_90_and_swatches_at_50() {
        let out = lines(&render_with(&payload(90), Some(&details()), &Trend::default(), classic()));
        assert!(out[0].contains('+') && out[0].contains("more"), "{}", out[0]);
        assert!(out[0].chars().count() <= 90, "{}", out[0]);
        let out = lines(&render_with(&payload(50), Some(&details()), &Trend::default(), classic()));
        assert!(out[0].contains("■sys"), "{}", out[0]);
        assert!(!out[0].contains("more"), "{}", out[0]);
        assert!(out[0].chars().count() <= 50, "{}", out[0]);
    }

    #[test]
    fn row2_drops_trend_then_badge() {
        let out = lines(&render_with(&payload(40), Some(&details()), &trend12(), classic()));
        assert!(!out[1].contains("trend"), "{}", out[1]);
        assert!(out[1].contains('‹'), "{}", out[1]);
        let out = lines(&render_with(&payload(24), Some(&details()), &trend12(), classic()));
        assert!(!out[1].contains('‹'), "{}", out[1]);
        assert!(out[1].starts_with("Claude Opus 5.5"), "{}", out[1]);
    }

    #[test]
    fn git_head_variants() {
        let tmp = temp_dir("git");
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub/dir")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        assert_eq!(git_branch(&repo).as_deref(), Some("feat/x"));
        assert_eq!(git_branch(&repo.join("sub/dir")).as_deref(), Some("feat/x"));
        // Worktree pointer file with a relative gitdir.
        let wt = tmp.join("wt");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
        // Outside the worktree and without git's back-link: refused.
        assert_eq!(git_branch(&wt), None);
        std::fs::write(repo.join(".git/worktrees/wt/gitdir"), format!("{}\n", wt.join(".git").display())).unwrap();
        assert_eq!(git_branch(&wt).as_deref(), Some("0123456"));
    }

    #[test]
    fn sparkline_glyphs() {
        let text = |s: &[Span]| s.iter().map(|x| x.text.as_str()).collect::<String>();
        assert!(sparkline(&[], &[], GOOD_CLR).is_empty());
        assert_eq!(text(&sparkline(&[500], &[], GOOD_CLR)), "▁");
        assert_eq!(text(&sparkline(&[7, 7, 7], &[], GOOD_CLR)), "▁▁▁");
        assert_eq!(text(&sparkline(&[1, 2, 3, 4, 5, 6, 7, 8], &[], GOOD_CLR)), "▁▂▃▄▅▆▇█");
        assert_eq!(text(&sparkline(&[14406, 14673, 14916, 15242, 15624], &[], GOOD_CLR)), "▁▃▄▆█");
        assert_eq!(spark_glyph(1000, 1000, 2000), '▁');
        assert_eq!(spark_glyph(1071, 1000, 2000), '▁');
        assert_eq!(spark_glyph(1072, 1000, 2000), '▂');
        assert_eq!(spark_glyph(5000, 1000, 2000), '█');
        // 15 values: only the last 12 are drawn, scaled to their own max.
        let vals: Vec<u64> = (1..=15).map(|v| v * 100).collect();
        let s = sparkline(&vals, &[], GOOD_CLR);
        assert_eq!(spans_width(&s), SPARK_LEN);
        assert_eq!(s.len(), 1);
        assert!(text(&s).ends_with('█'));
        // Compaction cells get their own color; flags align with the input.
        let mut flags = vec![false; 15];
        flags[13] = true;
        flags[0] = true; // outside the window
        let s = sparkline(&vals, &flags, GOOD_CLR);
        let colors: Vec<_> = s.iter().map(|x| (x.color, x.width)).collect();
        assert_eq!(colors, vec![(Some(GOOD_CLR), 10), (Some(COMPACTION_CLR), 1), (Some(GOOD_CLR), 1)]);
        assert_eq!(delta_text(2500), "+2.5K");
        assert_eq!(delta_text(-1200), "-1.2K");
        assert_eq!(delta_span(-5, CRIT_CLR).color, Some(GOOD_CLR));
        assert_eq!(delta_span(5, CRIT_CLR).color, Some(CRIT_CLR));
    }

    fn text(s: &[Span]) -> String {
        s.iter().map(|x| x.text.as_str()).collect()
    }

    fn cats() -> Vec<(String, u64)> {
        details().categories.iter().map(|c| (c.id.clone(), c.estimated_tokens)).collect()
    }

    #[test]
    fn legend_folds_tail() {
        let l = legend(&cats(), 60);
        let t = text(&l);
        assert!(spans_width(&l) <= 60, "{t}");
        assert!(t.starts_with("■ system"), "{t}");
        let (head, total) = t.rsplit_once(" more ").expect(&t);
        assert!(total.chars().all(|c| c.is_ascii_digit() || c == '.' || c == 'K'), "{t}");
        let hidden: usize = head.rsplit_once('+').unwrap().1.parse().unwrap();
        assert!(hidden > 0, "{t}");
        assert_eq!(l.last().unwrap().color, Some(TOKEN_CLR));
        // Fixed order, not by size: tools (largest) still follows system.
        assert!(t.find("tools").unwrap() > t.find("system").unwrap());

        let l = legend(&cats(), 120);
        let t = text(&l);
        assert!(t.starts_with("■ system 3.3K ■ tools 6.4K"), "{t}");
        assert!(!t.contains("more"), "{t}");
        assert_eq!(t.matches('■').count(), 7, "{t}");
        for w in 0..130 {
            assert!(spans_width(&legend(&cats(), w)) <= w, "width {w}");
        }
        assert!(legend(&cats(), 0).is_empty());
    }

    #[test]
    fn legend_swatches_only_below_60() {
        let l = legend(&cats(), 40);
        let t = text(&l);
        assert!(spans_width(&l) <= 40, "{t}");
        assert!(t.starts_with("■sys"), "{t}");
        let body = t.rsplit_once(" +").map_or(t.as_str(), |(b, _)| b);
        assert!(!body.chars().any(|c| c.is_ascii_digit()), "{t}");
        let l = legend(&cats(), 25);
        let t = text(&l);
        assert!(spans_width(&l) <= 25, "{t}");
        let (body, n) = t.rsplit_once(" +").expect(&t);
        assert!(n.parse::<usize>().unwrap() > 0 && !body.chars().any(|c| c.is_ascii_digit()), "{t}");
    }

    #[test]
    fn trend_marks_compaction_with_triangle() {
        // spark_glyph rounds 3 on 1..=5 to level 4 of 0..7, so the middle cell is ▅.
        let s = trend(&[1, 2, 3, 4, 5], &[false, false, false, true, false], Some(198));
        assert_eq!(text(&s), "trend ▁▃▅▼█ Δ +198");
        let tri = s.iter().find(|x| x.text == "▼").unwrap();
        assert_eq!(tri.color, Some(COMPACTION_CLR));
        assert!(s.iter().filter(|x| x.text != "▼" && x.color.is_some()).all(|x| x.color == Some(TXT_DIM)));
        assert!(trend(&[], &[], None).is_empty());
        assert_eq!(text(&trend(&[3], &[], None)), "trend ▁");
        assert_eq!(band_glyph(GOOD_CLR), '●');
        assert_eq!(band_glyph(WARN_CLR), '◐');
        assert_eq!(band_glyph(LOW_CLR), '◑');
        assert_eq!(band_glyph(CRIT_CLR), '○');
        assert_eq!(band_glyph(TXT_DIM), '●');
    }

    /// A fresh empty dir unique to this test, canonicalized, removed on drop.
    fn temp_dir(tag: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(tag)
    }

    fn all_layouts() -> [RenderOpts; 4] {
        [
            RenderOpts::default(),
            RenderOpts { layout: Layout::Classic, ..RenderOpts::default() },
            RenderOpts { rows: Rows::One, ..RenderOpts::default() },
            RenderOpts { bar: BarStyle::Labeled, ..RenderOpts::default() },
        ]
    }

    #[test]
    fn huge_render_width_is_clamped() {
        let p = StatusPayload::parse(
            r#"{"cwd":"/tmp","model":{"display_name":"M"},"context_window":{"used_percentage":50,"context_window_size":1000},"render_width_chars":1000000000000}"#,
        )
        .unwrap();
        assert_eq!(p.width(), MAX_WIDTH);
        // Bigger than usize: the lenient field is dropped, the default applies.
        let p2 = StatusPayload::parse(r#"{"render_width_chars":100000000000000000000000}"#).unwrap();
        assert_eq!(p2.width(), DEFAULT_WIDTH);
        let theme = Theme::preset(Family::Modus, Mode::Light);
        for opts in all_layouts() {
            for td in [None, Some(details())] {
                let out = render_themed(&p, td.as_ref(), &Trend::default(), &theme, opts);
                for line in strip_ansi(&out).lines() {
                    assert!(line.chars().count() <= MAX_WIDTH, "{opts:?}: {}", line.chars().count());
                }
                // A few bytes per cell (3-byte glyphs plus SGR), never more.
                assert!(out.len() < 2 * MAX_WIDTH * 32, "{opts:?}: {} bytes", out.len());
            }
        }
    }

    #[test]
    fn gitdir_pointer_outside_workspace_is_refused() {
        let tmp = temp_dir("gitdir");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("HEAD"), "ref: refs/heads/secret\n").unwrap();
        let ws = tmp.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        for pointer in ["gitdir: ../outside\n".to_string(), format!("gitdir: {}\n", outside.display())] {
            std::fs::write(ws.join(".git"), pointer).unwrap();
            assert_eq!(git_branch(&ws), None);
        }
        // Inside the workspace is fine.
        let inner = ws.join("modules/x");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("HEAD"), "ref: refs/heads/inner\n").unwrap();
        std::fs::write(ws.join(".git"), "gitdir: modules/x\n").unwrap();
        assert_eq!(git_branch(&ws).as_deref(), Some("inner"));
        // A linked worktree whose admin dir links back to this `.git` file.
        let admin = tmp.join("main/.git/worktrees/ws");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::write(admin.join("gitdir"), format!("{}\n", ws.join(".git").display())).unwrap();
        std::fs::write(ws.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert_eq!(git_branch(&ws).as_deref(), Some("feature"));
        // The same admin dir linking somewhere else is refused.
        std::fs::write(admin.join("gitdir"), format!("{}\n", outside.display())).unwrap();
        assert_eq!(git_branch(&ws), None);
        // A submodule git dir under an ancestor's .git/modules.
        let sub = tmp.join(".git/modules/ws");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("HEAD"), "ref: refs/heads/sub\n").unwrap();
        std::fs::write(ws.join(".git"), "gitdir: ../.git/modules/ws\n").unwrap();
        assert_eq!(git_branch(&ws).as_deref(), Some("sub"));
    }

    #[test]
    fn git_reads_are_capped_and_branch_is_bounded() {
        let tmp = temp_dir("gitcap");
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        let head = tmp.join(".git/HEAD");
        std::fs::write(&head, format!("ref: refs/heads/{}\n", "b".repeat(GIT_READ_CAP as usize))).unwrap();
        assert_eq!(git_branch(&tmp), None);
        std::fs::write(&head, format!("ref: refs/heads/{}\n", "b".repeat(1000))).unwrap();
        assert_eq!(git_branch(&tmp).map(|b| b.chars().count()), Some(BRANCH_MAX_CHARS));
        // A HEAD that is not a regular file is never opened.
        std::fs::remove_file(&head).unwrap();
        std::fs::create_dir_all(&head).unwrap();
        assert_eq!(git_branch(&tmp), None);
    }

    #[test]
    fn hostile_branch_and_payload_strings_render_clean() {
        let tmp = temp_dir("osc-branch");
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        std::fs::write(tmp.join(".git/HEAD"), "ref: refs/heads/evil\x1b]52;c;QUJD\x07\u{9b}31m\n").unwrap();
        let branch = git_branch(&tmp).unwrap();
        assert!(crate::text::is_clean(&branch), "{branch:?}");
        let evil = r"\u001b]52;c;QUJD\u0007\u001bP+q\u001b\\\u009d0;t\u009c";
        let json = format!(
            r#"{{"cwd":"{cwd}/x{evil}","model":{{"display_name":"Claude{evil} 1M","param_summary":"1M{evil}"}},
               "context_window":{{"used_percentage":5,"context_window_size":300000}},"render_width_chars":300}}"#,
            cwd = tmp.display()
        );
        std::fs::create_dir_all(tmp.join("x\x1b]52;c;QUJD\x07\x1bP+q\x1b\\\u{9d}0;t\u{9c}")).unwrap();
        let p = StatusPayload::parse(&json).unwrap();
        let mut td = details();
        td.categories.push(BreakdownEntry {
            id: format!("evil{}", "\x1b]52;c;QUJD\x07"),
            label: String::new(),
            estimated_tokens: 90_000,
        });
        for theme in [Theme::preset(Family::Modus, Mode::Light), Theme::plain()] {
            for opts in all_layouts() {
                for d in [None, Some(&td)] {
                    let out = render_themed(&p, d, &Trend::default(), &theme, opts);
                    let visible = strip_ansi(&out).replace('\n', "");
                    assert!(crate::text::is_clean(&visible), "{opts:?}: {visible:?}");
                    assert!(!out.contains("\x1b]") && !out.contains("\x1bP"), "{opts:?}: {out:?}");
                    if opts.rows == Rows::Two {
                        assert!(visible.contains("git:evil"), "{opts:?}: {visible:?}");
                    }
                }
            }
        }
    }
}
