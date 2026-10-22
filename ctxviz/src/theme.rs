//! Color theme: semantic roles mapped to paints, plus resolution from the
//! `--theme` flag and the environment (`NO_COLOR`, `TERM`, `CTXVIZ_THEME`,
//! `COLORTERM`, `TERM_THEME`, `COLORFGBG`).
//!
//! Renderers ask for a [`Role`] instead of hard-coding colors, so one table
//! drives the bar, the statusline, the TUI and the Kitty output.

use std::fmt::Write as _;
use std::sync::{Once, OnceLock};

use ratatui::style::{Color, Modifier, Style};

/// A 24-bit color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Lowercase `#rrggbb`.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// WCAG relative luminance in `0.0..=1.0`.
    pub fn luminance(self) -> f64 {
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    /// True when black text contrasts more than white on this color
    /// (WCAG contrast ratios are equal at luminance ~0.179).
    pub fn wants_dark_ink(self) -> bool {
        let l = self.luminance();
        (l + 0.05) / 0.05 >= 1.05 / (l + 0.05)
    }
}

/// Nominal xterm values for ANSI 0..15, used only to pick a readable ink on
/// an ANSI background (the terminal's real palette is unknown).
const XTERM16: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(205, 0, 0),
    Rgb(0, 205, 0),
    Rgb(205, 205, 0),
    Rgb(0, 0, 238),
    Rgb(205, 0, 205),
    Rgb(0, 205, 205),
    Rgb(229, 229, 229),
    Rgb(127, 127, 127),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(255, 255, 0),
    Rgb(92, 92, 255),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
    Rgb(255, 255, 255),
];

impl From<Rgb> for Color {
    fn from(c: Rgb) -> Color {
        Color::Rgb(c.0, c.1, c.2)
    }
}

/// How one role is drawn: terminal default, one of the 16 ANSI colors, or truecolor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Default,
    Ansi(u8),
    Rgb(Rgb),
}

impl Paint {
    /// Append the foreground SGR escape (nothing for `Default`).
    pub fn sgr_fg(self, out: &mut String) {
        self.sgr(out, 3, 9, 38);
    }

    /// Append the background SGR escape (nothing for `Default`).
    pub fn sgr_bg(self, out: &mut String) {
        self.sgr(out, 4, 10, 48);
    }

    fn sgr(self, out: &mut String, base: u8, bright: u8, ext: u8) {
        match self {
            Paint::Default => {}
            Paint::Ansi(n @ 0..=7) => {
                let _ = write!(out, "\x1b[{base}{n}m");
            }
            Paint::Ansi(n @ 8..=15) => {
                let _ = write!(out, "\x1b[{bright}{}m", n - 8);
            }
            // Outside the 16-color range: fall back to the 256-color form.
            Paint::Ansi(n) => {
                let _ = write!(out, "\x1b[{ext};5;{n}m");
            }
            Paint::Rgb(Rgb(r, g, b)) => {
                let _ = write!(out, "\x1b[{ext};2;{r};{g};{b}m");
            }
        }
    }

    /// The ratatui color. ANSI 0..15 map to the named colors so crossterm
    /// emits 3X/9X codes that follow the terminal's own palette.
    pub fn color(self) -> Color {
        match self {
            Paint::Default => Color::Reset,
            Paint::Ansi(n) => match n {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                15 => Color::White,
                n => Color::Indexed(n),
            },
            Paint::Rgb(c) => c.into(),
        }
    }

    /// Text color that reads on a background of this paint: black or white
    /// by relative luminance. Truecolor fills get truecolor ink; ANSI fills
    /// get ANSI black (`30`) or bright white (`97`). `Default` stays `Default`.
    pub fn ink_on(self) -> Paint {
        match self {
            Paint::Default => Paint::Default,
            Paint::Rgb(c) => Paint::Rgb(if c.wants_dark_ink() { Rgb(0, 0, 0) } else { Rgb(255, 255, 255) }),
            Paint::Ansi(n) => {
                let dark = XTERM16.get(n as usize).is_none_or(|c| c.wants_dark_ink());
                Paint::Ansi(if dark { 0 } else { 15 })
            }
        }
    }

    /// The truecolor value, or `fallback` for ANSI and default paints.
    pub fn rgb_or(self, fallback: Rgb) -> Rgb {
        match self {
            Paint::Rgb(c) => c,
            _ => fallback,
        }
    }
}

/// Semantic color slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Cat1,
    Cat2,
    Cat3,
    Cat4,
    Cat5,
    Cat6,
    Cat7,
    Cat8,
    Good,
    Warn,
    Low,
    Crit,
    Compaction,
    Text,
    Ink2,
    Muted,
    Border,
    Accent,
    SelectionBg,
    SelectionBgBlur,
    BarEmpty,
}

impl Role {
    pub const ALL: [Role; 21] = [
        Role::Cat1,
        Role::Cat2,
        Role::Cat3,
        Role::Cat4,
        Role::Cat5,
        Role::Cat6,
        Role::Cat7,
        Role::Cat8,
        Role::Good,
        Role::Warn,
        Role::Low,
        Role::Crit,
        Role::Compaction,
        Role::Text,
        Role::Ink2,
        Role::Muted,
        Role::Border,
        Role::Accent,
        Role::SelectionBg,
        Role::SelectionBgBlur,
        Role::BarEmpty,
    ];

