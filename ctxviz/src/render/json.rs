//! Snapshot JSON, same shape as `context_usage.py --json` plus `timeline`.

use crate::model::breakdown::BreakdownEntry;
use crate::model::snapshot::Snapshot;
use crate::model::timeline::Timeline;
use crate::model::tree::Tree;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotJson<'a> {
    pub store: String,
    pub agent_id: Option<&'a str>,
    pub name: Option<&'a str>,
    pub root_blob_id: Option<&'a str>,
    pub used_tokens: u64,
    pub max_tokens: u64,
    pub percent: Option<f64>,
    pub breakdown: &'a [BreakdownEntry],
    pub tree: &'a Tree,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline: Option<&'a Timeline>,
}

impl<'a> From<&'a Snapshot> for SnapshotJson<'a> {
    fn from(s: &'a Snapshot) -> Self {
        SnapshotJson {
            store: s.store.to_string_lossy().into_owned(),
            agent_id: s.agent_id.as_deref(),
            name: s.name.as_deref(),
            root_blob_id: s.root_blob_id.as_deref(),
            used_tokens: s.used,
            max_tokens: s.max,
            percent: s.percent,
            breakdown: &s.breakdown,
            tree: &s.tree,
            timeline: s.timeline.as_ref(),
        }
    }
}

pub fn to_value(s: &Snapshot) -> serde_json::Result<serde_json::Value> {
    serde_json::to_value(SnapshotJson::from(s))
}

/// Compact single-line JSON (one NDJSON record), with terminal-unsafe
/// characters escaped by [`escape_unsafe`].
pub fn to_string(s: &Snapshot) -> serde_json::Result<String> {
    serde_json::to_string(&SnapshotJson::from(s)).map(escape_unsafe)
}

/// True for the characters serde_json emits raw but a terminal or a
/// JavaScript consumer may treat specially: DEL, the C1 controls, and the
/// LINE SEPARATOR and PARAGRAPH SEPARATOR characters.
fn is_unsafe(c: char) -> bool {
    matches!(c, '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}')
}

/// Post-pass over serialized JSON: rewrites each [`is_unsafe`] character as a
/// `\uXXXX` escape. Outside strings, valid JSON holds only ASCII structural
/// characters, so every match is inside a string literal, where the escape
/// decodes back to the same character. Clean input is returned unchanged.
pub fn escape_unsafe(json: String) -> String {
    if !json.chars().any(is_unsafe) {
        return json;
    }
    let mut out = String::with_capacity(json.len() + 16);
    for c in json.chars() {
        if is_unsafe(c) {
            // Writing to a String cannot fail.
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_del_c1_and_line_separators() {
        let v = "\u{9b}31m\u{7f}\u{2028}";
        let out = escape_unsafe(serde_json::to_string(&serde_json::json!({ "k": v })).unwrap());
        assert!(out.chars().all(|c| c <= '\u{7e}'), "{out:?}");
        assert_eq!(out, r#"{"k":"\u009b31m\u007f\u2028"}"#);
        let back: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(back["k"], v);
    }

    #[test]
    fn clean_json_is_unchanged() {
        let json = serde_json::to_string(&serde_json::json!({ "k": "plain \u{e9}\u{1f600} \u{a0}" })).unwrap();
        assert_eq!(escape_unsafe(json.clone()), json);
    }
}
