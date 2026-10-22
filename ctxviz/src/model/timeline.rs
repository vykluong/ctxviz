//! Checkpoint timeline with an incremental rowid cache.
//!
//! Every checkpoint inserts a new root blob and `blobs.rowid` is insertion
//! ordered, so scanning `rowid > last_rowid` yields new checkpoints in
//! chronological order. Roots come in twins (differing only in field 4); they
//! are collapsed on `(message_count, used_tokens)`, keeping the lower rowid.
//! Roots without token details (field 5) are skipped.
//!
//! The cache lives at `<state dir>/<agentId>.json`, where the state dir is
//! `$CTXVIZ_STATE_DIR` or `~/.cursor/ctxviz` (see [`session::state_dir`]).
//! Agent ids come from the store, so ids that are not [`text::valid_id`] map
//! to a hashed name instead (see [`cache_path`]). Deleting the file resets it.

use crate::model::breakdown;
use crate::store::{self, Store};
use crate::{pb, schema, session, text};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Bump when the cache layout or the checkpoint derivation changes.
pub const CACHE_VERSION: u32 = 1;

/// Category id whose growth marks a compaction.
pub const SUMMARY_CATEGORY: &str = "summarized_conversation";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub rowid: i64,
    pub root_id: String,
    pub message_count: usize,
    pub used_tokens: u64,
    pub max_tokens: u64,
    /// (category id, tokens) in server order.
    pub categories: Vec<(String, u64)>,
    pub compaction: bool,
}

impl Checkpoint {
    /// Twin key: twins share message count and used tokens.
    pub fn key(&self) -> (usize, u64) {
        (self.message_count, self.used_tokens)
    }

