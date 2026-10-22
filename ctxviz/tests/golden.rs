//! Golden tests for the snapshot JSON.
//!
//! The synthetic store from `tests/common` always runs; its expected values
//! below were produced by the Python reference (`scripts/context_usage.py
//! --json`) on the same store. The real-store comparison against
//! `tests/fixtures/{store.db,store.json}` (private, gitignored, made by
//! `scripts/make-fixture.sh`) runs only with `CTXVIZ_REAL_FIXTURE=1`, and
//! then fails if the fixture is missing.

mod common;

use common::synthetic;
use ctxviz::model::snapshot::{Snapshot, SnapshotOptions};
use ctxviz::render::json;
use ctxviz::store::Store;
use serde_json::{json, Value};

fn snapshot_json(store: &Store) -> Value {
    let snap =
        Snapshot::load(store, SnapshotOptions { tree: true, timeline: false }).unwrap().expect("store has a root");
    json::to_value(&snap).unwrap()
}

/// Tree nodes sorted by id.
fn nodes_by_id(v: &Value) -> Vec<Value> {
    let mut nodes = v["tree"]["nodes"].as_array().expect("tree.nodes array").clone();
    nodes.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    nodes
}

/// Fast-path decode of the latest root agrees with the full decode.
fn check_latest_root(store: &Store) {
    let id = store.meta().unwrap().latest_root_blob_id.unwrap();
    let root = store.blob(&id).unwrap().unwrap();
    assert!(ctxviz::schema::is_root(&root));
    let fast = ctxviz::model::breakdown::token_details_only(&root).unwrap();
    let full = ctxviz::model::breakdown::decode_root(&root).unwrap().details;
    assert_eq!(fast, full);
}

// --------------------------------------------------------------------------
// Synthetic store (always runs)

