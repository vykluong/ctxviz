//! Context Explorer tree (port of context_usage.py `part_text`,
//! `message_parts`, `split_section`, `Tree.rollup`, `build_tree` and the
//! `render` ordering rule).
//!
//! Lengths are code points (Python `len`), never bytes.

use crate::model::breakdown::BreakdownEntry;
use crate::model::categories::{self, SECTION_TAGS};
use crate::store::Store;
use crate::text::{sanitize_single_line, sanitize_terminal_text};
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

/// agent.v1.PromptContextNode as the Python emits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    /// "category" | "group" | "item" | "message"
    pub kind: String,
    pub label: String,
    pub category_id: String,
    pub estimated_tokens: u64,
    pub character_count: u64,
    /// Local estimate before the server total replaced it (top-level only).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub local_estimate: Option<u64>,
    /// Full text for the TUI detail pane; never serialized.
    #[serde(skip)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    pub schema_version: u32,
    pub nodes: Vec<Node>,
}

impl Default for Tree {
    fn default() -> Self {
        Tree { schema_version: 1, nodes: Vec::new() }
    }
}

fn chars(s: &str) -> u64 {
    s.chars().count() as u64
}

/// Store text is attacker-controlled: labels become single-line and free of
/// control characters before they enter the tree, so every renderer (TUI,
/// snapshot text, JSON) inherits the filtering. Reuses `s` when it is clean.
fn clean_label(s: String) -> String {
    match sanitize_single_line(&s) {
        Cow::Borrowed(_) => s,
        Cow::Owned(o) => o,
    }
}

/// Multi-line body text for the detail pane: control characters replaced,
/// newlines and tabs kept.
fn clean_text(s: &str) -> String {
    sanitize_terminal_text(s).into_owned()
}

impl Tree {
    /// Python `Tree.add`: tokens/chars default to the estimate of `text`.
    /// Sizes come from the raw text (Python parity); the stored label and
    /// text are sanitized (one replacement char per removed char, so
    /// character counts still agree).
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        id: String,
        parent: Option<&str>,
        kind: &str,
        label: String,
        category: &str,
        text: &str,
        sizes: Option<(u64, u64)>,
    ) -> String {
        let (tokens, count) = sizes.unwrap_or_else(|| (categories::estimate(text), chars(text)));
        self.nodes.push(Node {
            id: id.clone(),
            parent_id: parent.map(str::to_string),
            kind: kind.to_string(),
            label: clean_label(label),
            category_id: category.to_string(),
            estimated_tokens: tokens,
            character_count: count,
            local_estimate: None,
            text: (!text.is_empty()).then(|| clean_text(text)),
        });
        id
    }

    /// Indices of each node's children, in insertion order. Key `None` holds
    /// the top-level nodes.
    pub fn children(&self) -> HashMap<Option<&str>, Vec<usize>> {
        let mut kids: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            kids.entry(n.parent_id.as_deref()).or_default().push(i);
        }
        kids
    }

    /// Fill group nodes with the sum of their children.
    fn rollup(&mut self) {
        let kids: HashMap<Option<String>, Vec<usize>> = {
            let mut m: HashMap<Option<String>, Vec<usize>> = HashMap::new();
            for (i, n) in self.nodes.iter().enumerate() {
                m.entry(n.parent_id.clone()).or_default().push(i);
            }
            m
        };
        fn total(nodes: &mut [Node], kids: &HashMap<Option<String>, Vec<usize>>, i: usize) -> u64 {
            if let Some(ch) = kids.get(&Some(nodes[i].id.clone())) {
                let tokens = ch.iter().map(|&c| total(nodes, kids, c)).sum();
                let count = ch.iter().map(|&c| nodes[c].character_count).sum();
                nodes[i].estimated_tokens = tokens;
                nodes[i].character_count = count;
            }
            nodes[i].estimated_tokens
        }
        for &i in kids.get(&None).map(Vec::as_slice).unwrap_or_default() {
            total(&mut self.nodes, &kids, i);
        }
    }

    /// Display order of `children` (Python `render.walk`): conversation stays
    /// chronological, everything else (and the top level) sorts by size,
    /// largest first, stable.
    pub fn ordered(&self, parent: Option<&str>, children: &[usize]) -> Vec<usize> {
        let mut ch = children.to_vec();
        let chronological =
            parent.is_some() && ch.first().is_some_and(|&i| self.nodes[i].category_id == "conversation");
        if !chronological {
            ch.sort_by_key(|&i| std::cmp::Reverse(self.nodes[i].estimated_tokens));
        }
        ch
    }
}

