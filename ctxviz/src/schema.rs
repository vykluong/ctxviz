//! Every protobuf field number ctxviz relies on, in one place. If Cursor
//! renumbers its messages, `ctxviz doctor` dumps the new layout and only this
//! file needs to change.

use crate::pb;

/// ConversationStateStructure (root blob).
pub mod root {
    /// repeated bytes: ordered message blob ids (32 raw bytes, hex = blob id).
    pub const MESSAGE_IDS: u32 = 1;
    /// bytes: differs between the twin roots written per checkpoint.
    pub const TWIN_MARKER: u32 = 4;
    /// agent.v1.ConversationTokenDetails.
    pub const TOKEN_DETAILS: u32 = 5;
    /// varint: conversation createdAt (same across all roots of a store).
    pub const CREATED_AT: u32 = 26;
}

/// agent.v1.ConversationTokenDetails.
pub mod token_details {
    pub const USED_TOKENS: u32 = 1;
    pub const MAX_TOKENS: u32 = 2;
    /// agent.v1.PromptTokenBreakdown.
    pub const BREAKDOWN: u32 = 3;
}

/// agent.v1.PromptTokenBreakdown.
pub mod breakdown {
    /// repeated PromptTokenBreakdownCategory.
    pub const CATEGORIES: u32 = 3;
}

/// agent.v1.PromptTokenBreakdownCategory.
pub mod category {
    pub const ID: u32 = 1;
    pub const LABEL: u32 = 2;
    pub const ESTIMATED_TOKENS: u32 = 3;
    /// varint seen in the wild, not read by the Python reference (likely chars).
    pub const UNKNOWN_4: u32 = 4;
}

/// Byte length of a message id in root field 1 (hex form is 64 chars).
pub const MESSAGE_ID_LEN: usize = 32;

/// Meta table key holding the hex-encoded JSON meta object.
pub const META_KEY: &str = "0";

/// True if the bytes look like a JSON document rather than protobuf.
pub fn looks_like_json(data: &[u8]) -> bool {
    let first = data.iter().find(|b| !b.is_ascii_whitespace());
    matches!(first, Some(b'{') | Some(b'[')) && serde_json::from_slice::<serde::de::IgnoredAny>(data).is_ok()
}

/// Structural root test: not JSON, decodes cleanly as protobuf, field 1
/// entries are all 32-byte ids (64 hex chars), and field 26 is a varint.
pub fn is_root(data: &[u8]) -> bool {
    if data.is_empty() || looks_like_json(data) {
        return false;
    }
    let mut ids = 0usize;
    let mut created = false;
    for f in pb::fields(data) {
        let Ok(f) = f else { return false };
        match (f.number, f.value) {
            (root::MESSAGE_IDS, pb::Value::Bytes(b)) if b.len() == MESSAGE_ID_LEN => ids += 1,
            (root::MESSAGE_IDS, _) => return false,
            (root::CREATED_AT, pb::Value::Varint(_)) => created = true,
            (root::CREATED_AT, _) => return false,
            _ => {}
        }
    }
    ids > 0 && created
}

pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
