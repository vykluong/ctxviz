//! `ctxviz doctor` exit codes, run as a child process under a bare env: `HOME`
//! is a temp dir and the variables a developer shell may export are removed,
//! so no test here touches this process's env.

mod common;

use common::{root_blob, Details, StoreBuilder, TempDir};
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ctxviz");

/// `ctxviz doctor --latest` with `HOME=home` and the dir variables removed.
fn doctor(home: &Path, kitty: Option<&str>) -> (i32, String) {
    let mut cmd = Command::new(BIN);
    cmd.args(["doctor", "--latest"]).env("HOME", home).current_dir(home);
    for k in ["CTXVIZ_CURSOR_DIR", "CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME", "CTXVIZ_STATE_DIR", "KITTY_LISTEN_ON"] {
        cmd.env_remove(k);
    }
    if let Some(k) = kitty {
        cmd.env("KITTY_LISTEN_ON", k);
    }
    let Output { status, stdout, stderr } = cmd.output().unwrap();
    let text = String::from_utf8(stdout).unwrap();
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    assert!(ctxviz::text::is_clean(&text), "{text:?}");
    (status.code().unwrap(), text)
}

/// A store that passes every store check, at `<cursor dir>/chats/proj/agent-1/store.db`.
fn make_store(cursor_dir: &Path) {
    let dir = cursor_dir.join("chats/proj/agent-1");
    std::fs::create_dir_all(&dir).unwrap();
    let b = StoreBuilder::create(&dir.join("store.db"));
    let details = Details { used: 1000, max: 200_000, categories: &[("tools", "Tool definitions", 1000)] };
    b.blob("r1", &root_blob(&[[0xaa; 32]], 1, Some(&details), 1_700_000_000_000));
    b.json(&common::hex(&[0xaa; 32]), &serde_json::json!({ "role": "user", "content": "hi" }));
    b.meta(&serde_json::json!({ "agentId": "agent-1", "latestRootBlobId": "r1", "name": "t" }));
    b.finish();
}

#[test]
fn doctor_exit_codes() {
    let root = TempDir::new("doctor-exit");
    let home = root.join("home");
    let cursor = home.join(".cursor");
    std::fs::create_dir_all(&cursor).unwrap();

    // No cli-config.json: a WARN naming the status line, exit 1.
    let (code, text) = doctor(&home, None);
    assert_eq!(code, 1, "{text}");
    assert!(text.lines().any(|l| l.starts_with("WARN:") && l.contains("statusLine")), "{text}");
    std::fs::write(cursor.join("cli-config.json"), br#"{"version": 1}"#).unwrap();
    let (code, text) = doctor(&home, None);
    assert_eq!(code, 1, "{text}");
    assert!(text.lines().any(|l| l.starts_with("WARN:") && l.contains("statusLine")), "{text}");

    // Wired to this binary, with a store: only INFO lines (not in Kitty, no
    // current.json yet), exit 0.
    let config = serde_json::json!({
        "version": 1,
        "statusLine": { "type": "command", "command": format!("{BIN} statusline"), "padding": 2 }
    });
    std::fs::write(cursor.join("cli-config.json"), config.to_string()).unwrap();
    make_store(&cursor);
    let (code, text) = doctor(&home, None);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("WARN:"), "{text}");
    assert!(text.contains("INFO: not in Kitty (ctxviz open unavailable)"), "{text}");
    assert!(text.contains("runs this binary"), "{text}");
    assert!(text.contains("structural root test: pass"), "{text}");

    let (code, text) = doctor(&home, Some("unix:/nonexistent/kitty"));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("OK: KITTY_LISTEN_ON set"), "{text}");
}

#[test]
fn doctor_without_a_store_is_info() {
    let root = TempDir::new("doctor-no-store");
    let home = root.join("home");
    let cursor = home.join(".cursor");
    std::fs::create_dir_all(cursor.join("chats")).unwrap();
    let config = serde_json::json!({ "statusLine": { "type": "command", "command": format!("{BIN} statusline") } });
    std::fs::write(cursor.join("cli-config.json"), config.to_string()).unwrap();
    let (code, text) = doctor(&home, None);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("INFO: no chat has a checkpoint yet"), "{text}");
    assert!(text.contains("INFO: no store: no cursor-agent chats in"), "{text}");
}