// --------------------------------------------------------------------------
// Python json.dumps (default ensure_ascii, ", " / ": " separators)

fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // Shortest round-trip digits, then Python repr's layout rules.
    let sci = format!("{f:e}");
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mant) = mant.strip_prefix('-').map_or(("", mant), |m| ("-", m));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        let point = exp + 1;
        let s = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{s}")
    } else {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn py_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
    }
    out.push('"');
}

fn py_dumps_into(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => out.push_str(&i.to_string()),
            (_, Some(u), _) => out.push_str(&u.to_string()),
            (_, _, Some(f)) => out.push_str(&py_float(f)),
            _ => out.push_str(&n.to_string()),
        },
        Value::String(s) => py_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_dumps_into(x, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_str(k, out);
                out.push_str(": ");
                py_dumps_into(x, out);
            }
            out.push('}');
        }
    }
}

/// Python `json.dumps(v)`. Object key order follows serde_json's map (sorted
/// unless `preserve_order` is enabled); lengths are unaffected.
pub fn py_dumps(v: &Value) -> String {
    let mut s = String::new();
    py_dumps_into(v, &mut s);
    s
}

// --------------------------------------------------------------------------
// Messages

fn part_text(part: &Value) -> String {
    let get = |k: &str| part.get(k);
    match get("type").and_then(Value::as_str) {
        Some("text" | "reasoning") => get("text").and_then(Value::as_str).unwrap_or("").to_string(),
        Some("tool-call") => {
            py_dumps(get("input").or_else(|| get("args")).unwrap_or(&Value::Object(Default::default())))
        }
        Some("tool-result") => {
            let mut out = get("output").or_else(|| get("result")).cloned().unwrap_or(Value::String(String::new()));
            if let Some(v) = out.as_object().and_then(|o| o.get("value")) {
                out = v.clone();
            }
            match out {
                Value::String(s) => s,
                v => py_dumps(&v),
            }
        }
        _ => py_dumps(part),
    }
}

/// (label, text) for each displayable piece of a prompt message.
pub fn message_parts(msg: &Value) -> Vec<(String, String)> {
    let role = msg.get("role").and_then(Value::as_str);
    let content = match msg.get("content") {
        Some(Value::String(s)) => vec![serde_json::json!({"type": "text", "text": s})],
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    content
        .iter()
        .map(|part| {
            let kind = part.get("type").and_then(Value::as_str);
            let text = part_text(part);
            let tool = || part.get("toolName").and_then(Value::as_str).unwrap_or("?").to_string();
            let label = if role == Some("user") {
                if text.contains("<user_query>") { "User Message" } else { "Context Message" }.to_string()
            } else {
                match kind {
                    Some("reasoning") => "Thinking".to_string(),
                    Some("text") => "Agent Message".to_string(),
                    Some("tool-call") => format!("Tool: {}", tool()),
                    Some("tool-result") => format!("Tool Result: {}", tool()),
                    Some(k) if !k.is_empty() => k.to_string(),
                    _ => role.unwrap_or("").to_string(),
                }
            };
            (label, text)
        })
        .collect()
}

// --------------------------------------------------------------------------
// Sections

/// Python `posixpath.basename`.
fn basename(p: &str) -> &str {
    p.rfind('/').map_or(p, |i| &p[i + 1..])
}

/// Python `posixpath.dirname`.
fn dirname(p: &str) -> &str {
    let head = p.rfind('/').map_or("", |i| &p[..i + 1]);
    let trimmed = head.trim_end_matches('/');
    if trimmed.is_empty() {
        head
    } else {
        trimmed
    }
}

/// Python `str.isspace` for one char (adds the \x1c-\x1f separators).
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// First line of `s` per Python `str.splitlines`.
fn first_line(s: &str) -> &str {
    let line_break = |c: char| {
        matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
    };
    s.find(line_break).map_or(s, |i| &s[..i])
}

static RULE_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<(\w*rule)\s[^>]*?name="([^"]+)"[^>]*>"#).unwrap());
static SKILL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<agent_skill\b([^>]*)>(.*?)</agent_skill>").unwrap());
static ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:name|fullPath|path|description)="([^"]+)""#).unwrap());
static NAMESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<namespace\s[^>]*?name="([^"]+)""#).unwrap());
/// `<tag>(.*?)</tag>` for each SECTION_TAGS entry, in table order.
static SECTIONS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    SECTION_TAGS.iter().map(|(tag, _)| Regex::new(&format!(r"(?s)<{tag}>(.*?)</{tag}>")).unwrap()).collect()
});

