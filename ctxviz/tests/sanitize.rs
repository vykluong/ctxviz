//! Store text is attacker-controlled (tool results, web pages). These tests
//! feed terminal escape sequences through the model and every text renderer
//! and assert that no control character reaches the output.
//!
//! Two layers are covered: the model boundary (`build_tree` on a real store
//! file) and the sinks (hand-built snapshots that skip the model's filtering).

use ctxviz::commands::snapshot::{render_text, render_tree};
use ctxviz::model::breakdown::BreakdownEntry;
use ctxviz::model::snapshot::Snapshot;
use ctxviz::model::timeline::{Checkpoint, Timeline};
use ctxviz::model::tree::{build_tree, Node, Tree};
use ctxviz::store::{MetaRow, Store};
use ctxviz::text::{is_clean, is_dangerous, REPLACEMENT};
use ctxviz::theme::Theme;
use ctxviz::tui::app::App;
use ratatui::{backend::TestBackend, Terminal};
use serde_json::{json, Value};
use std::path::PathBuf;

/// OSC 52: write the clipboard.
const OSC52: &str = "\x1b]52;c;QUJD\x07";
/// Kitty remote control over DCS.
const DCS: &str = "\x1bP@kitty-cmd{\"cmd\":\"launch\"}\x1b\\";
/// 8-bit CSI (C1).
const CSI8: &str = "\u{9b}31m";

/// All three payloads, for places that accept any text.
fn all() -> String {
    format!("{OSC52} {DCS} {CSI8}")
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

/// A throwaway store.db with `blobs` (id -> JSON), opened read-only. Each
/// call gets its own directory (tests run in parallel), removed on drop.
fn temp_store(name: &str, blobs: &[(&str, Value)]) -> TempStore {
    let dir = TempDir::new("ctxviz-sanitize", name);
    let path = dir.0.join("store.db");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch(
        "create table blobs(id text primary key, data blob); create table meta(key text primary key, value text);",
    )
    .unwrap();
    for (id, v) in blobs {
        c.execute("insert into blobs(id, data) values (?, ?)", rusqlite::params![id, serde_json::to_vec(v).unwrap()])
            .unwrap();
    }
    drop(c);
    TempStore { store: Some(Store::open(&path).unwrap()), _dir: dir }
}

/// Tree built by the model from hostile messages: user text, rules, skills,
/// MCP namespaces, tool names, tool results, and an unknown part type.
fn model_tree() -> Tree {
    // Attribute values cannot contain `"`, so the DCS payload goes in bodies.
    let user = format!(
        "<user_query>hi {}</user_query><rules><rule name=\"/r/{OSC52}{CSI8}.mdc\">rule {DCS}</rule></rules>\
         <agent_skills><agent_skill name=\"{OSC52}\">s {DCS}</agent_skill><agent_skill>\n{CSI8} first\n</agent_skill></agent_skills>\
         <dynamic_tool_catalog><namespace name=\"{CSI8}\">ns {DCS}</namespace></dynamic_tool_catalog>",
        all()
    );
    let blobs = [
        ("s", json!({"role": "system", "content": all()})),
        ("u", json!({"role": "user", "content": user})),
        (
            "a",
            json!({"role": "assistant", "content": [
                {"type": "reasoning", "text": all()},
                {"type": "text", "text": all()},
                {"type": "tool-call", "toolName": all(), "input": {"q": all()}},
                {"type": "tool-result", "toolName": all(), "output": all()},
                {"type": all()},
            ]}),
        ),
    ];
    let store = temp_store("model", &blobs);
    let ids: Vec<String> = ["s", "u", "a"].iter().map(|s| s.to_string()).collect();
    build_tree(&store, &ids, &[]).unwrap()
}

/// Tree whose nodes carry the raw payloads, bypassing the model's filtering.
fn raw_tree() -> Tree {
    let node = |id: &str, parent: Option<&str>, cat: &str, tokens: u64| Node {
        id: id.into(),
        parent_id: parent.map(str::to_string),
        kind: format!("item{CSI8}"),
        label: format!("{id} {}\nline two", all()),
        category_id: cat.into(),
        estimated_tokens: tokens,
        character_count: tokens * 4,
        local_estimate: Some(1),
        text: Some(format!("body {}\nsecond {}\r\nthird\u{2028}x", all(), all())),
    };
    Tree {
        nodes: vec![
            node("conv", None, "conversation", 100),
            node("t1", Some("conv"), "conversation", 60),
            node("m1", Some("t1"), "conversation", 60),
            node(&format!("odd{CSI8}"), None, &format!("cat{OSC52}"), 40),
            node("leaf", Some(&format!("odd{CSI8}")), &format!("cat{OSC52}"), 40),
        ],
        ..Tree::default()
    }
}

fn hostile_snapshot(tree: Tree) -> Snapshot {
    let hostile_id = format!("id{CSI8}{OSC52}");
    Snapshot {
        store: PathBuf::from(format!("/tmp/{OSC52}/store.db")),
        agent_id: Some(format!("agent{CSI8}")),
        name: Some(format!("chat {}", all())),
        root_blob_id: Some("r".into()),
        used: 150_000,
        max: 200_000,
        percent: Some(75.0),
        breakdown: vec![
            BreakdownEntry { id: "conversation".into(), label: format!("Conv {}", all()), estimated_tokens: 100_000 },
            BreakdownEntry { id: hostile_id.clone(), label: all(), estimated_tokens: 50_000 },
        ],
        message_ids: Vec::new(),
        created_at: None,
        tree,
        timeline: Some(Timeline {
            version: 1,
            store_path: None,
            store_created_at: None,
            last_rowid: 2,
            checkpoints: (1..=3)
                .map(|i| Checkpoint {
                    rowid: i,
                    root_id: format!("root{i}"),
                    message_count: i as usize,
                    used_tokens: 50_000 * i as u64,
                    max_tokens: 200_000,
                    categories: vec![(hostile_id.clone(), 40_000 * i as u64), ("conversation".into(), 10_000)],
                    compaction: false,
                })
                .collect(),
            last_notified_band: None,
        }),
        meta: MetaRow {
            name: Some(all()),
            mode: Some(all()),
            last_used_model: Some(format!("model {}", all())),
            ..Default::default()
        },
    }
}

/// Every cell of `term`'s buffer, failing on the first dangerous char.
fn assert_buffer_clean(term: &Terminal<TestBackend>, ctx: &str) {
    let buf = term.backend().buffer();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let sym = buf[(x, y)].symbol();
            assert!(!sym.chars().any(is_dangerous), "{ctx}: cell ({x},{y}) holds {sym:?}");
        }
    }
}