/// (id, parentId, kind, label, categoryId, estimatedTokens, characterCount,
/// localEstimate), in build order, as the Python reference emits them.
type GoldenNode = (&'static str, Option<&'static str>, &'static str, &'static str, &'static str, u64, u64, Option<u64>);

const CONV: Option<&str> = Some("category:conversation");
const RULES: Option<&str> = Some("section:rules:1:1");
const SKILLS: Option<&str> = Some("section:agent_skills:1:1");
const TURN1: Option<&str> = Some("turn:1");
const TURN2: Option<&str> = Some("turn:2");

#[rustfmt::skip]
const GOLDEN_TREE: [GoldenNode; 26] = [
    ("category:system_prompt", None, "category", "System prompt", "system_prompt", 400, 65, Some(16)),
    ("category:tools", None, "category", "Tool definitions (server-side, no per-tool detail locally)", "tools", 600, 0, None),
    ("category:rules", None, "category", "Rules", "rules", 200, 193, Some(48)),
    ("category:skills", None, "category", "Skills", "skills", 120, 146, Some(36)),
    ("category:mcp", None, "category", "MCP & dynamic tools", "mcp", 0, 0, None),
    ("category:subagents", None, "category", "Subagent definitions", "subagents", 0, 0, None),
    ("category:summarized_conversation", None, "category", "Summarized conversation", "summarized_conversation", 0, 0, None),
    ("category:conversation", None, "category", "Conversation", "conversation", 1280, 291, Some(74)),
    ("turn:0", CONV, "group", "Before first user message", "conversation", 0, 0, None),
    ("msg:0", Some("category:system_prompt"), "message", "System prompt", "system_prompt", 16, 65, None),
    ("section:rules:1:1", Some("category:rules"), "group", "Rules", "rules", 48, 193, None),
    ("section:rules:1:1:0", RULES, "item", "style.mdc", "rules", 44, 178, None),
    ("section:rules:1:1:rest", RULES, "item", "(section text)", "rules", 4, 15, None),
    ("section:agent_skills:1:1", Some("category:skills"), "group", "Agent Skills", "skills", 36, 146, None),
    ("section:agent_skills:1:1:0", SKILLS, "item", "inventory", "skills", 29, 117, None),
    ("section:agent_skills:1:1:rest", SKILLS, "item", "(section text)", "skills", 7, 29, None),
    ("turn:1", CONV, "group", "Turn 1", "conversation", 57, 226, None),
    ("msg:2:0", TURN1, "message", "User Message", "conversation", 17, 68, None),
    ("msg:3:0", TURN1, "message", "Thinking", "conversation", 11, 43, None),
    ("msg:3:1", TURN1, "message", "Agent Message", "conversation", 8, 31, None),
    ("msg:3:2", TURN1, "message", "Tool: Read", "conversation", 6, 25, None),
    ("msg:4:0", TURN1, "message", "Tool Result: Read", "conversation", 6, 23, None),
    ("msg:5:0", TURN1, "message", "Agent Message", "conversation", 9, 36, None),
    ("turn:2", CONV, "group", "Turn 2", "conversation", 17, 65, None),
    ("msg:6:0", TURN2, "message", "User Message", "conversation", 12, 46, None),
    ("msg:7:0", TURN2, "message", "Agent Message", "conversation", 5, 19, None),
];

fn golden_node(n: &GoldenNode) -> Value {
    let (id, parent, kind, label, cat, tokens, chars, local) = *n;
    let mut v = json!({
        "id": id, "parentId": parent, "kind": kind, "label": label,
        "categoryId": cat, "estimatedTokens": tokens, "characterCount": chars,
    });
    if let Some(l) = local {
        v["localEstimate"] = json!(l);
    }
    v
}

fn synthetic_store(name: &str) -> (common::SyntheticStore, Store) {
    let fx = common::SyntheticStore::new(name);
    let store = Store::open(&fx.path).unwrap();
    (fx, store)
}

#[test]
fn synthetic_snapshot_shape_and_totals() {
    let (fx, store) = synthetic_store("golden-shape");
    let got = snapshot_json(&store);
    let mut keys: Vec<&str> = got.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want_keys =
        vec!["store", "agentId", "name", "rootBlobId", "usedTokens", "maxTokens", "percent", "breakdown", "tree"];
    want_keys.sort_unstable();
    assert_eq!(keys, want_keys, "top-level keys (no timeline without the option)");

    assert_eq!(got["store"], json!(fx.path.to_string_lossy()));
    assert_eq!(got["agentId"], json!(synthetic::AGENT_ID));
    assert_eq!(got["name"], json!(synthetic::NAME));
    assert_eq!(got["rootBlobId"], json!(synthetic::latest_root_id()));
    assert_eq!(got["usedTokens"], json!(synthetic::USED_TOKENS));
    assert_eq!(got["maxTokens"], json!(synthetic::MAX_TOKENS));
    assert_eq!(got["percent"], json!(synthetic::PERCENT));
    assert_eq!(got["tree"]["schemaVersion"], json!(1));

    // Category breakdown in server order, summing to the used total.
    let want: Vec<Value> = synthetic::BREAKDOWN
        .iter()
        .map(|(id, label, t)| json!({"id": id, "label": label, "estimatedTokens": t}))
        .collect();
    assert_eq!(got["breakdown"], Value::Array(want));
    let sum: u64 = got["breakdown"].as_array().unwrap().iter().map(|c| c["estimatedTokens"].as_u64().unwrap()).sum();
    assert_eq!(sum, synthetic::USED_TOKENS);
}

#[test]
fn synthetic_tree_matches_golden() {
    let (_fx, store) = synthetic_store("golden-tree");
    let got = snapshot_json(&store);
    let nodes = got["tree"]["nodes"].as_array().unwrap();
    let want: Vec<Value> = GOLDEN_TREE.iter().map(golden_node).collect();
    assert_eq!(nodes.len(), want.len(), "node count");
    for (g, w) in nodes.iter().zip(&want) {
        assert_eq!(g, w, "node {}", w["id"]);
    }
    // Top-level server totals replace the local estimates.
    for (id, _, t) in synthetic::BREAKDOWN {
        let node = nodes.iter().find(|n| n["id"] == format!("category:{id}")).unwrap();
        assert_eq!(node["estimatedTokens"], json!(t), "{id}");
    }
}

#[test]
fn synthetic_latest_root_passes_structural_test() {
    let (_fx, store) = synthetic_store("golden-root");
    check_latest_root(&store);
    let root = store.blob(&synthetic::latest_root_id()).unwrap().unwrap();
    let info = ctxviz::model::breakdown::decode_root(&root).unwrap();
    assert_eq!(info.message_ids.len(), synthetic::CHECKPOINTS[2].0);
    assert_eq!(info.created_at, Some(synthetic::CREATED_AT));
}

#[test]
fn synthetic_meta_round_trips() {
    let (_fx, store) = synthetic_store("golden-meta");
    let meta = store.meta().unwrap();
    assert_eq!(meta.agent_id.as_deref(), Some(synthetic::AGENT_ID));
    assert_eq!(meta.mode.as_deref(), Some(synthetic::MODE));
    assert_eq!(meta.last_used_model.as_deref(), Some(synthetic::MODEL));
    assert_eq!(meta.created_at, Some(json!(synthetic::CREATED_AT)));
    assert!(meta.blob_encryption_key.is_none());
    assert_eq!(store.max_rowid().unwrap(), synthetic::ROWS);
}

// --------------------------------------------------------------------------
// Real store (CTXVIZ_REAL_FIXTURE=1)

fn real_fixture() -> Option<(Store, Value)> {
    let db = common::real_fixture("store.db")?;
    let want = common::real_fixture("store.json")?;
    let want = serde_json::from_slice(&std::fs::read(want).unwrap()).unwrap();
    Some((Store::open(db).unwrap(), want))
}

#[test]
fn real_snapshot_matches_python() {
    let Some((store, want)) = real_fixture() else { return };
    let got = snapshot_json(&store);
    for key in ["usedTokens", "maxTokens", "breakdown", "percent", "agentId", "name", "rootBlobId"] {
        assert_eq!(got[key], want[key], "key {key}");
    }
}

#[test]
fn real_latest_root_passes_structural_test() {
    let Some((store, _)) = real_fixture() else { return };
    check_latest_root(&store);
}

/// Explorer tree nodes equal the Python output, compared sorted by id.
#[test]
fn real_tree_matches_python() {
    let Some((store, want)) = real_fixture() else { return };
    let got = nodes_by_id(&snapshot_json(&store));
    let want = nodes_by_id(&want);
    assert!(!want.is_empty());
    assert_eq!(got.len(), want.len(), "node count");
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g, w);
    }
}