/// Any `</...rule>` close tag; collected once per body by [`rule_matches`].
static RULE_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"</(\w*rule)>").unwrap());

/// Python `re.finditer(r"<(\w*rule)\s[^>]*?name=\"([^\"]+)\"[^>]*>(.*?)</\1>", body, re.S)`.
/// The `regex` crate has no backreferences: match the open tag, then look up
/// its close tag; when there is none, retry one character later like the
/// backtracking engine would. Close tags are indexed in a single pass up front
/// (tag name to sorted offsets), so each lookup is a binary search instead of
/// a rescan of the rest of the body, and unclosed tags cost O(log N), not O(L).
fn rule_matches(body: &str) -> Vec<(String, &str)> {
    let mut closes: HashMap<&str, Vec<usize>> = HashMap::new();
    for c in RULE_CLOSE.captures_iter(body) {
        closes.entry(c.get(1).unwrap().as_str()).or_default().push(c.get(0).unwrap().start());
    }
    let mut items = Vec::new();
    let mut pos = 0;
    while let Some(c) = RULE_OPEN.captures_at(body, pos) {
        let open = c.get(0).unwrap();
        let tag = &c[1];
        let close = closes.get(tag).and_then(|starts| {
            let i = starts.partition_point(|&s| s < open.end());
            starts.get(i).copied()
        });
        match close {
            Some(start) => {
                // `</` + tag + `>`.
                let end = start + tag.len() + 3;
                items.push((basename(&c[2]).to_string(), &body[open.start()..end]));
                pos = end;
            }
            // `<` is one byte, so start + 1 is a char boundary.
            None => pos = open.start() + 1,
        }
    }
    items
}

