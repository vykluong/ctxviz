//! Shared test helpers: unique temp dirs, a small chat-store builder, and a
//! synthetic fixture store with known contents.
//!
//! Integration tests use this file with `mod common;`. The library's unit
//! tests include the same file under `#[cfg(test)]` as `crate::test_support`
//! (see `src/lib.rs`), so it depends only on std, rusqlite, and serde_json.
//!
//! Every id, timestamp, and piece of text in the synthetic store is invented.
//! The real-store fixture (`tests/fixtures/`, gitignored) is opt-in: see
//! [`real_fixture`].
#![allow(dead_code)]

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// --------------------------------------------------------------------------
// Temp dirs

/// A fresh temp dir unique to one test (pid, nanos, sequence, name),
/// canonicalized, and removed with everything in it on drop.
#[derive(Debug)]
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let dir = std::env::temp_dir().join(format!("ctxviz-test-{}-{nanos}-{seq}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// --------------------------------------------------------------------------
// Protobuf encoding (just enough for root blobs)

pub mod pb {
    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    pub fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
        varint(u64::from(number) << 3, out);
        varint(v, out);
    }

    pub fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
        varint((u64::from(number) << 3) | 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Token details for a root blob: used, max, and (id, label, tokens) per category.
pub struct Details<'a> {
    pub used: u64,
    pub max: u64,
    pub categories: &'a [(&'a str, &'a str, u64)],
}

/// A root blob (field numbers from `docs/store-format.md` and `src/schema.rs`):
/// message ids (1), twin marker (4), token details (5), createdAt (26).
pub fn root_blob(message_ids: &[[u8; 32]], twin: u8, details: Option<&Details>, created_at: u64) -> Vec<u8> {
    let mut b = Vec::new();
    for id in message_ids {
        pb::bytes(1, id, &mut b);
    }
    pb::bytes(4, &[twin; 4], &mut b);
    if let Some(d) = details {
        let mut breakdown = Vec::new();
        for (id, label, tokens) in d.categories {
            let mut c = Vec::new();
            pb::bytes(1, id.as_bytes(), &mut c);
            pb::bytes(2, label.as_bytes(), &mut c);
            pb::uint(3, *tokens, &mut c);
            // Field 4 appears in real stores and is ignored by the decoder.
            pb::uint(4, *tokens * 4, &mut c);
            pb::bytes(3, &c, &mut breakdown);
        }
        let mut td = Vec::new();
        pb::uint(1, d.used, &mut td);
        pb::uint(2, d.max, &mut td);
        pb::bytes(3, &breakdown, &mut td);
        pb::bytes(5, &td, &mut b);
    }
    pb::uint(26, created_at, &mut b);
    b
}

// --------------------------------------------------------------------------
// Store builder

/// Writes a chat store: `blobs(id, data)` and `meta(key, value)` with the
/// meta object hex-encoded under key `'0'`. Rows get rowids in call order.
pub struct StoreBuilder {
    pub path: PathBuf,
    conn: Connection,
}

impl StoreBuilder {
    pub fn create(path: &Path) -> StoreBuilder {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "create table blobs(id text primary key, data blob); create table meta(key text primary key, value text);",
        )
        .unwrap();
        StoreBuilder { path: path.to_path_buf(), conn }
    }

    pub fn blob(&self, id: &str, data: &[u8]) -> &Self {
        self.conn.execute("insert into blobs(id, data) values (?, ?)", rusqlite::params![id, data]).unwrap();
        self
    }

    pub fn json(&self, id: &str, v: &Value) -> &Self {
        self.blob(id, &serde_json::to_vec(v).unwrap())
    }

    pub fn meta(&self, v: &Value) -> &Self {
        self.conn
            .execute("insert or replace into meta(key, value) values ('0', ?)", [hex(v.to_string().as_bytes())])
            .unwrap();
        self
    }

    /// Close the connection and return the store path.
    pub fn finish(self) -> PathBuf {
        self.path
    }
}

// --------------------------------------------------------------------------
// Synthetic fixture

/// Invented ids and values of the synthetic store.
pub mod synthetic {
    pub const AGENT_ID: &str = "00000000-feed-4000-8000-0000000000c7";
    pub const NAME: &str = "Synthetic widget inventory";
    pub const MODE: &str = "agent";
    pub const MODEL: &str = "synthetic-model-1";
    pub const CREATED_AT: u64 = 1_700_000_000_000;
    pub const MAX_TOKENS: u64 = 200_000;
    /// Breakdown of the latest root, server order; sums to [`USED_TOKENS`].
    pub const BREAKDOWN: [(&str, &str, u64); 5] = [
        ("system_prompt", "System prompt", 400),
        ("tools", "Tool definitions", 600),
        ("rules", "Rules", 200),
        ("skills", "Skills", 120),
        ("conversation", "Conversation", 1280),
    ];
    pub const USED_TOKENS: u64 = 2600;
    pub const PERCENT: f64 = 1.3;
    /// (message count, used tokens) of each timeline checkpoint, oldest first.
    pub const CHECKPOINTS: [(usize, u64); 3] = [(3, 1500), (6, 2100), (8, 2600)];
    /// Roots that pass the structural test, including the two without usage.
    pub const ROOTS: usize = 8;
    /// Roots with usage (each checkpoint is written twice, as twins).
    pub const ROOTS_WITH_USAGE: usize = 6;
    /// Rows in `blobs`: 8 messages and 8 roots.
    pub const ROWS: i64 = 16;
    /// `latestRootBlobId`: the second twin of the last checkpoint.
    pub fn latest_root_id() -> String {
        super::hex(&root_id(3, 1))
    }

    /// Message id `n` (32 raw bytes; the blob id is its hex form).
    pub fn message_id(n: u8) -> [u8; 32] {
        let mut b = [0x5a; 32];
        b[31] = n;
        b
    }

    /// Root id of checkpoint `cp` (0 = before usage), twin `t`.
    pub fn root_id(cp: u8, t: u8) -> [u8; 32] {
        let mut b = [0xc0; 32];
        b[30] = cp;
        b[31] = t;
        b
    }
}

/// The eight synthetic messages, in conversation order.
fn synthetic_messages() -> [Value; 8] {
    let context = concat!(
        "<rules><always_applied_workspace_rule name=\"/synthetic/project/.cursor/rules/style.mdc\">",
        "Prefer short functions and descriptive names in the demo project.",
        "</always_applied_workspace_rule></rules>\n",
        "<agent_skills><agent_skill fullPath=\"/synthetic/skills/inventory/SKILL.md\">",
        "Count items in a synthetic inventory file.",
        "</agent_skill></agent_skills>"
    );
    [
        json!({"role": "system", "content": "You are a synthetic assistant used only by the ctxviz test suite."}),
        json!({"role": "user", "content": [{"type": "text", "text": context}]}),
        json!({"role": "user", "content": "<user_query>How many widgets are in the demo inventory?</user_query>"}),
        json!({"role": "assistant", "content": [
            {"type": "reasoning", "text": "The inventory file should list the widgets."},
            {"type": "text", "text": "Let me read the inventory file."},
            {"type": "tool-call", "toolCallId": "call-1", "toolName": "Read", "input": {"path": "inventory.txt"}},
        ]}),
        json!({"role": "tool", "content": [
            {"type": "tool-result", "toolCallId": "call-1", "toolName": "Read",
             "output": {"type": "text", "value": "widgets: 42\ngadgets: 7\n"}},
        ]}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "The demo inventory holds 42 widgets."}]}),
        json!({"role": "user", "content": "<user_query>And how many gadgets?</user_query>"}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "It holds 7 gadgets."}]}),
    ]
}

