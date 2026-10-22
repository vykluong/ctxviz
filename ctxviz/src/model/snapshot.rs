//! Snapshot: everything the renderers need, built from one Store read.

use crate::model::breakdown::{self, BreakdownEntry};
use crate::model::timeline::Timeline;
use crate::model::tree::{self, Tree};
use crate::store::{MetaRow, Store};
use crate::text::sanitize_single_line;
use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotOptions {
    /// Build the explorer tree (reads every message blob).
    pub tree: bool,
    /// Load/extend the timeline cache.
    pub timeline: bool,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub store: PathBuf,
    pub agent_id: Option<String>,
    pub name: Option<String>,
    pub root_blob_id: Option<String>,
    pub used: u64,
    pub max: u64,
    /// round(used / max * 100, 1), `None` when max is 0.
    pub percent: Option<f64>,
    pub breakdown: Vec<BreakdownEntry>,
    pub message_ids: Vec<String>,
    /// Root field 26.
    pub created_at: Option<u64>,
    pub tree: Tree,
    pub timeline: Option<Timeline>,
    pub meta: MetaRow,
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref().map(|v| sanitize_single_line(v).into_owned())
}

/// The meta row as renderers see it: the display strings (chat name, mode,
/// model) are single-line and free of control characters. Identifiers and
/// the raw map are left alone; `blob_encryption_key` is never displayed.
pub fn display_meta(mut meta: MetaRow) -> MetaRow {
    meta.name = clean(&meta.name);
    meta.mode = clean(&meta.mode);
    meta.last_used_model = clean(&meta.last_used_model);
    meta
}

/// Python `round(x, 1)`: Rust's float formatting rounds on the exact value too.
pub fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

impl Snapshot {
    /// meta -> latestRootBlobId -> root blob -> field 5. `Ok(None)` when the
    /// chat has no checkpoint yet (or the root insert has not landed).
    pub fn load(store: &Store, opts: SnapshotOptions) -> Result<Option<Snapshot>> {
        let meta = store.meta()?;
        let root_id = meta.latest_root_blob_id.clone().unwrap_or_default();
        let Some(root) = store.blob(&root_id)? else { return Ok(None) };
        let info = breakdown::decode_root(&root).context("decoding latest root blob")?;
        let td = info.details.unwrap_or_default();
        let tree =
            if opts.tree { tree::build_tree(store, &info.message_ids, &td.categories)? } else { Tree::default() };
        let timeline = if opts.timeline {
            let agent = meta.agent_id.clone().unwrap_or_default();
            Some(Timeline::load_or_scan(store, &agent)?)
        } else {
            None
        };
        let (used, max) = (td.used_tokens, td.max_tokens);
        let meta = display_meta(meta);
        Ok(Some(Snapshot {
            store: store.path.clone(),
            agent_id: clean(&meta.agent_id),
            name: meta.name.clone(),
            root_blob_id: meta.latest_root_blob_id.clone(),
            used,
            max,
            percent: (max > 0).then(|| round1(used as f64 / max as f64 * 100.0)),
            breakdown: td.categories,
            message_ids: info.message_ids,
            created_at: info.created_at,
            tree,
            timeline,
            meta,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_meta_cleans_display_strings_only() {
        let meta = MetaRow {
            name: Some("chat \x1b]52;c;QUJD\x07".into()),
            mode: Some("\u{9b}31m".into()),
            last_used_model: Some("model\nx".into()),
            blob_encryption_key: Some("k".into()),
            agent_id: Some("abc".into()),
            ..Default::default()
        };
        let m = display_meta(meta);
        assert_eq!(m.name.as_deref(), Some("chat \u{FFFD}]52;c;QUJD\u{FFFD}"));
        assert_eq!(m.mode.as_deref(), Some("\u{FFFD}31m"));
        assert_eq!(m.last_used_model.as_deref(), Some("model x"));
        assert_eq!(m.agent_id.as_deref(), Some("abc"));
        assert_eq!(m.blob_encryption_key.as_deref(), Some("k"));
    }
}
