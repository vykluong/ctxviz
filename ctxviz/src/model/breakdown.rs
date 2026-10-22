//! ConversationTokenDetails decoding (port of `decode_token_details`).

use crate::text::sanitize_single_line;
use crate::{pb, schema};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownEntry {
    pub id: String,
    pub label: String,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenDetails {
    pub used_tokens: u64,
    pub max_tokens: u64,
    pub categories: Vec<BreakdownEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootInfo {
    /// Hex message blob ids from root field 1, in order.
    pub message_ids: Vec<String>,
    /// `None` when the root has no field 5 (first checkpoints of a chat).
    pub details: Option<TokenDetails>,
    /// Root field 26.
    pub created_at: Option<u64>,
}

/// Full root decode: message ids, token details, createdAt.
pub fn decode_root(root: &[u8]) -> Result<RootInfo> {
    let mut out = RootInfo::default();
    let mut td = None;
    for f in pb::fields(root) {
        let f = f.context("root blob is not valid protobuf")?;
        match (f.number, f.value) {
            (schema::root::MESSAGE_IDS, pb::Value::Bytes(b)) => out.message_ids.push(schema::hex(b)),
            (schema::root::TOKEN_DETAILS, pb::Value::Bytes(b)) if td.is_none() => td = Some(b),
            (schema::root::CREATED_AT, v) if out.created_at.is_none() => out.created_at = v.as_u64(),
            _ => {}
        }
    }
    out.details = td.map(decode_token_details).transpose()?;
    Ok(out)
}

/// Fast path: decode only root field 5, skipping everything else.
pub fn token_details_only(root: &[u8]) -> Result<Option<TokenDetails>> {
    match pb::find_first(root, schema::root::TOKEN_DETAILS)? {
        Some(v) => {
            let b = v.as_bytes().context("root field 5 is not length-delimited")?;
            Ok(Some(decode_token_details(b)?))
        }
        None => Ok(None),
    }
}

/// Decode an agent.v1.ConversationTokenDetails message.
pub fn decode_token_details(td: &[u8]) -> Result<TokenDetails> {
    use schema::{breakdown as bd, category as cat, token_details as t};
    let g = pb::group(td).context("token details are not valid protobuf")?;
    let first_u64 = |g: &std::collections::BTreeMap<u32, Vec<pb::Value>>, n| {
        g.get(&n).and_then(|v| v.first()).and_then(|v| v.as_u64()).unwrap_or(0)
    };
    let mut out = TokenDetails {
        used_tokens: first_u64(&g, t::USED_TOKENS),
        max_tokens: first_u64(&g, t::MAX_TOKENS),
        categories: Vec::new(),
    };
    if let Some(b) = g.get(&t::BREAKDOWN).and_then(|v| v.first()).and_then(|v| v.as_bytes()) {
        for raw in pb::find_all(b, bd::CATEGORIES)? {
            let raw = raw.as_bytes().context("breakdown category is not a message")?;
            let c = pb::group(raw)?;
            let s = |n| {
                c.get(&n)
                    .and_then(|v| v.first())
                    .and_then(|v| v.as_bytes())
                    // Server-supplied strings are shown in every renderer:
                    // strip control characters here, once.
                    .map(|b| sanitize_single_line(&String::from_utf8_lossy(b)).into_owned())
                    .unwrap_or_default()
            };
            out.categories.push(BreakdownEntry {
                id: s(cat::ID),
                label: s(cat::LABEL),
                estimated_tokens: first_u64(&c, cat::ESTIMATED_TOKENS),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ld(field: u32, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![((field << 3) | 2) as u8, bytes.len() as u8];
        out.extend_from_slice(bytes);
        out
    }

    #[test]
    fn category_strings_are_sanitized_at_decode() {
        use schema::{breakdown as bd, category as cat, token_details as t};
        let mut c = ld(cat::ID, "\u{9b}31mid".as_bytes());
        c.extend(ld(cat::LABEL, b"\x1b]52;c;QUJD\x07\nlabel"));
        let td = ld(t::BREAKDOWN, &ld(bd::CATEGORIES, &c));
        let out = decode_token_details(&td).unwrap();
        assert_eq!(out.categories.len(), 1);
        assert_eq!(out.categories[0].id, "\u{FFFD}31mid");
        assert_eq!(out.categories[0].label, "\u{FFFD}]52;c;QUJD\u{FFFD} label");
    }
}