/// Split a section body into named children (desktop FI_/UI_/$I_ grouping).
pub fn split_section<'a>(category: &str, body: &'a str) -> Vec<(String, &'a str)> {
    match category {
        "rules" => rule_matches(body),
        "skills" => SKILL
            .captures_iter(body)
            .map(|m| {
                let label: String = match ATTR.captures(&m[1]) {
                    Some(a) => a[1].to_string(),
                    None => first_line(m[2].trim_matches(py_space)).chars().take(60).collect(),
                };
                let label = if label.ends_with("SKILL.md") { basename(dirname(&label)).to_string() } else { label };
                (label, m.get(0).unwrap().as_str())
            })
            .collect(),
        "mcp" => {
            // Namespaces are not closed elements: each runs until the next one.
            let starts: Vec<_> = NAMESPACE.captures_iter(body).collect();
            let last = body.find("</dynamic_tool_namespaces>").unwrap_or(body.len());
            starts
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let start = m.get(0).unwrap().start();
                    let end = starts.get(i + 1).map_or(last, |n| n.get(0).unwrap().start());
                    (m[1].to_string(), body.get(start..end.max(start)).unwrap_or(""))
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Python `str.title` for the ASCII section tags ("agent skills" -> "Agent Skills").
fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        if prev_cased {
            out.extend(c.to_lowercase())
        } else {
            out.extend(c.to_uppercase())
        }
        prev_cased = c.is_alphabetic();
    }
    out
}

// --------------------------------------------------------------------------
// Tree

/// Build the explorer tree from the root's message ids and server breakdown.
pub fn build_tree(store: &Store, message_ids: &[String], breakdown: &[BreakdownEntry]) -> Result<Tree> {
    let mut tree = Tree::default();
    let server: HashMap<&str, u64> = breakdown.iter().map(|c| (c.id.as_str(), c.estimated_tokens)).collect();
    for c in &categories::CATEGORIES {
        tree.add(format!("category:{}", c.id), None, "category", c.label.to_string(), c.id, "", Some((0, 0)));
    }

    let mut sections = [0u64; SECTION_TAGS.len()];
    let (mut turn, mut turn_count) = ("turn:0".to_string(), 0u64);
    tree.add(
        turn.clone(),
        Some("category:conversation"),
        "group",
        "Before first user message".into(),
        "conversation",
        "",
        Some((0, 0)),
    );

    for (i, bid) in message_ids.iter().enumerate() {
        let Some(raw) = store.blob(bid)? else { continue };
        // Python would raise on a non-JSON message blob; skip it instead.
        let Ok(msg) = serde_json::from_slice::<Value>(&raw) else { continue };
        let parts = message_parts(&msg);

        if msg.get("role").and_then(Value::as_str) == Some("system") {
            let text: String = parts.into_iter().map(|(_, t)| t).collect();
            tree.add(
                format!("msg:{i}"),
                Some("category:system_prompt"),
                "message",
                "System prompt".into(),
                "system_prompt",
                &text,
                None,
            );
            continue;
        }

        for (j, (label, mut text)) in parts.into_iter().enumerate() {
            // Carve out rules/skills/subagents/mcp sections (desktop xI_).
            for (t, (tag, cid)) in SECTION_TAGS.iter().enumerate() {
                let re = &SECTIONS[t];
                if !text.contains(&format!("<{tag}>")) {
                    continue;
                }
                for m in re.captures_iter(&text) {
                    sections[t] += 1;
                    let group_id = tree.add(
                        format!("section:{tag}:{i}:{}", sections[t]),
                        Some(&format!("category:{cid}")),
                        "group",
                        title(&tag.replace('_', " ")),
                        cid,
                        "",
                        Some((0, 0)),
                    );
                    let children = split_section(cid, m.get(1).unwrap().as_str());
                    for (k, (child_label, child_text)) in children.iter().enumerate() {
                        tree.add(
                            format!("{group_id}:{k}"),
                            Some(&group_id),
                            "item",
                            child_label.clone(),
                            cid,
                            child_text,
                            None,
                        );
                    }
                    let whole = m.get(0).unwrap().as_str();
                    let rest = chars(whole).saturating_sub(children.iter().map(|(_, t)| chars(t)).sum());
                    let rest_label = if children.is_empty() { tag.to_string() } else { "(section text)".into() };
                    tree.add(
                        format!("{group_id}:rest"),
                        Some(&group_id),
                        "item",
                        rest_label,
                        cid,
                        "",
                        Some((categories::estimate_chars(rest as usize), rest)),
                    );
                    if children.is_empty() {
                        tree.nodes.last_mut().unwrap().text = Some(clean_text(whole));
                    }
                }
                text = re.replace_all(&text, "").into_owned();
            }

            if label == "User Message" {
                turn_count += 1;
                turn = format!("turn:{turn_count}");
                tree.add(
                    turn.clone(),
                    Some("category:conversation"),
                    "group",
                    format!("Turn {turn_count}"),
                    "conversation",
                    "",
                    Some((0, 0)),
                );
            }
            if !text.trim_matches(py_space).is_empty() {
                tree.add(format!("msg:{i}:{j}"), Some(&turn), "message", label, "conversation", &text, None);
            }
        }
    }

    tree.rollup();
    // Tool definitions are not in the local store; carry the server total.
    for n in &mut tree.nodes {
        if n.id == "category:tools" {
            n.estimated_tokens = server.get("tools").copied().unwrap_or(0);
            n.label += " (server-side, no per-tool detail locally)";
        } else if n.parent_id.is_none() {
            if let Some(&s) = server.get(n.category_id.as_str()) {
                n.local_estimate = Some(n.estimated_tokens);
                n.estimated_tokens = s;
            }
        }
    }
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rules_with_nested_and_unclosed_tags() {
        let body = concat!(
            "<always_applied_workspace_rule name=\"/a/b/one.mdc\">x <rule name=\"inner\">y</rule> z</always_applied_workspace_rule>",
            "<rule name=\"open\">never closed ",
            "<agent_requestable_rule path=\"p\" name=\"two\">two</agent_requestable_rule>",
        );
        let items = split_section("rules", body);
        let labels: Vec<_> = items.iter().map(|(l, _)| l.as_str()).collect();
        // The outer rule swallows its nested rule; the unclosed one is skipped.
        assert_eq!(labels, ["one.mdc", "two"]);
        assert!(items[0].1.ends_with("z</always_applied_workspace_rule>"));
        assert_eq!(items[1].1, "<agent_requestable_rule path=\"p\" name=\"two\">two</agent_requestable_rule>");
    }

    #[test]
    fn rule_backreference_matches_own_close_tag() {
        // `</rule>` must not close `<xrule ...>`.
        let items = split_section("rules", "<xrule name=\"a\">1</rule>2</xrule>");
        assert_eq!(items, [("a".to_string(), "<xrule name=\"a\">1</rule>2</xrule>")]);
    }

    #[test]
    fn many_unclosed_rule_tags_are_skipped_in_linear_time() {
        // Each unclosed open tag used to rescan the rest of the body.
        let mut body = "<xrule name=\"u\">pad ".repeat(20_000);
        body.push_str("<rule name=\"/r/ok.mdc\">k</rule><xrule name=\"tail\">never closed");
        let items = split_section("rules", &body);
        assert_eq!(items, [("ok.mdc".to_string(), "<rule name=\"/r/ok.mdc\">k</rule>")]);
    }

    #[test]
    fn unclosed_rule_retries_inside_its_own_open_tag() {
        // The outer open tag has no `</arule>`, so the match restarts one
        // character later and finds the `<rule ...>` nested in its attribute.
        let items = split_section("rules", "<arule x=\"<rule name=\"in\">\" name=\"out\">b</rule>");
        assert_eq!(items, [("in".to_string(), "<rule name=\"in\">\" name=\"out\">b</rule>")]);
    }

    #[test]
    fn skills_and_mcp() {
        let body = "<agent_skill fullPath=\"/s/pdf/SKILL.md\">pdf</agent_skill>\n<agent_skill>\n  Ünïcode first line\nsecond</agent_skill>";
        let items = split_section("skills", body);
        assert_eq!(items[0].0, "pdf");
        assert_eq!(items[1].0, "Ünïcode first line");

        let body = "pre<namespace name=\"a\">A</namespace><namespace x name=\"b\">B</dynamic_tool_namespaces>tail";
        let items = split_section("mcp", body);
        assert_eq!(
            items,
            [("a".into(), "<namespace name=\"a\">A</namespace>"), ("b".into(), "<namespace x name=\"b\">B")]
        );
        assert!(split_section("conversation", body).is_empty());
    }

    #[test]
    fn message_parts_labels_and_unicode() {
        let user = json!({"role": "user", "content": "<user_query>héllo 🌍</user_query>"});
        let parts = message_parts(&user);
        assert_eq!(parts[0].0, "User Message");
        assert_eq!(chars(&parts[0].1), 32);
        assert_eq!(categories::estimate(&parts[0].1), 8);

        let asst = json!({"role": "assistant", "content": [
            {"type": "reasoning", "text": "hm"},
            {"type": "text", "text": "ok"},
            {"type": "tool-call", "toolName": "Read", "input": {"path": "é"}},
            {"type": "tool-result", "toolName": "Read", "output": {"type": "json", "value": [1, 2.5, 1e16]}},
            {"type": "image"},
        ]});
        let parts = message_parts(&asst);
        let labels: Vec<_> = parts.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Thinking", "Agent Message", "Tool: Read", "Tool Result: Read", "image"]);
        // Python json.dumps: ensure_ascii and spaced separators.
        assert_eq!(parts[2].1, "{\"path\": \"\\u00e9\"}");
        assert_eq!(parts[3].1, "[1, 2.5, 1e+16]");
        assert_eq!(parts[4].1, r#"{"type": "image"}"#);

        let ctx = json!({"role": "user", "content": [{"type": "text", "text": "<rules>r</rules>"}]});
        assert_eq!(message_parts(&ctx)[0].0, "Context Message");
    }

    #[test]
    fn python_float_repr() {
        for (f, want) in [
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e15, "1000000000000000.0"),
            (-1.5e-7, "-1.5e-07"),
            (123.456, "123.456"),
        ] {
            assert_eq!(py_float(f), want);
        }
    }

    /// A unique temp dir (pid, nanos, sequence, name), removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(prefix: &str, name: &str) -> TempDir {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{seq}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A read-only store in its own temp dir; the dir goes away with it.
    struct TempStore {
        store: Option<Store>,
        _dir: TempDir,
    }

    impl std::ops::Deref for TempStore {
        type Target = Store;
        fn deref(&self) -> &Store {
            self.store.as_ref().unwrap()
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            // Close the connection before the directory is removed.
            self.store.take();
        }
    }

    /// A throwaway store.db holding `blobs` (id -> JSON), opened read-only,
    /// in a unique dir removed on drop.
    fn temp_store(name: &str, blobs: &[(&str, Value)]) -> TempStore {
        let dir = TempDir::new("ctxviz-tree", name);
        let path = dir.0.join("store.db");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "create table blobs(id text primary key, data blob); create table meta(key text primary key, value text);",
        )
        .unwrap();
        for (id, v) in blobs {
            c.execute(
                "insert into blobs(id, data) values (?, ?)",
                rusqlite::params![id, serde_json::to_vec(v).unwrap()],
            )
            .unwrap();
        }
        drop(c);
        TempStore { store: Some(Store::open(&path).unwrap()), _dir: dir }
    }

    const OSC52: &str = "\x1b]52;c;QUJD\x07";
    const DCS: &str = "\x1bP@kitty-cmd{\"cmd\":\"launch\"}\x1b\\";
    const CSI8: &str = "\u{9b}31m";

    #[test]
    fn build_tree_sanitizes_labels_and_text() {
        let user = format!(
            "<user_query>hi {OSC52}</user_query><rules><rule name=\"/r/{CSI8}.mdc\">rule {DCS}</rule></rules>\
             <agent_skills><agent_skill name=\"{OSC52}\">s</agent_skill></agent_skills>\
             <dynamic_tool_catalog><namespace name=\"{CSI8}\">ns {DCS}</namespace></dynamic_tool_catalog>"
        );
        let blobs = [
            ("u", json!({"role": "user", "content": user})),
            (
                "a",
                json!({"role": "assistant", "content": [
                    {"type": "text", "text": DCS},
                    {"type": "tool-call", "toolName": OSC52, "input": {}},
                    {"type": "tool-result", "toolName": format!("{CSI8}\nsecond line"), "output": DCS},
                    {"type": format!("{OSC52}kind")},
                ]}),
            ),
        ];
        let store = temp_store("labels", &blobs);
        let tree = build_tree(&store, &["u".into(), "a".into()], &[]).unwrap();
        let mut replaced = 0;
        for n in &tree.nodes {
            assert!(crate::text::is_clean(&n.label) && !n.label.contains('\n'), "label {:?}", n.label);
            if let Some(t) = &n.text {
                assert!(crate::text::is_clean(t), "text of {}: {t:?}", n.id);
            }
            replaced += n.label.matches(crate::text::REPLACEMENT).count();
        }
        // Every hostile label (rule, skill, namespace, two tools, part kind) was kept, filtered.
        assert!(replaced >= 6, "{:#?}", tree.nodes);
        let labels: Vec<_> = tree.nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(labels.contains(&"Tool Result: \u{FFFD}31m second line"), "{labels:?}");
        assert!(labels.contains(&"\u{FFFD}31m.mdc"), "{labels:?}");
    }

    #[test]
    fn sanitizing_keeps_python_sizes() {
        // One replacement char per removed char: counts match the raw text.
        let raw = format!("<user_query>{OSC52}{DCS}{CSI8}</user_query>");
        let store = temp_store("sizes", &[("u", json!({"role": "user", "content": raw}))]);
        let tree = build_tree(&store, &["u".into()], &[]).unwrap();
        let msg = tree.nodes.iter().find(|n| n.kind == "message").unwrap();
        assert_eq!(msg.character_count, chars(&raw));
        assert_eq!(msg.estimated_tokens, categories::estimate(&raw));
        assert_eq!(chars(msg.text.as_deref().unwrap()), chars(&raw));
    }

    #[test]
    fn clean_label_reuses_clean_input() {
        let s = String::from("plain label");
        let ptr = s.as_ptr();
        let kept = clean_label(s);
        assert_eq!(kept.as_ptr(), ptr);
        assert_eq!(clean_label("a\tb\x1b".into()), "a b\u{FFFD}");
    }

    #[test]
    fn title_and_paths() {
        assert_eq!(title("available subagent models"), "Available Subagent Models");
        assert_eq!(dirname("/a/b/SKILL.md"), "/a/b");
        assert_eq!(basename(dirname("SKILL.md")), "");
    }
}