    /// Position in [`Role::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The role for a category id, in the fixed category order; unknown ids are `Muted`.
    pub fn category(id: &str) -> Role {
        const CATS: [Role; 8] =
            [Role::Cat1, Role::Cat2, Role::Cat3, Role::Cat4, Role::Cat5, Role::Cat6, Role::Cat7, Role::Cat8];
        CATS.get(crate::model::categories::order(id)).copied().unwrap_or(Role::Muted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
    Ansi,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Modus,
    Catppuccin,
    RosePine,
    TokyoNight,
    Gruvbox,
}

impl Family {
    /// Parse a lowercase family name (`modus`, `catppuccin`, `rose-pine`,
    /// `tokyo-night`, `gruvbox`).
    pub fn parse(s: &str) -> Option<Family> {
        match s {
            "modus" => Some(Family::Modus),
            "catppuccin" => Some(Family::Catppuccin),
            "rose-pine" => Some(Family::RosePine),
            "tokyo-night" => Some(Family::TokyoNight),
            "gruvbox" => Some(Family::Gruvbox),
            _ => None,
        }
    }
}

const N_ROLES: usize = Role::ALL.len();

/// Category fills, Cat1..Cat8 (system_prompt, tools, rules, skills, mcp,
/// subagents, summarized_conversation, conversation). Shared by every family;
/// only the mode differs. Validated with the dataviz `validate_palette.js`
/// against a white (light) and a near-black (dark) surface.
const FILLS_LIGHT: [Rgb; 8] = [
    Rgb(0x71, 0x71, 0x71),
    Rgb(0x73, 0x66, 0xc6),
    Rgb(0x00, 0x87, 0x5f),
    Rgb(0x9c, 0x6a, 0x22),
    Rgb(0x86, 0x23, 0x68),
    Rgb(0x3e, 0x76, 0xbc),
    Rgb(0x00, 0xaf, 0xaf),
    Rgb(0xb9, 0x43, 0x26),
];
const FILLS_DARK: [Rgb; 8] = [
    Rgb(0x9a, 0x9a, 0x9a),
    Rgb(0x9b, 0x90, 0xe0),
    Rgb(0x2f, 0xae, 0x87),
    Rgb(0xc7, 0x91, 0x4a),
    Rgb(0xb8, 0x5a, 0x9a),
    Rgb(0x6f, 0x9f, 0xd9),
    Rgb(0x3c, 0xcb, 0xcb),
    Rgb(0xe0, 0x71, 0x4f),
];

/// Status text, Good/Warn/Low/Crit. Shared by every family.
const STATUS_LIGHT: [Rgb; 4] =
    [Rgb(0x00, 0x63, 0x00), Rgb(0x9a, 0x67, 0x00), Rgb(0xbc, 0x4c, 0x00), Rgb(0xcf, 0x22, 0x2e)];
const STATUS_DARK: [Rgb; 4] =
    [Rgb(0x3f, 0xb9, 0x50), Rgb(0xd2, 0x99, 0x22), Rgb(0xdb, 0x6d, 0x28), Rgb(0xff, 0x7b, 0x72)];

/// Per-preset chrome. `Compaction` reuses `ink2` and `BarEmpty` reuses `muted`.
///
/// `text` is the theme's `foreground` everywhere. Every preset pins
/// `ink2`, `muted` and `accent` by hand (Modus also pins `border` and `sel`);
/// the rest comes from the snap rule below, computed once from each Kitty
/// theme `.conf` (not parsed at runtime). The rule's ink2/muted/accent arms
/// are kept for reference; applied literally they collapse roles into fg for
/// some themes, which is why those three are hand-picked:
/// - `text` = `foreground`.
/// - `ink2` = first of color8, color7, color15 with WCAG contrast vs
///   `background` in [4.5, 10], else mix(fg, bg, 0.70).
/// - `muted` = the color8/color7/color15 candidate with contrast closest to
///   3.5 while >= 3.0, else mix(fg, bg, 0.50).
/// - `border` = mix(fg, bg, 0.35), `sel` = mix(fg, bg, 0.12),
///   `sel_blur` = mix(fg, bg, 0.06); the conf's `selection_background` is unused.
/// - `accent` = color5 if contrast >= 4.5, else color13 if so, else fg.
///
/// mix(a, b, t) is linear in 8-bit sRGB channels (t of a, 1 - t of b), rounded.
/// Modus's ink2/muted/border/accent/sel are hand-tuned from the Modus
/// palettes and are not produced by this rule.
#[derive(Clone, Copy)]
struct Chrome {
    text: Rgb,
    ink2: Rgb,
    muted: Rgb,
    border: Rgb,
    accent: Rgb,
    sel: Rgb,
    sel_blur: Rgb,
}

const fn hex(v: u32) -> Rgb {
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const fn chrome(text: u32, ink2: u32, muted: u32, border: u32, accent: u32, sel: u32, sel_blur: u32) -> Chrome {
    Chrome {
        text: hex(text),
        ink2: hex(ink2),
        muted: hex(muted),
        border: hex(border),
        accent: hex(accent),
        sel: hex(sel),
        sel_blur: hex(sel_blur),
    }
}

// Modus_Operandi.conf: bg #ffffff, fg #000000. Hand-tuned from the Modus palettes (ink2,
// muted, border, accent, sel); only sel_blur comes from the sRGB snap rule.
const MODUS_LIGHT: Chrome = chrome(0x000000, 0x595959, 0x8a8a8a, 0x9f9f9f, 0x721045, 0xe3e3e3, 0xf0f0f0);
// Modus_Vivendi.conf: bg #000000, fg #ffffff. Hand-tuned from the Modus palettes (ink2,
// muted, border, accent, sel); only sel_blur comes from the sRGB snap rule.
const MODUS_DARK: Chrome = chrome(0xffffff, 0xa6a6a6, 0x8a8a8a, 0x595959, 0xfeacd0, 0x303030, 0x0f0f0f);
// Catppuccin-Latte.conf: bg #eff1f5, fg #4c4f69. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const CATPPUCCIN_LIGHT: Chrome = chrome(0x4c4f69, 0x6c6f85, 0x9ca0b0, 0xb6b8c4, 0x8839ef, 0xdbdee4, 0xe5e7ed);
// Catppuccin-Mocha.conf: bg #1e1e2e, fg #cdd6f4. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const CATPPUCCIN_DARK: Chrome = chrome(0xcdd6f4, 0xa6adc8, 0x7f849c, 0x5b5e73, 0xcba6f7, 0x333446, 0x28293a);
// rose-pine-dawn.conf: bg #faf4ed, fg #575279. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const ROSE_PINE_LIGHT: Chrome = chrome(0x575279, 0x797593, 0x9893a5, 0xc1bbc4, 0xb4637a, 0xe6e1df, 0xf0eae6);
// rose-pine.conf: bg #191724, fg #e0def4. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const ROSE_PINE_DARK: Chrome = chrome(0xe0def4, 0x908caa, 0x6e6a86, 0x5f5d6d, 0xc4a7e7, 0x312f3d, 0x252330);
// tokyo_night_day.conf: bg #e1e2e7, fg #3760bf. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const TOKYO_NIGHT_LIGHT: Chrome = chrome(0x3760bf, 0x6172b0, 0x8990b3, 0xa6b4d9, 0x7847bd, 0xcdd2e2, 0xd7dae5);
// tokyo_night_night.conf: bg #1a1b26, fg #c0caf5. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const TOKYO_NIGHT_DARK: Chrome = chrome(0xc0caf5, 0xa9b1d6, 0x565f89, 0x54586e, 0xbb9af7, 0x2e303f, 0x242632);
// gruvbox-light.conf: bg #fbf1c7, fg #3c3836. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const GRUVBOX_LIGHT: Chrome = chrome(0x3c3836, 0x665c54, 0x928374, 0xb8b094, 0x8f3f71, 0xe4dbb6, 0xf0e6be);
// gruvbox-dark.conf: bg #282828, fg #ebdbb2. ink2/muted/accent hand-picked from the
// theme's own palette (plan mockup); border/sel/sel_blur from the sRGB mix rule.
const GRUVBOX_DARK: Chrome = chrome(0xebdbb2, 0xa89984, 0x928374, 0x6c6758, 0xd3869b, 0x3f3d39, 0x343330);

/// 16-color palette, indexed like [`Role::ALL`].
const ANSI: [Paint; N_ROLES] = [
    Paint::Ansi(6),  // Cat1
    Paint::Ansi(1),  // Cat2
    Paint::Ansi(5),  // Cat3
    Paint::Ansi(9),  // Cat4
    Paint::Ansi(2),  // Cat5
    Paint::Ansi(13), // Cat6
    Paint::Ansi(3),  // Cat7
    Paint::Ansi(4),  // Cat8
    Paint::Ansi(2),  // Good
    Paint::Ansi(3),  // Warn
    Paint::Ansi(11), // Low
    Paint::Ansi(1),  // Crit
    Paint::Ansi(8),  // Compaction
    Paint::Default,  // Text
    Paint::Ansi(8),  // Ink2
    Paint::Ansi(8),  // Muted
    Paint::Ansi(8),  // Border
    Paint::Ansi(5),  // Accent
    Paint::Default,  // SelectionBg
    Paint::Default,  // SelectionBgBlur
    Paint::Ansi(8),  // BarEmpty
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub mode: Mode,
    pub family: Family,
    colors: [Paint; N_ROLES],
}

impl Theme {
    /// The theme for a family and mode. `Ansi` and `Plain` ignore the family.
    pub fn preset(family: Family, mode: Mode) -> Theme {
        match mode {
            Mode::Ansi => Theme::ansi(),
            Mode::Plain => Theme::plain(),
            Mode::Light | Mode::Dark => {
                let light = mode == Mode::Light;
                let (fills, status) = if light { (FILLS_LIGHT, STATUS_LIGHT) } else { (FILLS_DARK, STATUS_DARK) };
                let c = match (family, light) {
                    (Family::Modus, true) => MODUS_LIGHT,
                    (Family::Modus, false) => MODUS_DARK,
                    (Family::Catppuccin, true) => CATPPUCCIN_LIGHT,
                    (Family::Catppuccin, false) => CATPPUCCIN_DARK,
                    (Family::RosePine, true) => ROSE_PINE_LIGHT,
                    (Family::RosePine, false) => ROSE_PINE_DARK,
                    (Family::TokyoNight, true) => TOKYO_NIGHT_LIGHT,
                    (Family::TokyoNight, false) => TOKYO_NIGHT_DARK,
                    (Family::Gruvbox, true) => GRUVBOX_LIGHT,
                    (Family::Gruvbox, false) => GRUVBOX_DARK,
                };
                // Indexed like [`Role::ALL`].
                let [good, warn, low, crit] = status;
                let rgb: [Rgb; N_ROLES] = [
                    fills[0], fills[1], fills[2], fills[3], fills[4], fills[5], fills[6], fills[7], good, warn, low,
                    crit, c.ink2, // Compaction
                    c.text, c.ink2, c.muted, c.border, c.accent, c.sel, c.sel_blur, c.muted, // BarEmpty
                ];
                Theme { mode, family, colors: rgb.map(Paint::Rgb) }
            }
        }
    }

    /// 16-color theme that follows the terminal's own palette.
    pub fn ansi() -> Theme {
        Theme { mode: Mode::Ansi, family: Family::Modus, colors: ANSI }
    }

    /// No colors at all (`NO_COLOR`, `TERM=dumb`, `--theme none`).
    pub fn plain() -> Theme {
        Theme { mode: Mode::Plain, family: Family::Modus, colors: [Paint::Default; N_ROLES] }
    }

    pub fn paint(&self, role: Role) -> Paint {
        self.colors[role.index()]
    }

    /// Foreground style for a role; empty when the paint is `Default`.
    pub fn fg(&self, role: Role) -> Style {
        match self.paint(role) {
            Paint::Default => Style::default(),
            p => Style::default().fg(p.color()),
        }
    }

    pub fn category(&self, id: &str) -> Paint {
        self.paint(Role::category(id))
    }

    /// Highlight for the selected row. Truecolor themes use a background
    /// tint; ANSI and plain themes use reverse video.
    pub fn selection(&self, focused: bool) -> Style {
        if self.is_truecolor() {
            let role = if focused { Role::SelectionBg } else { Role::SelectionBgBlur };
            Style::default().bg(self.paint(role).color()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    pub fn is_truecolor(&self) -> bool {
        matches!(self.mode, Mode::Light | Mode::Dark)
    }
}

static CURRENT: OnceLock<Theme> = OnceLock::new();

/// Install the process-wide theme. Only the first call has an effect.
pub fn init(theme: Theme) {
    let _ = CURRENT.set(theme);
}

/// The process-wide theme; Modus Light when [`init`] was never called.
pub fn current() -> &'static Theme {
    CURRENT.get_or_init(|| Theme::preset(Family::Modus, Mode::Light))
}

/// Resolve the theme from the `--theme` flag and the process environment.
pub fn resolve(flag: Option<&str>) -> Theme {
    resolve_with(flag, |k| std::env::var(k).ok())
}

/// Resolve with an injectable environment lookup.
///
/// Precedence: `NO_COLOR` (non-empty) or `TERM=dumb` always yields plain;
/// otherwise the flag, then `CTXVIZ_THEME`, then auto detection.
pub fn resolve_with(flag: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Theme {
    resolve_with_probe(flag, env, &|| None)
}

/// [`resolve_with`] plus a terminal background probe (OSC 11) consulted by
/// auto detection after `TERM_THEME` and before `COLORFGBG`. The probe runs
/// only when auto detection needs a mode and truecolor is available.
pub fn resolve_with_probe(
    flag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
    probe: &dyn Fn() -> Option<Mode>,
) -> Theme {
    if no_color(&env) {
        return Theme::plain();
    }
    let value = match flag {
        Some(f) => Some(f.to_owned()),
        None => env("CTXVIZ_THEME"),
    };
    let value = value.map(|v| v.trim().to_ascii_lowercase()).unwrap_or_default();
    if value.is_empty() || value == "auto" {
        return Theme::preset(Family::Modus, auto_mode(&env, probe));
    }
    match value.as_str() {
        "light" => return Theme::preset(Family::Modus, Mode::Light),
        "dark" => return Theme::preset(Family::Modus, Mode::Dark),
        "ansi" => return Theme::ansi(),
        "none" | "plain" => return Theme::plain(),
        _ => {}
    }
    if let Some(family) = Family::parse(&value) {
        return Theme::preset(family, auto_mode(&env, probe));
    }
    for (suffix, mode) in [("-light", Mode::Light), ("-dark", Mode::Dark)] {
        if let Some(family) = value.strip_suffix(suffix).and_then(Family::parse) {
            return Theme::preset(family, mode);
        }
    }
    static WARN: Once = Once::new();
    WARN.call_once(|| eprintln!("ctxviz: unknown theme \"{value}\", using auto"));
    Theme::preset(Family::Modus, auto_mode(&env, probe))
}

/// Auto detection: no truecolor means ANSI; then `TERM_THEME`, then the
/// probe (Light/Dark only), then the background field of `COLORFGBG`;
/// default Light.
fn auto_mode(env: &impl Fn(&str) -> Option<String>, probe: &dyn Fn() -> Option<Mode>) -> Mode {
    let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm != "truecolor" && colorterm != "24bit" {
        return Mode::Ansi;
    }
    match env("TERM_THEME").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("light") => return Mode::Light,
        Some("dark") => return Mode::Dark,
        _ => {}
    }
    if let Some(mode @ (Mode::Light | Mode::Dark)) = probe() {
        return mode;
    }
    if let Some(bg) =
        env("COLORFGBG").and_then(|v| v.rsplit(';').next().map(str::to_owned)).and_then(|b| b.trim().parse::<u8>().ok())
    {
        match bg {
            0..=6 | 8 => return Mode::Dark,
            7 | 9..=15 => return Mode::Light,
            _ => {}
        }
    }
    Mode::Light
}

// ---------------------------------------------------------------------------
// Kitty .conf themes (`--theme-file`).

impl Rgb {
    /// Parse `#rrggbb`, `rrggbb`, or Kitty's `#rgb` short form.
    pub(crate) fn parse_hex(s: &str) -> Option<Rgb> {
        let s = s.trim();
        let digits = s.strip_prefix('#').unwrap_or(s);
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        let nibble = |i: usize| u8::from_str_radix(&digits[i..i + 1], 16).ok().map(|n| n * 17);
        match digits.len() {
            6 => Some(Rgb(byte(0)?, byte(2)?, byte(4)?)),
            3 if s.starts_with('#') => Some(Rgb(nibble(0)?, nibble(1)?, nibble(2)?)),
            _ => None,
        }
    }
}

/// sRGB channel (0..=255) to linear light (0..=1).
fn linearize(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance (0 black ..= 1 white).
pub(crate) fn relative_luminance(c: Rgb) -> f64 {
    0.2126 * linearize(c.0) + 0.7152 * linearize(c.1) + 0.0722 * linearize(c.2)
}

/// WCAG contrast ratio, always >= 1 regardless of argument order.
pub(crate) fn wcag_contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// OKLab lightness (0 black ..= 1 white).
pub(crate) fn oklab_l(c: Rgb) -> f64 {
    let (r, g, b) = (linearize(c.0), linearize(c.1), linearize(c.2));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s
}

/// Blend `a` over `b`; `t` is the weight of `a`. Interpolates the 8-bit sRGB
/// channels directly, not linear light: sRGB lands closer to Modus's
/// hand-tuned chrome (Vivendi's border is exact, the rest within 18 levels;
/// linear light misses Modus borders by 50 to 70 levels). Neither reproduces
/// Modus exactly, hence [`PRESET_SNAP`].
pub(crate) fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let ch = |x: u8, y: u8| (t * f64::from(x) + (1.0 - t) * f64::from(y)).round().clamp(0.0, 255.0) as u8;
    Rgb(ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Exact (background, foreground) pairs of built-in presets. A conf matching
/// one returns that preset untouched, because preset chrome literals are
/// authoritative and hand-tuned. Modus is asymmetric: Operandi's border is
/// #9f9f9f and its selection #e3e3e3, while Vivendi uses #595959 and #303030.
/// So no single symmetric mix (sRGB or linear) reproduces both modes.
/// The other families' ink2/muted/accent are hand-picked, so they snap too.
/// Values are the `background`/`foreground` lines of the kitty-themes confs.
const PRESET_SNAP: [(Rgb, Rgb, Family, Mode); 10] = [
    (hex(0xffffff), hex(0x000000), Family::Modus, Mode::Light), // Modus_Operandi
    (hex(0x000000), hex(0xffffff), Family::Modus, Mode::Dark),  // Modus_Vivendi
    (hex(0xeff1f5), hex(0x4c4f69), Family::Catppuccin, Mode::Light), // Catppuccin-Latte
    (hex(0x1e1e2e), hex(0xcdd6f4), Family::Catppuccin, Mode::Dark), // Catppuccin-Mocha
    (hex(0xfaf4ed), hex(0x575279), Family::RosePine, Mode::Light), // rose-pine-dawn
    (hex(0x191724), hex(0xe0def4), Family::RosePine, Mode::Dark), // rose-pine
    (hex(0xe1e2e7), hex(0x3760bf), Family::TokyoNight, Mode::Light), // tokyo_night_day
    (hex(0x1a1b26), hex(0xc0caf5), Family::TokyoNight, Mode::Dark), // tokyo_night_night
    (hex(0xfbf1c7), hex(0x3c3836), Family::Gruvbox, Mode::Light), // gruvbox-light
    (hex(0x282828), hex(0xebdbb2), Family::Gruvbox, Mode::Dark), // gruvbox-dark
];

/// The colors ctxviz reads from a Kitty theme .conf.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KittyConf {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub color: [Option<Rgb>; 16],
}

/// Parse Kitty conf text: `key value` lines, `#` comments, blank lines.
/// Unknown keys (including `include`) are ignored; values for known keys
/// must be hex colors. Errors when `foreground` or `background` is missing.
pub(crate) fn parse_kitty_conf(text: &str) -> Result<KittyConf, String> {
    let mut conf = KittyConf::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let slot = match key {
            "foreground" => &mut conf.foreground,
            "background" => &mut conf.background,
            _ => match key.strip_prefix("color").and_then(|i| i.parse::<usize>().ok()) {
                Some(i) if i < 16 && key.len() <= "color15".len() => &mut conf.color[i],
                _ => continue,
            },
        };
        let value = value.trim();
        *slot = Some(Rgb::parse_hex(value).ok_or_else(|| format!("line {}: {key}: bad color \"{value}\"", n + 1))?);
    }
    if conf.foreground.is_none() {
        return Err("no foreground color".into());
    }
    if conf.background.is_none() {
        return Err("no background color".into());
    }
    Ok(conf)
}

impl Theme {
    /// Theme from a Kitty .conf: mode from the background's OKLab lightness,
    /// fills and status colors from the Modus table for that mode, chrome
    /// roles snapped to the conf's palette.
    ///
    /// A conf whose (background, foreground) appears in [`PRESET_SNAP`] gets
    /// that preset untouched; everything else goes through the heuristic.
    pub(crate) fn from_kitty_conf(conf: &KittyConf) -> Result<Theme, String> {
        let fg = conf.foreground.ok_or("no foreground color")?;
        let bg = conf.background.ok_or("no background color")?;
        if let Some(&(_, _, family, mode)) = PRESET_SNAP.iter().find(|s| (s.0, s.1) == (bg, fg)) {
            return Ok(Theme::preset(family, mode));
        }
        let mode = if oklab_l(bg) > 0.6 { Mode::Light } else { Mode::Dark };
        let mut theme = Theme::preset(Family::Modus, mode);

        let greys: Vec<(Rgb, f64)> =
            [8, 7, 15].iter().filter_map(|&i| conf.color[i]).map(|c| (c, wcag_contrast(c, bg))).collect();
        let ink2 =
            greys.iter().find(|(_, k)| (4.5..=10.0).contains(k)).map(|&(c, _)| c).unwrap_or_else(|| mix(fg, bg, 0.70));
        let muted = greys
            .iter()
            // Capped below 4.5 so muted stays distinct from ink-2.
            .filter(|(_, k)| (3.0..4.5).contains(k))
            .min_by(|a, b| (a.1 - 3.5).abs().total_cmp(&(b.1 - 3.5).abs()))
            .map(|&(c, _)| c)
            .unwrap_or_else(|| mix(fg, bg, 0.50));
        let accent = [5, 13].iter().filter_map(|&i| conf.color[i]).find(|&c| wcag_contrast(c, bg) >= 4.5).unwrap_or(fg);

        for (role, c) in [
            (Role::Text, fg),
            (Role::Ink2, ink2),
            (Role::Muted, muted),
            (Role::Border, mix(fg, bg, 0.35)),
            (Role::SelectionBg, mix(fg, bg, 0.12)),
            (Role::SelectionBgBlur, mix(fg, bg, 0.06)),
            (Role::Accent, accent),
            (Role::Compaction, ink2),
            (Role::BarEmpty, muted),
        ] {
            theme.colors[role.index()] = Paint::Rgb(c);
        }
        Ok(theme)
    }

    /// Read, parse and derive a theme from a Kitty .conf file (a regular
    /// file of at most [`THEME_FILE_MAX_BYTES`]).
    pub(crate) fn load_file(path: &std::path::Path) -> Result<Theme, String> {
        use std::io::Read;
        let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let meta = std::fs::metadata(path).map_err(|e| err(&e))?;
        if !meta.is_file() {
            return Err(err(&"not a regular file"));
        }
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(THEME_FILE_MAX_BYTES + 1).read_to_string(&mut text))
            .map_err(|e| err(&e))?;
        if text.len() as u64 > THEME_FILE_MAX_BYTES {
            return Err(err(&format!("larger than {THEME_FILE_MAX_BYTES} bytes")));
        }
        parse_kitty_conf(&text).and_then(|c| Theme::from_kitty_conf(&c)).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Largest `--theme-file` read.
pub const THEME_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// Like [`resolve`], but a `--theme-file` takes precedence over the flag.
pub fn resolve_file_or(flag: Option<&str>, file: Option<&std::path::Path>) -> Theme {
    resolve_file_or_with(flag, file, |k| std::env::var(k).ok())
}

/// [`resolve_file_or`] with a terminal background probe for auto detection
/// (see [`resolve_with_probe`]). Used by the TUI only.
pub fn resolve_file_or_probe(
    flag: Option<&str>,
    file: Option<&std::path::Path>,
    probe: &dyn Fn() -> Option<Mode>,
) -> Theme {
    resolve_file_or_with_probe(flag, file, |k| std::env::var(k).ok(), probe)
}

/// Precedence: `NO_COLOR`/`TERM=dumb` (plain), then `file` (a load error warns
/// once on stderr and falls through), then [`resolve_with`].
pub fn resolve_file_or_with(
    flag: Option<&str>,
    file: Option<&std::path::Path>,
    env: impl Fn(&str) -> Option<String>,
) -> Theme {
    resolve_file_or_with_probe(flag, file, env, &|| None)
}

/// [`resolve_file_or_with`] plus a background probe.
pub fn resolve_file_or_with_probe(
    flag: Option<&str>,
    file: Option<&std::path::Path>,
    env: impl Fn(&str) -> Option<String>,
    probe: &dyn Fn() -> Option<Mode>,
) -> Theme {
    if let Some(path) = file.filter(|_| !no_color(&env)) {
        match Theme::load_file(path) {
            Ok(t) => return t,
            Err(e) => eprintln!("ctxviz: --theme-file {e}; using --theme resolution"),
        }
    }
    resolve_with_probe(flag, env, probe)
}

fn no_color(env: &impl Fn(&str) -> Option<String>) -> bool {
    env("NO_COLOR").is_some_and(|v| !v.is_empty()) || env("TERM").as_deref() == Some("dumb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ink_contrasts_with_fill() {
        assert!(Rgb(255, 255, 255).luminance() > 0.99);
        assert!(Rgb(0, 0, 0).luminance() < 0.01);
        let black = Paint::Rgb(Rgb(0, 0, 0));
        let white = Paint::Rgb(Rgb(255, 255, 255));
        assert_eq!(Paint::Rgb(Rgb(0xed, 0xa1, 0x00)).ink_on(), black, "amber");
        assert_eq!(Paint::Rgb(Rgb(0x1b, 0xaf, 0x7a)).ink_on(), black, "light green");
        assert_eq!(Paint::Rgb(Rgb(0x62, 0x50, 0xd6)).ink_on(), white, "violet");
        assert_eq!(Paint::Rgb(Rgb(0x00, 0x63, 0x00)).ink_on(), white, "dark green");
        assert_eq!(Paint::Ansi(4).ink_on(), Paint::Ansi(15), "blue");
        assert_eq!(Paint::Ansi(1).ink_on(), Paint::Ansi(15), "red");
        assert_eq!(Paint::Ansi(3).ink_on(), Paint::Ansi(0), "yellow");
        assert_eq!(Paint::Ansi(14).ink_on(), Paint::Ansi(0), "bright cyan");
        assert_eq!(Paint::Default.ink_on(), Paint::Default);
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(Rgb(0x1b, 0xaf, 0x7a).hex(), "#1baf7a");
        assert_eq!(Rgb(0, 0, 0).hex(), "#000000");
    }

    #[test]
    fn sgr_escapes() {
        let cases = [
            (Paint::Rgb(Rgb(1, 22, 255)), "\x1b[38;2;1;22;255m", "\x1b[48;2;1;22;255m"),
            (Paint::Ansi(1), "\x1b[31m", "\x1b[41m"),
            (Paint::Ansi(9), "\x1b[91m", "\x1b[101m"),
            (Paint::Default, "", ""),
        ];
        for (p, fg, bg) in cases {
            let mut s = String::new();
            p.sgr_fg(&mut s);
            assert_eq!(s, fg, "{p:?} fg");
            let mut s = String::new();
            p.sgr_bg(&mut s);
            assert_eq!(s, bg, "{p:?} bg");
        }
    }

    #[test]
    fn paint_color_and_rgb_or() {
        assert_eq!(Paint::Default.color(), Color::Reset);
        assert_eq!(Paint::Ansi(8).color(), Color::DarkGray);
        assert_eq!(Paint::Ansi(15).color(), Color::White);
        assert_eq!(Paint::Rgb(Rgb(1, 2, 3)).color(), Color::Rgb(1, 2, 3));
        let fb = Rgb(9, 9, 9);
        assert_eq!(Paint::Ansi(1).rgb_or(fb), fb);
        assert_eq!(Paint::Rgb(Rgb(1, 2, 3)).rgb_or(fb), Rgb(1, 2, 3));
    }

    #[test]
    fn role_index_matches_all() {
        for (i, r) in Role::ALL.iter().enumerate() {
            assert_eq!(r.index(), i);
        }
    }

    #[test]
    fn role_coverage() {
        let ansi = Theme::ansi();
        let plain = Theme::plain();
        for r in Role::ALL {
            for t in truecolor_presets() {
                assert_ne!(t.paint(r), Paint::Default, "{:?} {:?} {r:?}", t.family, t.mode);
            }
            let blank = matches!(r, Role::Text | Role::SelectionBg | Role::SelectionBgBlur);
            assert_eq!(ansi.paint(r) == Paint::Default, blank, "ansi {r:?}");
            assert_eq!(plain.paint(r), Paint::Default, "plain {r:?}");
        }
    }

    const CATS: [Role; 8] =
        [Role::Cat1, Role::Cat2, Role::Cat3, Role::Cat4, Role::Cat5, Role::Cat6, Role::Cat7, Role::Cat8];

    fn assert_pairwise_distinct(t: &Theme, roles: &[Role]) {
        for (i, &a) in roles.iter().enumerate() {
            for &b in &roles[i + 1..] {
                assert_ne!(t.paint(a), t.paint(b), "{:?} {:?} {a:?} vs {b:?}", t.family, t.mode);
            }
        }
    }

    #[test]
    fn categories_distinct() {
        const SIGNALS: [Role; 6] = [Role::Good, Role::Warn, Role::Low, Role::Crit, Role::Compaction, Role::Accent];
        for t in truecolor_presets() {
            assert_pairwise_distinct(&t, &CATS);
            assert_ne!(t.paint(Role::Warn), t.paint(Role::Low), "{:?} {:?}", t.family, t.mode);
            for c in CATS {
                for s in SIGNALS {
                    assert_ne!(t.paint(c), t.paint(s), "{:?} {:?} {c:?} vs {s:?}", t.family, t.mode);
                }
            }
        }
        // 16 colors cannot keep Cat1..Cat8 disjoint from the status/accent signals.
        let ansi = Theme::ansi();
        assert_pairwise_distinct(&ansi, &CATS);
        assert_pairwise_distinct(&ansi, &[Role::Good, Role::Warn, Role::Low, Role::Crit]);
    }

    /// The 10 truecolor presets, every family in both modes.
    fn truecolor_presets() -> Vec<Theme> {
        FAMILIES.iter().flat_map(|&f| [Theme::preset(f, Mode::Light), Theme::preset(f, Mode::Dark)]).collect()
    }

    fn palette(t: &Theme) -> Vec<Rgb> {
        Role::ALL.iter().map(|&r| t.paint(r).rgb_or(Rgb(1, 2, 3))).collect()
    }

    #[test]
    fn hex_round_trip_all_presets() {
        for t in truecolor_presets() {
            for c in palette(&t) {
                assert_eq!(Rgb::parse_hex(&c.hex()), Some(c), "{:?} {:?} {}", t.family, t.mode, c.hex());
            }
        }
    }

    #[test]
    fn category_roles() {
        assert_eq!(Role::category("system_prompt"), Role::Cat1);
        assert_eq!(Role::category("conversation"), Role::Cat8);
        assert_eq!(Role::category("nope"), Role::Muted);
    }

    #[test]
    fn selection_styles() {
        let light = Theme::preset(Family::Modus, Mode::Light);
        assert_eq!(
            light.selection(true),
            Style::default().bg(Color::Rgb(0xe3, 0xe3, 0xe3)).add_modifier(Modifier::BOLD)
        );
        assert_eq!(Theme::ansi().selection(false), Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD));
        assert_eq!(Theme::plain().fg(Role::Crit), Style::default());
    }

    fn resolve_env(flag: Option<&str>, vars: &[(&str, &str)]) -> Theme {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        resolve_with(flag, |k| map.get(k).cloned())
    }

    /// Resolve with a fake env and a fake probe; returns the mode and how
    /// many times the probe ran.
    fn resolve_probe(flag: Option<&str>, vars: &[(&str, &str)], answer: Option<Mode>) -> (Mode, usize) {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let calls = std::cell::Cell::new(0);
        let probe = || {
            calls.set(calls.get() + 1);
            answer
        };
        let t = resolve_with_probe(flag, |k| map.get(k).cloned(), &probe);
        (t.mode, calls.get())
    }

    #[test]
    fn probe_precedence() {
        const TC: (&str, &str) = ("COLORTERM", "truecolor");
        // TERM_THEME beats the probe, which is never asked.
        assert_eq!(resolve_probe(None, &[TC, ("TERM_THEME", "light")], Some(Mode::Dark)), (Mode::Light, 0));
        // The probe beats COLORFGBG.
        assert_eq!(resolve_probe(None, &[TC, ("COLORFGBG", "0;15")], Some(Mode::Dark)), (Mode::Dark, 1));
        assert_eq!(resolve_probe(None, &[TC, ("COLORFGBG", "15;0")], Some(Mode::Light)), (Mode::Light, 1));
        // No answer falls through to COLORFGBG, then the Light default.
        assert_eq!(resolve_probe(None, &[TC, ("COLORFGBG", "15;0")], None), (Mode::Dark, 1));
        assert_eq!(resolve_probe(None, &[TC], None), (Mode::Light, 1));
        // A bare family still needs a mode, so it asks.
        assert_eq!(resolve_probe(Some("gruvbox"), &[TC], Some(Mode::Dark)), (Mode::Dark, 1));
    }

    #[test]
    fn probe_skipped_when_it_cannot_matter() {
        const TC: (&str, &str) = ("COLORTERM", "truecolor");
        let dark = Some(Mode::Dark);
        // Explicit flag or CTXVIZ_THEME.
        assert_eq!(resolve_probe(Some("light"), &[TC], dark), (Mode::Light, 0));
        assert_eq!(resolve_probe(Some("catppuccin-light"), &[TC], dark), (Mode::Light, 0));
        assert_eq!(resolve_probe(None, &[TC, ("CTXVIZ_THEME", "light")], dark), (Mode::Light, 0));
        assert_eq!(resolve_probe(None, &[TC, ("CTXVIZ_THEME", "ansi")], dark), (Mode::Ansi, 0));
        // No truecolor, NO_COLOR, TERM=dumb.
        assert_eq!(resolve_probe(None, &[], dark), (Mode::Ansi, 0));
        assert_eq!(resolve_probe(None, &[("COLORTERM", "256color")], dark), (Mode::Ansi, 0));
        assert_eq!(resolve_probe(None, &[TC, ("NO_COLOR", "1")], dark), (Mode::Plain, 0));
        assert_eq!(resolve_probe(None, &[TC, ("TERM", "dumb")], dark), (Mode::Plain, 0));
        // A loadable --theme-file wins without probing.
        let dir = crate::test_support::TempDir::new("theme-probe");
        let path = dir.join("probe.conf");
        std::fs::write(&path, "foreground #000000\nbackground #ffffff\n").unwrap();
        let calls = std::cell::Cell::new(0);
        let probe = || {
            calls.set(calls.get() + 1);
            dark
        };
        let t =
            resolve_file_or_with_probe(None, Some(&path), |k| (k == "COLORTERM").then(|| "truecolor".into()), &probe);
        assert_eq!((t.mode, calls.get()), (Mode::Light, 0));
    }

    #[test]
    fn resolve_precedence() {
        const TC: (&str, &str) = ("COLORTERM", "truecolor");
        let mode = |flag, vars: &[(&str, &str)]| resolve_env(flag, vars).mode;

        // NO_COLOR and TERM=dumb beat an explicit flag.
        assert_eq!(mode(Some("dark"), &[("NO_COLOR", "1"), TC]), Mode::Plain);
        assert_eq!(mode(Some("dark"), &[("TERM", "dumb"), TC]), Mode::Plain);
        // Empty NO_COLOR is ignored.
        assert_eq!(mode(Some("dark"), &[("NO_COLOR", "")]), Mode::Dark);
        // Flag beats CTXVIZ_THEME.
        assert_eq!(mode(Some("light"), &[("CTXVIZ_THEME", "dark"), TC]), Mode::Light);
        assert_eq!(mode(None, &[("CTXVIZ_THEME", "dark"), TC]), Mode::Dark);
        // CTXVIZ_THEME=ansi wins over a truecolor terminal.
        assert_eq!(mode(None, &[("CTXVIZ_THEME", "ansi"), TC]), Mode::Ansi);
        // Auto without truecolor is ANSI.
        assert_eq!(mode(None, &[]), Mode::Ansi);
        assert_eq!(mode(Some("auto"), &[("TERM_THEME", "dark")]), Mode::Ansi);
        // Auto with truecolor.
        assert_eq!(mode(None, &[TC]), Mode::Light);
        assert_eq!(mode(None, &[("COLORTERM", "24bit"), ("TERM_THEME", "dark")]), Mode::Dark);
        assert_eq!(mode(None, &[TC, ("TERM_THEME", "dark")]), Mode::Dark);
        assert_eq!(mode(None, &[TC, ("COLORFGBG", "15;0")]), Mode::Dark);
        assert_eq!(mode(None, &[TC, ("COLORFGBG", "0;15")]), Mode::Light);
        assert_eq!(mode(None, &[TC, ("COLORFGBG", "0;default;8")]), Mode::Dark);
        assert_eq!(mode(None, &[TC, ("TERM_THEME", "light"), ("COLORFGBG", "15;0")]), Mode::Light);
        // Family without a mode takes the auto mode.
        let t = resolve_env(Some("catppuccin"), &[TC, ("TERM_THEME", "dark")]);
        assert_eq!((t.family, t.mode), (Family::Catppuccin, Mode::Dark));
        assert_eq!(resolve_env(Some("gruvbox"), &[]).mode, Mode::Ansi);
        // Family with an explicit mode ignores detection.
        let t = resolve_env(Some("Catppuccin-Light"), &[TC, ("TERM_THEME", "dark")]);
        assert_eq!((t.family, t.mode), (Family::Catppuccin, Mode::Light));
        let t = resolve_env(None, &[("CTXVIZ_THEME", "rose-pine-dark")]);
        assert_eq!((t.family, t.mode), (Family::RosePine, Mode::Dark));
        let t = resolve_env(Some("tokyo-night"), &[TC]);
        assert_eq!((t.family, t.mode), (Family::TokyoNight, Mode::Light));
        // Unknown value falls back to auto.
        for vars in [&[][..], &[TC, ("TERM_THEME", "dark")][..]] {
            assert_eq!(resolve_env(Some("bogus"), vars), resolve_env(None, vars));
        }
        // Explicit dark without COLORTERM is still truecolor.
        let t = resolve_env(Some("dark"), &[]);
        assert_eq!((t.family, t.mode), (Family::Modus, Mode::Dark));
        assert!(t.is_truecolor());
        assert_eq!(mode(Some("none"), &[TC]), Mode::Plain);
        assert_eq!(mode(Some("PLAIN"), &[TC]), Mode::Plain);
        // NO_COLOR beats CTXVIZ_THEME.
        assert_eq!(mode(None, &[("NO_COLOR", "1"), ("CTXVIZ_THEME", "dark"), TC]), Mode::Plain);
        // Non-truecolor COLORTERM is ANSI only under auto.
        for ct in ["", "256color", "yes"] {
            let vars = [("COLORTERM", ct)];
            assert_eq!(mode(None, &vars), Mode::Ansi, "COLORTERM={ct:?}");
            assert_eq!(mode(Some("auto"), &vars), Mode::Ansi, "COLORTERM={ct:?}");
            assert_eq!(mode(Some("dark"), &vars), Mode::Dark, "COLORTERM={ct:?}");
            let t = resolve_env(Some("catppuccin-dark"), &vars);
            assert_eq!((t.family, t.mode), (Family::Catppuccin, Mode::Dark), "COLORTERM={ct:?}");
        }
        // An explicit mode in CTXVIZ_THEME beats TERM_THEME.
        let t = resolve_env(None, &[("CTXVIZ_THEME", "catppuccin-dark"), TC, ("TERM_THEME", "light")]);
        assert_eq!((t.family, t.mode), (Family::Catppuccin, Mode::Dark));
    }

    const FAMILIES: [Family; 5] =
        [Family::Modus, Family::Catppuccin, Family::RosePine, Family::TokyoNight, Family::Gruvbox];

    #[test]
    fn modus_chrome_matches_resolved_defaults() {
        let hexes = |t: &Theme, roles: &[Role]| -> Vec<String> {
            roles.iter().map(|&r| t.paint(r).rgb_or(Rgb(1, 2, 3)).hex()).collect()
        };
        let roles = [Role::Text, Role::Ink2, Role::Muted, Role::Border, Role::Accent, Role::SelectionBg];
        let light = Theme::preset(Family::Modus, Mode::Light);
        let dark = Theme::preset(Family::Modus, Mode::Dark);
        assert_eq!(hexes(&light, &roles), ["#000000", "#595959", "#8a8a8a", "#9f9f9f", "#721045", "#e3e3e3"]);
        assert_eq!(hexes(&dark, &roles), ["#ffffff", "#a6a6a6", "#8a8a8a", "#595959", "#feacd0", "#303030"]);
        for t in [&light, &dark] {
            assert_eq!(t.paint(Role::Compaction), t.paint(Role::Ink2));
            assert_eq!(t.paint(Role::BarEmpty), t.paint(Role::Muted));
        }
    }

    #[test]
    fn truecolor_presets_distinct_categories_and_status() {
        for family in FAMILIES {
            for mode in [Mode::Light, Mode::Dark] {
                let t = Theme::preset(family, mode);
                for r in Role::ALL {
                    assert!(matches!(t.paint(r), Paint::Rgb(_)), "{family:?} {mode:?} {r:?}");
                }
                for i in 0..8 {
                    for j in i + 1..8 {
                        assert_ne!(
                            t.paint(Role::ALL[i]),
                            t.paint(Role::ALL[j]),
                            "{family:?} {mode:?} Cat{} vs Cat{}",
                            i + 1,
                            j + 1
                        );
                    }
                }
                assert_ne!(t.paint(Role::Warn), t.paint(Role::Low), "{family:?} {mode:?}");
                assert_ne!(t.paint(Role::Ink2), t.paint(Role::Text), "{family:?} {mode:?} Ink2 vs Text");
                assert_ne!(t.paint(Role::Muted), t.paint(Role::Ink2), "{family:?} {mode:?} Muted vs Ink2");
            }
        }
    }

    #[test]
    fn current_defaults_to_modus_light() {
        assert_eq!(*current(), Theme::preset(Family::Modus, Mode::Light));
    }
}

#[cfg(test)]
mod conf_tests {
    use super::*;
    use std::collections::HashMap;

    const MODUS_OPERANDI: &str = "\
# Modus Operandi (excerpt of kitty-themes Modus_Operandi.conf)
foreground              #000000
background              #ffffff
color0 #000000
color8 #595959
color1 #a60000
color9 #972500
color2  #006800
color10 #00663f
color3  #6f5500
color11 #884900
color4  #0031a9
color12 #3548cf
color5  #721045
color13 #531ab6
color6  #005e8b
color14 #005f5f
color7  #a6a6a6
color15 #595959
";

    const MODUS_VIVENDI: &str = "\
foreground              #ffffff
background              #000000
color0 #000000
color8 #595959
color1 #ff5f59
color9 #ff7f9f
color2  #44bc44
color10 #00c06f
color3  #d0bc00
color11 #fec43f
color4  #2fafff
color12 #79a8ff
color5  #feacd0
color13 #b6a0ff
color6  #00d3d0
color14 #6ae4b9
color7  #a6a6a6
color15 #ffffff
";

    const CATPPUCCIN_MOCHA: &str = "\
## name: Catppuccin-Mocha
foreground              #CDD6F4
background              #1E1E2E
selection_foreground    #1E1E2E
color0 #45475A
color8 #585B70
color1 #F38BA8
color9 #F38BA8
color2  #A6E3A1
color10 #A6E3A1
color3  #F9E2AF
color11 #F9E2AF
color4  #89B4FA
color12 #89B4FA
color5  #F5C2E7
color13 #F5C2E7
color6  #94E2D5
color14 #94E2D5
color7  #BAC2DE
color15 #A6ADC8
";

    fn theme(text: &str) -> Theme {
        Theme::from_kitty_conf(&parse_kitty_conf(text).unwrap()).unwrap()
    }

    fn hex(t: &Theme, r: Role) -> String {
        match t.paint(r) {
            Paint::Rgb(c) => c.hex(),
            p => panic!("{r:?} is {p:?}"),
        }
    }

    #[test]
    fn parse_hex_forms() {
        assert_eq!(Rgb::parse_hex("#1baf7a"), Some(Rgb(0x1b, 0xaf, 0x7a)));
        assert_eq!(Rgb::parse_hex("1BAF7A"), Some(Rgb(0x1b, 0xaf, 0x7a)));
        assert_eq!(Rgb::parse_hex("#f0a"), Some(Rgb(0xff, 0x00, 0xaa)));
        for bad in ["", "#", "f0a", "#12345", "#1234567", "#gg0000", "red", "#+1+2+3"] {
            assert_eq!(Rgb::parse_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn color_math() {
        let (k, w) = (Rgb(0, 0, 0), Rgb(255, 255, 255));
        assert!((relative_luminance(w) - 1.0).abs() < 1e-9);
        assert_eq!(relative_luminance(k), 0.0);
        assert!((wcag_contrast(k, w) - 21.0).abs() < 1e-9);
        assert_eq!(wcag_contrast(w, k), wcag_contrast(k, w));
        assert!((wcag_contrast(Rgb(0x59, 0x59, 0x59), w) - 7.0).abs() < 0.01);
        assert!((oklab_l(w) - 1.0).abs() < 1e-6);
        assert!(oklab_l(k).abs() < 1e-9);
        assert!((oklab_l(Rgb(0x1e, 0x1e, 0x2e)) - 0.2435).abs() < 0.001);
        assert_eq!(mix(w, k, 0.35), Rgb(89, 89, 89));
        assert_eq!(mix(w, k, 1.0), w);
        assert_eq!(mix(w, k, 0.0), k);
        assert_eq!(mix(Rgb(0, 100, 200), Rgb(100, 0, 0), 0.5), Rgb(50, 50, 100));
    }

    #[test]
    fn parse_skips_comments_blanks_and_unknown_keys() {
        let c = parse_kitty_conf(
            "\n# comment\n   \ninclude other.conf\ncursor #ff0000\ncolor16 #123456\ncolor015 #123456\n\
             foreground #111111\n  background\t#eeeeee  \ncolor3 #333\n",
        )
        .unwrap();
        assert_eq!(c.foreground, Some(Rgb(0x11, 0x11, 0x11)));
        assert_eq!(c.background, Some(Rgb(0xee, 0xee, 0xee)));
        assert_eq!(c.color[3], Some(Rgb(0x33, 0x33, 0x33)));
        assert_eq!(c.color.iter().flatten().count(), 1);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_kitty_conf("background #ffffff\n").unwrap_err().contains("foreground"));
        assert!(parse_kitty_conf("foreground #000000\n").unwrap_err().contains("background"));
        assert!(parse_kitty_conf("foreground nope\nbackground #fff\n").unwrap_err().contains("line 1"));
        let no_bg = KittyConf { foreground: Some(Rgb(0, 0, 0)), ..KittyConf::default() };
        assert!(Theme::from_kitty_conf(&no_bg).is_err());
    }

    #[test]
    fn modus_confs_keep_modus_chrome() {
        let op = theme(MODUS_OPERANDI);
        assert_eq!(op, Theme::preset(Family::Modus, Mode::Light));
        let viv = theme(MODUS_VIVENDI);
        assert_eq!(viv, Theme::preset(Family::Modus, Mode::Dark));
    }

    /// The derivation itself on Modus's palettes, nudged off the exact-match
    /// snap (Operandi background #fefefe, Vivendi foreground #fefefe).
    #[test]
    fn modus_derivation() {
        let op = theme(&MODUS_OPERANDI.replace("#ffffff", "#fefefe"));
        assert_eq!(op.mode, Mode::Light);
        assert_eq!(hex(&op, Role::Text), "#000000");
        assert_eq!(hex(&op, Role::Ink2), "#595959");
        // No grey in [3, 4.5): #595959 is 7:1, #a6a6a6 2.4:1.
        assert_eq!(hex(&op, Role::Muted), "#7f7f7f");
        assert_eq!(hex(&op, Role::Border), "#a5a5a5");
        assert_eq!(hex(&op, Role::Accent), "#721045");
        assert_eq!(hex(&op, Role::SelectionBg), "#e0e0e0");
        assert_eq!(hex(&op, Role::SelectionBgBlur), "#efefef");
        assert_eq!(op.paint(Role::Compaction), op.paint(Role::Ink2));
        assert_eq!(op.paint(Role::BarEmpty), op.paint(Role::Muted));

        let viv = theme(&MODUS_VIVENDI.replace("foreground              #ffffff", "foreground #fefefe"));
        assert_eq!(viv.mode, Mode::Dark);
        assert_eq!(hex(&viv, Role::Ink2), "#a6a6a6");
        // color8 #595959 is 2.998:1 on black, just under the 3:1 floor.
        assert_eq!(hex(&viv, Role::Muted), "#7f7f7f");
        assert_eq!(hex(&viv, Role::Border), "#595959");
        assert_eq!(hex(&viv, Role::Accent), "#feacd0");
        assert_eq!(hex(&viv, Role::SelectionBg), "#1e1e1e");
    }

    #[test]
    fn catppuccin_mocha() {
        // The exact conf snaps to the preset; nudge the base's red channel
        // (#1e1e2e to #1d1e2e) to exercise the derivation.
        assert_eq!(theme(CATPPUCCIN_MOCHA), Theme::preset(Family::Catppuccin, Mode::Dark));
        let t = theme(&CATPPUCCIN_MOCHA.replace("background              #1E1E2E", "background #1D1E2E"));
        assert_eq!(t.mode, Mode::Dark);
        assert_eq!(hex(&t, Role::Text), "#cdd6f4");
        // color8 #585b70 is only 2.5:1 on the base: never ink-2.
        assert_ne!(hex(&t, Role::Ink2), "#585b70");
        assert_eq!(hex(&t, Role::Ink2), "#bac2de");
        // #a6adc8 (7.4:1) is ink-level, so muted falls back to the 50% mix.
        assert_eq!(hex(&t, Role::Muted), "#757a91");
        assert_ne!(t.paint(Role::Muted), t.paint(Role::Ink2));
        assert_eq!(hex(&t, Role::Border), "#5b5e73");
        assert_eq!(hex(&t, Role::Accent), "#f5c2e7");
        assert_eq!(hex(&t, Role::SelectionBg), "#323446");
        // Fills and status come from the Modus dark table.
        let base = Theme::preset(Family::Modus, Mode::Dark);
        for r in [Role::Cat1, Role::Cat8, Role::Good, Role::Crit] {
            assert_eq!(t.paint(r), base.paint(r), "{r:?}");
        }
    }

    #[test]
    fn preset_snap_round_trips() {
        for (i, a) in PRESET_SNAP.iter().enumerate() {
            for b in &PRESET_SNAP[i + 1..] {
                assert_ne!((a.0, a.1), (b.0, b.1), "{:?} {:?} vs {:?} {:?}", a.2, a.3, b.2, b.3);
            }
        }
        let mut covered = Vec::new();
        for &(bg, fg, family, mode) in &PRESET_SNAP {
            let preset = Theme::preset(family, mode);
            let conf = |bg: Rgb| format!("background {}\nforeground {}\n", bg.hex(), fg.hex());
            assert_eq!(theme(&conf(bg)), preset, "{family:?} {mode:?}");
            // One level off on red: no snap, so the rule runs and the chrome moves.
            let off = Rgb(if bg.0 == 0xff { bg.0 - 1 } else { bg.0 + 1 }, bg.1, bg.2);
            let t = theme(&conf(off));
            assert!(
                t.paint(Role::Border) != preset.paint(Role::Border)
                    || t.paint(Role::SelectionBg) != preset.paint(Role::SelectionBg),
                "{family:?} {mode:?} off-by-one still matches the preset"
            );
            covered.push((family, mode));
        }
        for family in [Family::Modus, Family::Catppuccin, Family::RosePine, Family::TokyoNight, Family::Gruvbox] {
            for mode in [Mode::Light, Mode::Dark] {
                assert!(covered.contains(&(family, mode)), "{family:?} {mode:?} has no snap row");
            }
        }
    }

    #[test]
    fn fallbacks_without_palette() {
        let t =
            theme("foreground #ffffff\nbackground #000000\ncolor5 #000001\n".replace("#000000", "#101010").as_str());
        assert_eq!(t.mode, Mode::Dark);
        assert_eq!(hex(&t, Role::Ink2), mix(Rgb(255, 255, 255), Rgb(16, 16, 16), 0.70).hex());
        assert_eq!(hex(&t, Role::Muted), mix(Rgb(255, 255, 255), Rgb(16, 16, 16), 0.50).hex());
        assert_eq!(hex(&t, Role::Accent), "#ffffff");
    }

    #[test]
    fn resolve_with_file() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            let map: HashMap<&str, &str> = vars.iter().copied().collect();
            move |k: &str| map.get(k).map(|v| v.to_string())
        };
        let dir = crate::test_support::TempDir::new("theme-conf");
        let path = dir.join("theme.conf");
        std::fs::write(&path, CATPPUCCIN_MOCHA).unwrap();
        let mocha = theme(CATPPUCCIN_MOCHA);
        assert_eq!(resolve_file_or_with(Some("light"), Some(&path), env(&[])), mocha);
        assert_eq!(resolve_file_or_with(None, Some(&path), env(&[("NO_COLOR", "1")])).mode, Mode::Plain);
        assert_eq!(resolve_file_or_with(None, Some(&path), env(&[("TERM", "dumb")])).mode, Mode::Plain);
        std::fs::remove_file(&path).unwrap();
        // Missing file falls back to normal resolution.
        assert_eq!(resolve_file_or_with(Some("dark"), Some(&path), env(&[])), Theme::preset(Family::Modus, Mode::Dark));
        assert_eq!(resolve_file_or_with(Some("ansi"), None, env(&[])), Theme::ansi());
    }

    #[test]
    fn resolve_file_precedence() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            let map: HashMap<&str, &str> = vars.iter().copied().collect();
            move |k: &str| map.get(k).map(|v| v.to_string())
        };
        let dir = crate::test_support::TempDir::new("theme-prec");
        let bad = dir.join("no-such/missing.conf");
        // A bad path falls back to resolve_with with the same flag and env.
        for vars in [
            &[][..],
            &[("COLORTERM", "truecolor"), ("TERM_THEME", "dark")][..],
            &[("CTXVIZ_THEME", "gruvbox-light")][..],
        ] {
            assert_eq!(resolve_file_or_with(None, Some(&bad), env(vars)), resolve_with(None, env(vars)), "{vars:?}");
        }
        assert_eq!(resolve_file_or_with(None, Some(&bad), env(&[("NO_COLOR", "1")])), Theme::plain());

        let path = dir.join("theme.conf");
        std::fs::write(&path, CATPPUCCIN_MOCHA).unwrap();
        // A real file is truecolor even without COLORTERM, and beats CTXVIZ_THEME.
        let t = resolve_file_or_with(None, Some(&path), env(&[("CTXVIZ_THEME", "ansi")]));
        assert_ne!(t.mode, Mode::Plain);
        assert!(t.is_truecolor());
        assert_eq!(t, theme(CATPPUCCIN_MOCHA));
        // NO_COLOR beats the file and the flag.
        assert_eq!(resolve_file_or_with(Some("dark"), Some(&path), env(&[("NO_COLOR", "1")])), Theme::plain());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn theme_file_over_cap_is_rejected() {
        let dir = crate::test_support::TempDir::new("theme-big");
        let path = dir.join("big.conf");
        let mut body = String::from("foreground #000000\nbackground #ffffff\n");
        body.push_str(&"#".repeat(THEME_FILE_MAX_BYTES as usize));
        std::fs::write(&path, &body).unwrap();
        let err = Theme::load_file(&path).unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        assert!(Theme::load_file(&dir).unwrap_err().contains("not a regular file"));
    }
}
