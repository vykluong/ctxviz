//! Category table (port of `context_usage.py` CATEGORIES and
//! SECTION_TAGS; truecolor paints live in `crate::theme`). Order matches agent.v1.PromptTokenBreakdownCategory.

pub use crate::theme::Rgb;

#[derive(Debug, Clone, Copy)]
pub struct CategoryDef {
    pub id: &'static str,
    pub label: &'static str,
    /// xterm 256-color index (same values as the Python reference).
    pub color256: u8,
}

pub const CATEGORIES: [CategoryDef; 8] = [
    CategoryDef { id: "system_prompt", label: "System prompt", color256: 244 },
    CategoryDef { id: "tools", label: "Tool definitions", color256: 99 },
    CategoryDef { id: "rules", label: "Rules", color256: 29 },
    CategoryDef { id: "skills", label: "Skills", color256: 136 },
    CategoryDef { id: "mcp", label: "MCP & dynamic tools", color256: 90 },
    CategoryDef { id: "subagents", label: "Subagent definitions", color256: 33 },
    CategoryDef { id: "summarized_conversation", label: "Summarized conversation", color256: 37 },
    CategoryDef { id: "conversation", label: "Conversation", color256: 160 },
];

/// Color for categories not in the table (Python default 250).
pub const UNKNOWN_COLOR256: u8 = 250;

/// User-info sections attributed to a category instead of the conversation
/// (desktop table M0h): (xml tag, category id).
pub const SECTION_TAGS: [(&str, &str); 5] = [
    ("rules", "rules"),
    ("agent_skills", "skills"),
    ("available_subagent_models", "subagents"),
    ("available_subagent_types", "subagents"),
    ("dynamic_tool_catalog", "mcp"),
];

pub fn category(id: &str) -> Option<&'static CategoryDef> {
    CATEGORIES.iter().find(|c| c.id == id)
}

/// Position in the fixed category order; unknown ids sort last.
pub fn order(id: &str) -> usize {
    CATEGORIES.iter().position(|c| c.id == id).unwrap_or(CATEGORIES.len())
}

pub fn label(id: &str) -> Option<&'static str> {
    category(id).map(|c| c.label)
}

pub fn color256(id: &str) -> u8 {
    category(id).map_or(UNKNOWN_COLOR256, |c| c.color256)
}

/// Compact label for tight layouts (legends, statusline). Unknown ids fall
/// back to the id itself, or to `"?"` when it carries anything a terminal
/// could interpret (ids are sanitized at decode; this is the second line).
pub fn short_label(id: &str) -> &str {
    match id {
        "system_prompt" => "system",
        "tools" => "tools",
        "rules" => "rules",
        "skills" => "skills",
        "mcp" => "mcp",
        "subagents" => "subagents",
        "summarized_conversation" => "summary",
        "conversation" => "chat",
        other if crate::text::is_clean(other) && !other.contains(['\n', '\t']) => other,
        _ => "?",
    }
}

/// Desktop token-estimate.ts: Math.round(chars / 4), on code points.
/// Python `round` is half-to-even; `chars / 4` ties are x.5 exactly.
pub fn estimate_chars(chars: usize) -> u64 {
    let q = chars / 4;
    let r = chars % 4;
    (q + usize::from(r > 2 || (r == 2 && q % 2 == 1))) as u64
}

pub fn estimate(text: &str) -> u64 {
    estimate_chars(text.chars().count())
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_label_never_returns_control_chars() {
        assert_eq!(super::short_label("conversation"), "chat");
        assert_eq!(super::short_label("custom_thing"), "custom_thing");
        for bad in ["\x1b]52;c;QUJD\x07", "\u{9b}31m", "a\nb", "x\u{7f}"] {
            assert_eq!(super::short_label(bad), "?", "{bad:?}");
        }
    }

    #[test]
    fn estimate_matches_python_round() {
        // Python: [round(n / 4) for n in range(12)]
        let want = [0, 0, 0, 1, 1, 1, 2, 2, 2, 2, 2, 3];
        for (n, w) in want.iter().enumerate() {
            assert_eq!(super::estimate_chars(n), *w, "n={n}");
        }
    }
}