/// Write the synthetic store to `path`. Insertion order mirrors a live chat:
/// messages land before the roots that reference them, the first roots have
/// no usage, and every checkpoint is written as two twin roots.
pub fn write_synthetic_store(path: &Path) {
    use synthetic::*;
    let b = StoreBuilder::create(path);
    let msgs = synthetic_messages();
    let ids: Vec<[u8; 32]> = (0..msgs.len() as u8).map(message_id).collect();
    let add_msgs = |range: std::ops::Range<usize>| {
        for i in range {
            b.json(&hex(&ids[i]), &msgs[i]);
        }
    };
    let add_root = |cp: u8, count: usize, details: Option<&Details>| {
        for t in 0..2 {
            b.blob(&hex(&root_id(cp, t)), &root_blob(&ids[..count], t, details, CREATED_AT));
        }
    };

    // Before usage: one root without field 5, one with zero usage.
    add_msgs(0..2);
    b.blob(&hex(&root_id(0, 0)), &root_blob(&ids[..2], 0, None, CREATED_AT));
    let zero = Details { used: 0, max: MAX_TOKENS, categories: &[] };
    b.blob(&hex(&root_id(0, 1)), &root_blob(&ids[..2], 1, Some(&zero), CREATED_AT));

    // Checkpoints: the conversation category grows, the rest stays fixed.
    let fixed: u64 = BREAKDOWN[..4].iter().map(|c| c.2).sum();
    let mut start = 2;
    for (n, &(count, used)) in CHECKPOINTS.iter().enumerate() {
        add_msgs(start..count);
        start = count;
        let mut cats = BREAKDOWN;
        cats[4].2 = used - fixed;
        add_root(n as u8 + 1, count, Some(&Details { used, max: MAX_TOKENS, categories: &cats }));
    }

    b.meta(&json!({
        "agentId": AGENT_ID,
        "name": NAME,
        "latestRootBlobId": latest_root_id(),
        "createdAt": CREATED_AT,
        "mode": MODE,
        "lastUsedModel": MODEL,
    }));
    b.finish();
}

/// The synthetic store in its own temp dir (removed on drop).
pub struct SyntheticStore {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl SyntheticStore {
    pub fn new(name: &str) -> SyntheticStore {
        let dir = TempDir::new(&format!("fixture-{name}"));
        let path = dir.join("store.db");
        write_synthetic_store(&path);
        SyntheticStore { dir, path }
    }
}

// --------------------------------------------------------------------------
// Real-store fixture (opt-in)

/// Env var that enables the real-store fixture tests.
pub const REAL_FIXTURE_ENV: &str = "CTXVIZ_REAL_FIXTURE";

/// `tests/fixtures/<file>` when `CTXVIZ_REAL_FIXTURE=1`, else `None` (with a
/// note on stderr). With the variable set, a missing file panics so the
/// opt-in run fails instead of silently skipping. Regenerate the fixture
/// with `scripts/make-fixture.sh`; it is private and never committed.
pub fn real_fixture(file: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(file);
    if std::env::var_os(REAL_FIXTURE_ENV).is_none_or(|v| v != "1") {
        eprintln!("skipping real-store test: set {REAL_FIXTURE_ENV}=1 to compare against {}", path.display());
        return None;
    }
    assert!(
        path.is_file(),
        "{REAL_FIXTURE_ENV}=1 but {} is missing; run scripts/make-fixture.sh <store.db>",
        path.display()
    );
    Some(path)
}