    pub fn category(&self, id: &str) -> u64 {
        self.categories.iter().find(|(c, _)| c == id).map_or(0, |(_, t)| *t)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeline {
    pub version: u32,
    pub store_path: Option<String>,
    /// Root field 26; cache is invalid if it differs.
    pub store_created_at: Option<u64>,
    pub last_rowid: i64,
    pub checkpoints: Vec<Checkpoint>,
    pub last_notified_band: Option<u8>,
}

/// State dir for caches; the same dir as [`session::state_dir`].
pub fn state_dir() -> PathBuf {
    session::state_dir()
}

/// `<state dir>/<agentId>.json` for a [`text::valid_id`] id. Any other id (one
/// with `/`, `..`, a leading dot, ...) maps to `<state dir>/invalid-<md5>.json`,
/// so a crafted `meta.agentId` can never place the cache outside the state dir.
pub fn cache_path(agent_id: &str) -> PathBuf {
    if text::valid_id(agent_id) {
        state_dir().join(format!("{agent_id}.json"))
    } else {
        state_dir().join(format!("invalid-{:x}.json", md5::compute(agent_id.as_bytes())))
    }
}

fn store_path_string(store: &Store) -> String {
    store::abspath(&store.path).to_string_lossy().into_owned()
}

/// Cheap pre-filter before the structural root test: message blobs are JSON.
fn maybe_root(data: &[u8]) -> bool {
    !matches!(data.iter().find(|b| !b.is_ascii_whitespace()), None | Some(b'{') | Some(b'['))
}

/// The first roots of a chat carry a field 5 with zero usage (no server
/// count yet); treat them like roots without token details.
fn has_usage(td: &breakdown::TokenDetails) -> bool {
    td.used_tokens > 0
}

/// Twin key of a root blob, if it is a root with token details.
fn root_key(data: &[u8]) -> Option<(usize, u64)> {
    if !maybe_root(data) || !schema::is_root(data) {
        return None;
    }
    let info = breakdown::decode_root(data).ok()?;
    let td = info.details.filter(has_usage)?;
    Some((info.message_ids.len(), td.used_tokens))
}

/// Field 26 of a root blob.
fn created_at(data: &[u8]) -> Option<u64> {
    pb::find_first(data, schema::root::CREATED_AT).ok()??.as_u64()
}

impl Timeline {
    /// Empty timeline bound to `store`, ready for a scan from rowid 0.
    pub fn new(store: &Store) -> Timeline {
        Timeline { version: CACHE_VERSION, store_path: Some(store_path_string(store)), ..Timeline::default() }
    }

    /// Full scan from rowid 0, no cache.
    pub fn scan(store: &Store) -> Result<Timeline> {
        let mut t = Timeline::new(store);
        t.extend(store)?;
        Ok(t)
    }

    /// Load the cache for `agent_id` (see [`cache_path`]), extend it from new
    /// rowids (or rescan when stale), and save it back when it changed. An
    /// empty `agent_id` scans without a cache; an id that is not
    /// [`text::valid_id`] uses the hashed name from [`cache_path`].
    pub fn load_or_scan(store: &Store, agent_id: &str) -> Result<Timeline> {
        if agent_id.is_empty() {
            return Timeline::scan(store);
        }
        Timeline::load_or_scan_at(store, &cache_path(agent_id))
    }

    /// [`load_or_scan`](Self::load_or_scan) with an explicit cache file.
    /// Saving is best effort: a read-only state dir never fails the load.
    pub fn load_or_scan_at(store: &Store, path: &Path) -> Result<Timeline> {
        let cached = Timeline::load(path);
        let (mut t, from_cache) = match &cached {
            Some(c) if c.is_valid_for(store)? => (c.clone(), true),
            _ => (Timeline::new(store), false),
        };
        t.extend(store)?;
        if from_cache && !t.has_latest_root(store)? {
            t = Timeline::scan(store)?;
        }
        if cached.as_ref() != Some(&t) {
            let _ = t.save(path);
        }
        Ok(t)
    }

    /// Read a cache file; `None` when missing, unparsable, or larger than
    /// [`session::MAX_STATE_BYTES`].
    pub fn load(path: &Path) -> Option<Timeline> {
        serde_json::from_slice(&session::read_capped(path)?).ok()
    }

    /// Write the cache atomically (temp file, rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let res = session::write_atomic(path, &serde_json::to_vec(self)?);
        if path.starts_with(store::NO_HOME) {
            // Keep the internal sentinel path out of the message.
            return res.context("saving timeline cache");
        }
        res.with_context(|| format!("saving timeline cache {}", path.display()))
    }

    /// Version, store path, createdAt and rowid range all agree with `store`.
    fn is_valid_for(&self, store: &Store) -> Result<bool> {
        if self.version != CACHE_VERSION || self.store_path.as_deref() != Some(&store_path_string(store)) {
            return Ok(false);
        }
        if self.last_rowid > store.max_rowid()? {
            return Ok(false);
        }
        let latest = match store.latest_root_id()? {
            Some(id) => store.blob(&id)?,
            None => None,
        };
        Ok(match latest.as_deref().and_then(created_at) {
            Some(c) => self.store_created_at == Some(c),
            // No latest root yet: nothing to contradict the cache.
            None => true,
        })
    }

    /// The store's latestRootBlobId is a checkpoint, or the twin of one, or
    /// carries no token details (so it never becomes a checkpoint). A missing
    /// root blob (meta updated before the root insert landed) also passes.
    fn has_latest_root(&self, store: &Store) -> Result<bool> {
        let Some(id) = store.latest_root_id()? else { return Ok(true) };
        if self.checkpoints.iter().any(|c| c.root_id == id) {
            return Ok(true);
        }
        let Some(data) = store.blob(&id)? else { return Ok(true) };
        Ok(match root_key(&data) {
            Some(key) => self.checkpoints.iter().any(|c| c.key() == key),
            None => true,
        })
    }

    /// Append checkpoints from rows with rowid > last_rowid; returns how many.
    pub fn extend(&mut self, store: &Store) -> Result<usize> {
        let mut seen: HashSet<(usize, u64)> = self.checkpoints.iter().map(Checkpoint::key).collect();
        let before = self.checkpoints.len();
        store.for_each_row(self.last_rowid, |row| {
            self.last_rowid = self.last_rowid.max(row.rowid);
            self.push_row(row, &mut seen);
            Ok(())
        })?;
        Ok(self.checkpoints.len() - before)
    }

    /// Append `row` as a checkpoint if it is a new root with usage.
    fn push_row(&mut self, row: store::BlobRow, seen: &mut HashSet<(usize, u64)>) {
        if !maybe_root(&row.data) || !schema::is_root(&row.data) {
            return;
        }
        let Ok(info) = breakdown::decode_root(&row.data) else { return };
        if self.store_created_at.is_none() {
            self.store_created_at = info.created_at;
        }
        let Some(td) = info.details.filter(has_usage) else { return };
        let key = (info.message_ids.len(), td.used_tokens);
        if !seen.insert(key) {
            return;
        }
        let categories: Vec<(String, u64)> = td.categories.into_iter().map(|c| (c.id, c.estimated_tokens)).collect();
        let mut cp = Checkpoint {
            rowid: row.rowid,
            root_id: row.id,
            message_count: key.0,
            used_tokens: td.used_tokens,
            max_tokens: td.max_tokens,
            categories,
            compaction: false,
        };
        if let Some(prev) = self.checkpoints.last() {
            cp.compaction = cp.category(SUMMARY_CATEGORY) > prev.category(SUMMARY_CATEGORY)
                || (cp.used_tokens < prev.used_tokens && cp.message_count > prev.message_count);
        }
        self.checkpoints.push(cp);
    }

    /// Change in used tokens from each checkpoint to the next, aligned with
    /// `checkpoints[1..]` (empty for fewer than two checkpoints).
    pub fn deltas(&self) -> Vec<i64> {
        self.checkpoints.windows(2).map(|w| w[1].used_tokens as i64 - w[0].used_tokens as i64).collect()
    }

    /// Cheap per-turn proxy: each checkpoint after the first with its delta.
    pub fn per_turn(&self) -> Vec<(&Checkpoint, i64)> {
        self.checkpoints.iter().skip(1).zip(self.deltas()).collect()
    }

    pub fn compactions(&self) -> Vec<&Checkpoint> {
        self.checkpoints.iter().filter(|c| c.compaction).collect()
    }

    pub fn last_delta(&self) -> Option<i64> {
        match self.checkpoints.as_slice() {
            [.., a, b] => Some(b.used_tokens as i64 - a.used_tokens as i64),
            _ => None,
        }
    }

    /// Used tokens of the last `n` checkpoints, oldest first (sparkline input).
    pub fn recent_used(&self, n: usize) -> Vec<u64> {
        let start = self.checkpoints.len().saturating_sub(n);
        self.checkpoints[start..].iter().map(|c| c.used_tokens).collect()
    }
}