fn buffer_text(term: &Terminal<TestBackend>) -> String {
    let buf = term.backend().buffer();
    (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect()
}

/// Draw the full TUI once per node (each selected in turn, so the detail
/// pane shows every node's text) and check every cell.
fn draw_every_node(snap: Snapshot, ctx: &str) {
    let mut app = App::new();
    let ids: Vec<String> = snap.tree.nodes.iter().map(|n| n.id.clone()).collect();
    app.set_snapshot(snap);
    app.expanded.extend(ids.iter().cloned());
    app.set_snapshot(app.snapshot.clone().unwrap());
    app.status = Some(format!("opened {}", all()));
    let mut term = Terminal::new(TestBackend::new(180, 48)).unwrap();
    let mut saw_replacement = false;
    for id in &ids {
        app.selected = Some(id.clone());
        term.draw(|f| ctxviz::tui::draw(f, &app)).unwrap();
        assert_buffer_clean(&term, &format!("{ctx}, node {id:?}"));
        saw_replacement |= buffer_text(&term).contains(REPLACEMENT);
    }
    // The payloads were on screen, filtered, not merely scrolled out of view.
    assert!(saw_replacement, "{ctx}: no U+FFFD drawn\n{}", buffer_text(&term));
}

#[test]
fn model_tree_labels_and_text_are_clean() {
    let tree = model_tree();
    let mut replaced = 0;
    for n in &tree.nodes {
        assert!(is_clean(&n.label) && !n.label.contains(['\n', '\t']), "label {:?}", n.label);
        if let Some(t) = &n.text {
            assert!(is_clean(t), "text of {}: {t:?}", n.id);
            replaced += t.matches(REPLACEMENT).count();
        }
        replaced += n.label.matches(REPLACEMENT).count();
    }
    assert!(replaced > 10, "payloads missing from the tree: {:#?}", tree.nodes);
    // JSON output inherits the model's filtering (serde_json escapes C0 but
    // would pass C1 and DEL through verbatim).
    let json = ctxviz::render::json::to_string(&hostile_snapshot(tree)).unwrap();
    let tree_json = &json[json.find("\"tree\"").unwrap()..json.find("\"timeline\"").unwrap()];
    assert!(is_clean(tree_json), "{tree_json}");
}

#[test]
fn tui_panels_render_model_tree_clean() {
    draw_every_node(hostile_snapshot(model_tree()), "model tree");
}

#[test]
fn tui_panels_render_raw_tree_clean() {
    // Sink defense: nodes that never went through the model.
    draw_every_node(hostile_snapshot(raw_tree()), "raw tree");
}

#[test]
fn tui_narrow_layouts_render_clean() {
    // Small panels take other code paths (sparkline, stacked timeline).
    for (w, h) in [(40, 14), (70, 24), (100, 30)] {
        let mut app = App::new();
        app.set_snapshot(hostile_snapshot(raw_tree()));
        app.selected = Some("conv".into());
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ctxviz::tui::draw(f, &app)).unwrap();
        assert_buffer_clean(&term, &format!("{w}x{h}"));
    }
}

#[test]
fn snapshot_text_output_is_clean() {
    for (ctx, tree) in [("model tree", model_tree()), ("raw tree", raw_tree())] {
        let snap = hostile_snapshot(tree);
        // The plain theme writes no escapes of its own, so any control char
        // in the output would have come from the store.
        let out = render_text(&snap, &Theme::plain()) + &render_tree(&snap, 10);
        assert!(is_clean(&out.replace('\n', "")), "{ctx}: {out:?}");
        assert!(out.contains(REPLACEMENT), "{ctx}: payloads missing: {out}");
        assert!(out.contains("Context Usage · chat \u{FFFD}]52;c;QUJD\u{FFFD}"), "{ctx}: {out}");
    }
}

#[test]
fn snapshot_colored_output_has_only_sgr_escapes() {
    let snap = hostile_snapshot(raw_tree());
    let out = render_text(&snap, &Theme::ansi()) + &render_tree(&snap, 10);
    // Strip the renderer's own SGR sequences; nothing else may remain.
    let stripped = ctxviz::render::statusline::strip_ansi(&out);
    assert!(is_clean(&stripped.replace('\n', "")), "{stripped:?}");
    for esc in out.split('\x1b').skip(1) {
        assert!(esc.starts_with('['), "non-CSI escape {esc:?}");
        let end = esc.find(|c: char| c.is_ascii_alphabetic()).unwrap();
        assert_eq!(&esc[end..=end], "m", "non-SGR escape {esc:?}");
    }
}
