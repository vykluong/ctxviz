//! Hardening against crafted chat stores and hostile state: path traversal via
//! `meta.agentId`, views posing as tables, deep protobuf groups, random bytes,
//! state file permissions, and SQLite sidecar behavior.
//!
//! `CTXVIZ_STATE_DIR`, `HOME`, `CTXVIZ_CURSOR_DIR`, `CURSOR_CONFIG_DIR` and
//! `XDG_CONFIG_HOME` are process-global, so every test that reads or sets them
//! holds `ENV_LOCK` and uses its own unique temp dir. Tests that rely on
//! `HOME` clear the cursor dir variables with [`home_only`] (or start from
//! [`bare_env`]), since a developer shell may export them. Child processes
//! get the same variables removed.

mod common;

use common::{StoreBuilder, TempDir};
use ctxviz::model::snapshot::{Snapshot, SnapshotOptions};
use ctxviz::model::timeline::{self, Timeline};
use ctxviz::pb::{self, encode};
use ctxviz::session::{self, CurrentSession};
use ctxviz::store::{self, Store};
use rusqlite::Connection;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Restores an env var on drop.
struct EnvVar(&'static str, Option<std::ffi::OsString>);

impl EnvVar {
    fn set(key: &'static str, val: Option<&Path>) -> EnvVar {
        let old = std::env::var_os(key);
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        EnvVar(key, old)
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match &self.1 {
            Some(v) => std::env::set_var(self.0, v),
            None => std::env::remove_var(self.0),
        }
    }
}

/// The cursor dir variables a developer shell may export.
const CURSOR_DIR_VARS: [&str; 3] = ["CTXVIZ_CURSOR_DIR", "CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"];

/// Clears `CTXVIZ_CURSOR_DIR`, `CURSOR_CONFIG_DIR` and `XDG_CONFIG_HOME` so
/// `HOME` alone picks the cursor dir. Hold the guards for the whole test.
fn home_only() -> [EnvVar; 3] {
    CURSOR_DIR_VARS.map(|k| EnvVar::set(k, None))
}

/// `HOME` set to `home`, the cursor dir variables and `CTXVIZ_STATE_DIR`
/// cleared: the default layout under a temp home. Hold the guards for the
/// whole test.
fn bare_env(home: &Path) -> [EnvVar; 5] {
    let [a, b, c] = home_only();
    [EnvVar::set("HOME", Some(home)), EnvVar::set("CTXVIZ_STATE_DIR", None), a, b, c]
}

/// Root blob with one message id, token details, and createdAt.
fn root_blob(used: u64) -> Vec<u8> {
    let mut td = Vec::new();
    encode::uint(1, used, &mut td);
    encode::uint(2, 200_000, &mut td);
    let mut b = Vec::new();
    encode::bytes(1, &[0xaa; 32], &mut b);
    encode::bytes(5, &td, &mut b);
    encode::uint(26, 1_700_000_000_000, &mut b);
    b
}

/// A minimal store at `<dir>/store.db` whose meta claims `agent_id`.
fn make_store(dir: &Path, agent_id: &str) -> PathBuf {
    let b = StoreBuilder::create(&dir.join("store.db"));
    for (id, used) in [("r1", 1000u64), ("r2", 2000)] {
        b.blob(id, &root_blob(used));
    }
    b.meta(&serde_json::json!({ "agentId": agent_id, "latestRootBlobId": "r2", "name": "t" }));
    b.finish()
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn hostile_agent_id_cache_stays_in_state_dir() {
    let _g = env_lock();
    let root = TempDir::new("hardening-traversal");
    // state dir two levels down so `../../x` would land inside `root`.
    let state = root.0.join("a/b/state");
    let _env = EnvVar::set("CTXVIZ_STATE_DIR", Some(&state));
    let abs_target = root.0.join("abs-target");
    let ids = [
        "../../evil".to_string(),
        abs_target.to_string_lossy().into_owned(),
        "..".to_string(),
        ".hidden".to_string(),
        "a/b".to_string(),
    ];
    for (n, id) in ids.iter().enumerate() {
        let chat = root.0.join(format!("chat{n}"));
        std::fs::create_dir_all(&chat).unwrap();
        let store = Store::open(make_store(&chat, id)).unwrap();
        assert_eq!(store.meta().unwrap().agent_id.as_deref(), Some(id.as_str()));

        let cache = timeline::cache_path(id);
        assert_eq!(cache.parent(), Some(state.as_path()), "{id:?} -> {}", cache.display());
        let name = cache.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("invalid-") && name.ends_with(".json"), "{name}");

        // End to end through the snapshot loader, which reads meta.agentId.
        let snap = Snapshot::load(&store, SnapshotOptions { tree: false, timeline: true }).unwrap().unwrap();
        assert_eq!(snap.timeline.as_ref().unwrap().checkpoints.len(), 2);
        assert!(cache.is_file(), "cache saved under hashed name for {id:?}");
        assert_eq!(Timeline::load_or_scan(&store, id).unwrap().checkpoints.len(), 2);
    }
    assert!(!root.0.join("evil.json").exists());
    assert!(!root.0.join("a/evil.json").exists());
    assert!(!root.0.join("abs-target.json").exists());
    // Nothing besides the stores was written outside the state dir.
    for f in files_under(&root.0) {
        let in_chat = f.parent().and_then(Path::file_name).is_some_and(|n| n.to_string_lossy().starts_with("chat"));
        assert!(f.starts_with(&state) || in_chat, "unexpected file {}", f.display());
    }
    // Valid ids keep their readable name.
    assert_eq!(timeline::cache_path("abc-123"), state.join("abc-123.json"));
}

#[test]
fn invalid_agent_ids_are_refused() {
    let _g = env_lock();
    let home = TempDir::new("hardening-home");
    let _env = EnvVar::set("HOME", Some(&home.0));
    let _only = home_only();
    // A real store reachable by traversal must still not be found.
    let target = home.0.join(".cursor/x");
    std::fs::create_dir_all(&target).unwrap();
    make_store(&target, "x");
    std::fs::create_dir_all(home.0.join(".cursor/chats/proj")).unwrap();
    for bad in ["../x", "../../.cursor/x", "/abs", "..", ""] {
        assert_eq!(store::store_for_agent(bad), None, "{bad:?}");
        assert!(store::try_store_for_agent(bad).is_err(), "{bad:?}");
        assert!(store::find_store(Some(bad), Path::new("/"), false).is_err(), "{bad:?}");
    }
    assert_eq!(store::try_store_for_agent("missing").unwrap(), None);
}

#[test]
fn view_posing_as_table_is_rejected() {
    let dir = TempDir::new("hardening-view");
    let path = dir.0.join("store.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "create table real_blobs(id text, data blob); create view blobs as select * from real_blobs; \
         create table meta(key text primary key, value text);",
    )
    .unwrap();
    drop(c);
    let err = Store::open(&path).err().expect("view must be rejected").to_string();
    assert!(err.contains("view"), "{err}");

    let path2 = dir.0.join("nometa.db");
    Connection::open(&path2).unwrap().execute_batch("create table blobs(id text, data blob);").unwrap();
    assert!(Store::open(&path2).is_err(), "missing meta table");
}

#[test]
fn connection_is_query_only_and_defensive() {
    let dir = TempDir::new("hardening-pragmas");
    let store = Store::open(make_store(&dir.0, "ok")).unwrap();
    let c = store.conn();
    let q: i64 = c.query_row("pragma query_only", [], |r| r.get(0)).unwrap();
    let t: i64 = c.query_row("pragma trusted_schema", [], |r| r.get(0)).unwrap();
    assert_eq!((q, t), (1, 0));
    assert!(c.db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
    assert!(c.execute("delete from blobs", []).is_err());
}

#[test]
fn oversized_meta_is_an_error_not_a_load() {
    let dir = TempDir::new("hardening-bigmeta");
    let path = make_store(&dir.0, "ok");
    let c = Connection::open(&path).unwrap();
    // zeroblob keeps the test cheap: 17 MiB of text-typed zeros.
    c.execute("update meta set value = cast(zeroblob(17*1024*1024) as text) where key='0'", []).unwrap();
    drop(c);
    let store = Store::open(&path).unwrap();
    assert!(store.meta().is_err());
}

#[test]
fn readonly_open_sidecars() {
    // Documents the behavior described on `Store::open`.
    let dir = TempDir::new("hardening-shm");
    let path = make_store(&dir.0, "ok");
    let c = Connection::open(&path).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(c);
    let sidecar = |s: &str| dir.0.join(format!("store.db{s}"));
    assert!(!sidecar("-wal").exists() && !sidecar("-shm").exists(), "writer closed cleanly");
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.max_rowid().unwrap(), 2);
    }
    assert!(sidecar("-shm").exists(), "read-only open creates -shm");
    assert_eq!(std::fs::metadata(sidecar("-wal")).unwrap().len(), 0, "and an empty -wal");
}

#[test]
fn state_files_are_private_and_capped() {
    let _g = env_lock();
    let root = TempDir::new("hardening-state");
    let state = root.0.join("new/state");
    let _env = EnvVar::set("CTXVIZ_STATE_DIR", Some(&state));
    assert_eq!(session::state_dir(), state);
    assert_eq!(timeline::state_dir(), state, "one state dir");
    assert_eq!(session::current_path(), state.join("current.json"));

    let cur = CurrentSession { session_id: "abc".into(), ts: 1, ..Default::default() };
    cur.write().unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&state), 0o700);
    assert_eq!(mode(&state.join("current.json")), 0o600);
    assert_eq!(CurrentSession::read(), Some(cur.clone()));
    // Rewrites leave no temp files behind.
    cur.write().unwrap();
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), 1);

    // Invalid session ids are neither written nor read back.
    let bad = CurrentSession { session_id: "../x".into(), ..Default::default() };
    assert!(bad.write().is_err());
    std::fs::write(session::current_path(), serde_json::to_vec(&bad).unwrap()).unwrap();
    assert_eq!(CurrentSession::read(), None);

    // Reads over 1 MiB are refused.
    let big = state.join("big.json");
    let mut json = serde_json::to_vec(&cur).unwrap();
    json.pop();
    json.extend(std::iter::repeat_n(b' ', 1024 * 1024));
    json.push(b'}');
    std::fs::write(&big, &json).unwrap();
    assert_eq!(CurrentSession::read_from(&big), None);
    assert!(Timeline::load(&big).is_none());
    assert!(session::read_capped(&big).is_none());
}

#[test]
fn current_json_write_leaves_no_tmp() {
    // `ctxviz tui` needs a terminal, so this calls the sweep it runs on start.
    let _g = env_lock();
    let root = TempDir::new("hardening-current-tmp");
    let state = root.0.join("state");
    let _env = EnvVar::set("CTXVIZ_STATE_DIR", Some(&state));
    let tmp_files = || -> Vec<String> {
        let names = std::fs::read_dir(&state).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned());
        names.filter(|n| n.starts_with(".current.json.tmp.")).collect()
    };
    let cur = CurrentSession { session_id: "abc".into(), ts: 1, ..Default::default() };
    for _ in 0..3 {
        cur.write().unwrap();
    }
    assert_eq!(tmp_files(), Vec::<String>::new());
    assert_eq!(CurrentSession::read(), Some(cur));

    // Leftovers of a writer killed before its rename: only the stale one goes.
    let stale = state.join(".current.json.tmp.1.2.0");
    let fresh = state.join(".current.json.tmp.1.3.1");
    for p in [&stale, &fresh] {
        std::fs::write(p, "{}").unwrap();
    }
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 + 60);
    std::fs::File::options().write(true).open(&stale).unwrap().set_modified(past).unwrap();
    assert_eq!(session::sweep_stale_tmp_files(), 1);
    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(state.join("current.json").exists());
}

#[test]
fn missing_home_is_an_error() {
    let _g = env_lock();
    let _only = home_only();
    {
        let _home = EnvVar::set("HOME", None);
        let _state = EnvVar::set("CTXVIZ_STATE_DIR", None);
        assert!(store::home().is_err());
        assert!(store::try_chats_dir().is_err());
        assert!(session::try_state_dir().is_err());
        assert!(!store::chats_dir().starts_with("/.cursor"));
        assert!(CurrentSession { session_id: "abc".into(), ..Default::default() }.write().is_err());
        assert!(store::find_store(None, Path::new("/"), true).is_err());
    }
    {
        let _home = EnvVar::set("HOME", Some(Path::new("relative/home")));
        assert!(store::home().is_err());
    }
}

#[test]
fn cursor_config_dir_overrides_home() {
    let _g = env_lock();
    let root = TempDir::new("hardening-cursor-dir");
    let cfg = root.0.join("cfg");
    let _state = EnvVar::set("CTXVIZ_STATE_DIR", None);
    let _ctxviz = EnvVar::set("CTXVIZ_CURSOR_DIR", None);
    let _xdg = EnvVar::set("XDG_CONFIG_HOME", Some(&root.0.join("xdg")));
    {
        // CURSOR_CONFIG_DIR wins over XDG_CONFIG_HOME and needs no HOME.
        let _home = EnvVar::set("HOME", None);
        let _cfg = EnvVar::set("CURSOR_CONFIG_DIR", Some(&cfg));
        assert_eq!(store::try_chats_dir().unwrap(), cfg.join("chats"));
        assert_eq!(session::try_state_dir().unwrap(), cfg.join("ctxviz"));
        let store_dir = cfg.join("chats/proj/agent-1");
        std::fs::create_dir_all(&store_dir).unwrap();
        let path = make_store(&store_dir, "agent-1");
        assert_eq!(store::store_for_agent("agent-1"), Some(path.clone()));
        assert_eq!(store::find_store(None, Path::new("/"), true).unwrap(), path);
    }
    {
        let _cfg = EnvVar::set("CURSOR_CONFIG_DIR", None);
        assert_eq!(store::try_chats_dir().unwrap(), root.0.join("xdg/cursor/chats"));
        assert_eq!(session::try_state_dir().unwrap(), root.0.join("xdg/cursor/ctxviz"));
    }
    {
        // A relative override is an error, never a lookup relative to the cwd.
        let _cfg = EnvVar::set("CURSOR_CONFIG_DIR", Some(Path::new("rel/cfg")));
        let err = format!("{:#}", store::try_chats_dir().unwrap_err());
        assert!(err.contains("not an absolute path"), "{err}");
        assert!(session::try_state_dir().is_err());
        assert!(CurrentSession { session_id: "abc".into(), ..Default::default() }.write().is_err());
        assert!(!Path::new("rel").exists());
    }
}

#[test]
fn missing_home_messages_hide_placeholder() {
    // `ctxviz open` without HOME (and no CTXVIZ_STATE_DIR) names the cause, not
    // the internal placeholder path. A fake KITTY_LISTEN_ON gets past
    // `kitty::detect`; the run fails before any kitten call.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ctxviz"))
        .arg("open")
        .env_remove("HOME")
        .env_remove("CTXVIZ_STATE_DIR")
        .env_remove("CTXVIZ_CURSOR_DIR")
        .env_remove("CURSOR_CONFIG_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env("KITTY_LISTEN_ON", "unix:/nonexistent/ctxviz-test.sock")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("HOME is not set"), "{stderr}");
    assert!(!stderr.contains(store::NO_HOME), "{stderr}");

    let cache = PathBuf::from(store::NO_HOME).join(".cursor/ctxviz/abc.json");
    let err = format!("{:#}", Timeline::default().save(&cache).unwrap_err());
    assert!(err.contains("HOME is not set"), "{err}");
    assert!(!err.contains(store::NO_HOME), "{err}");
}

#[test]
fn statusline_renders_payload_row_for_corrupt_root() {
    // The latest root is 1000 bytes of 0x0b (start-group tags nested far past
    // the depth limit). The status line logs the decode error and still
    // prints the payload-only row with exit 0.
    let root = TempDir::new("hardening-statusline-corrupt");
    let home = root.0.join("home");
    let cwd = root.0.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let sid = "corrupt-root-session";
    let store_dir = home.join(".cursor/chats").join(store::project_hash(&cwd)).join(sid);
    std::fs::create_dir_all(&store_dir).unwrap();
    let b = StoreBuilder::create(&store_dir.join("store.db"));
    b.blob("r1", &[0x0b; 1000]);
    b.meta(&serde_json::json!({ "agentId": sid, "latestRootBlobId": "r1", "name": "t" }));
    b.finish();

    let payload = serde_json::json!({
        "session_id": sid,
        "cwd": cwd,
        "context_window": { "used_percentage": 42, "context_window_size": 200000 },
    });
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ctxviz"))
        .args(["statusline", "--theme", "none"])
        .env("HOME", &home)
        .env("CTXVIZ_STATE_DIR", root.0.join("state"))
        .env_remove("CTXVIZ_CURSOR_DIR")
        .env_remove("CURSOR_CONFIG_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env("CTXVIZ_NO_GIT", "1")
        .env_remove("KITTY_LISTEN_ON")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stdout.trim().is_empty(), "blank row; stderr: {stderr}");
    assert!(stdout.contains("84K/200K"), "payload usage missing: {stdout}");
    assert!(stderr.contains("decoding root token details"), "{stderr}");
}

/// Run `ctxviz statusline` on `payload` with `env` (and a bare environment
/// for the cursor dir variables); returns exit code, stdout, stderr.
fn run_statusline(payload: &serde_json::Value, env: &[(&str, Option<&Path>)]) -> (Option<i32>, String, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ctxviz"));
    cmd.args(["statusline", "--theme", "none", "--rows", "1"]).env("CTXVIZ_NO_GIT", "1").env_remove("KITTY_LISTEN_ON");
    for key in CURSOR_DIR_VARS.into_iter().chain(["CTXVIZ_STATE_DIR"]) {
        cmd.env_remove(key);
    }
    for (k, v) in env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn statusline_follows_relocated_cursor_dir() {
    // The Cursor CLI keeps chats in $CURSOR_CONFIG_DIR (or
    // $XDG_CONFIG_HOME/cursor) when set. The status line must read the store
    // there, not fall back to the payload-only row, and keep its state there
    // without creating ~/.cursor.
    for var in ["CURSOR_CONFIG_DIR", "XDG_CONFIG_HOME"] {
        let root = TempDir::new("hardening-statusline-relocated");
        let home = root.0.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (env_value, cursor_dir) = match var {
            "CURSOR_CONFIG_DIR" => (root.0.join("cfg"), root.0.join("cfg")),
            _ => (root.0.join("xdg"), root.0.join("xdg/cursor")),
        };
        let cwd = root.0.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let sid = "relocated-session";
        let store_dir = cursor_dir.join("chats").join(store::project_hash(&cwd)).join(sid);
        std::fs::create_dir_all(&store_dir).unwrap();
        make_store(&store_dir, sid);

        let payload = serde_json::json!({
            "session_id": sid,
            "cwd": cwd,
            "context_window": { "used_percentage": 42, "context_window_size": 200000 },
        });
        let (code, stdout, stderr) = run_statusline(&payload, &[("HOME", Some(&home)), (var, Some(&env_value))]);
        assert_eq!(code, Some(0), "{var}: stderr: {stderr}");
        // The store's root (2000 of 200000 tokens) wins over the payload's 42%.
        assert!(stdout.contains("1%"), "{var}: store not read: {stdout}");
        assert!(!stdout.contains("84K"), "{var}: payload-only row: {stdout}");
        let current = CurrentSession::read_from(&cursor_dir.join("ctxviz/current.json")).expect("current.json");
        assert_eq!(current.store.as_deref(), Some(store_dir.join("store.db").as_path()));
        assert!(!home.join(".cursor").exists(), "{var}: created ~/.cursor");
    }
}

/// A store for `agent_id` at `<cursor_dir>/chats/<project>/<agent_id>/store.db`.
fn make_store_in(cursor_dir: &Path, project: &str, agent_id: &str) -> PathBuf {
    let dir = cursor_dir.join("chats").join(project).join(agent_id);
    std::fs::create_dir_all(&dir).unwrap();
    make_store(&dir, agent_id)
}

#[test]
fn ctxviz_cursor_dir_overrides_cursor_config_dir() {
    let _g = env_lock();
    let root = TempDir::new("hardening-ctxviz-cursor-dir");
    let _env = bare_env(&root.0.join("home"));
    let (a, b) = (root.0.join("a"), root.0.join("b"));
    let _cfg = EnvVar::set("CURSOR_CONFIG_DIR", Some(&b));
    let in_a = make_store_in(&a, "proj", "agent-1");
    let in_b = make_store_in(&b, "proj", "agent-1");
    {
        let _ctxviz = EnvVar::set("CTXVIZ_CURSOR_DIR", Some(&a));
        assert_eq!(store::try_cursor_dir_with_source().unwrap(), (a.clone(), store::CursorDirSource::CtxvizEnv));
        assert_eq!(store::try_chats_dir().unwrap(), a.join("chats"));
        assert_eq!(session::try_state_dir().unwrap(), a.join("ctxviz"));
        assert_eq!(store::store_for_agent("agent-1"), Some(in_a.clone()));
        assert_eq!(store::find_store(None, Path::new("/"), true).unwrap(), in_a);
        let err = format!("{:#}", store::find_store(None, Path::new("/elsewhere"), false).unwrap_err());
        assert!(err.contains("set CTXVIZ_CURSOR_DIR if the agent uses another config dir"), "{err}");
    }
    {
        // Blank counts as unset.
        let _ctxviz = EnvVar::set("CTXVIZ_CURSOR_DIR", Some(Path::new(" ")));
        assert_eq!(store::try_cursor_dir_with_source().unwrap(), (b.clone(), store::CursorDirSource::ConfigDirEnv));
        assert_eq!(session::try_state_dir().unwrap(), b.join("ctxviz"));
        assert_eq!(store::store_for_agent("agent-1"), Some(in_b.clone()));
        assert_eq!(store::find_store(None, Path::new("/"), true).unwrap(), in_b);
    }
    {
        // A relative value is an error naming the variable, not a fallback.
        let _ctxviz = EnvVar::set("CTXVIZ_CURSOR_DIR", Some(Path::new("rel/a")));
        let err = format!("{:#}", store::try_chats_dir().unwrap_err());
        assert!(err.contains("$CTXVIZ_CURSOR_DIR is not an absolute path"), "{err}");
        assert!(session::try_state_dir().is_err());
        assert!(store::try_store_for_agent("agent-1").is_err());
        assert!(store::find_store(None, Path::new("/"), true).is_err());
        assert!(CurrentSession { session_id: "abc".into(), ..Default::default() }.write().is_err());
        assert!(!Path::new("rel").exists());
        // The candidate list skips it and keeps the others.
        let dirs: Vec<PathBuf> = store::candidate_cursor_dirs_from_env().into_iter().map(|(d, _)| d).collect();
        assert_eq!(dirs, vec![b.clone(), root.0.join("home/.cursor")]);
    }
}

#[test]
fn statusline_finds_store_in_other_candidate_dir() {
    // The agent runs with CURSOR_CONFIG_DIR=B while CTXVIZ_CURSOR_DIR=A: the
    // status line still finds the session's store under B (by the direct path,
    // and by the exact-id scan when the cwd hash does not match) and records
    // it in A's state dir.
    let root = TempDir::new("hardening-statusline-candidates");
    let home = root.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (a, b) = (root.0.join("a"), root.0.join("b"));
    let cwd = root.0.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let direct = make_store_in(&b, &store::project_hash(&cwd), "direct-session");
    let scanned = make_store_in(&b, "other-project", "scanned-session");
    for (sid, expected) in [("direct-session", &direct), ("scanned-session", &scanned)] {
        let payload = serde_json::json!({
            "session_id": sid,
            "cwd": cwd,
            "context_window": { "used_percentage": 42, "context_window_size": 200000 },
        });
        let env = [("HOME", Some(home.as_path())), ("CTXVIZ_CURSOR_DIR", Some(&a)), ("CURSOR_CONFIG_DIR", Some(&b))];
        let (code, stdout, stderr) = run_statusline(&payload, &env);
        assert_eq!(code, Some(0), "{sid}: stderr: {stderr}");
        assert!(stdout.contains("1%"), "{sid}: store not read: {stdout}");
        assert!(!stdout.contains("84K"), "{sid}: payload-only row: {stdout}");
        let current = CurrentSession::read_from(&a.join("ctxviz/current.json")).expect("current.json under A");
        assert_eq!(current.session_id, sid);
        assert_eq!(current.store.as_ref(), Some(expected));
        assert_eq!(current.cursor_dir.as_ref(), Some(&b));
        assert_eq!(current.ctxviz_version.as_deref(), Some(ctxviz::build_info::VERSION));
        assert!(!b.join("ctxviz").exists(), "{sid}: state written under B");
        assert!(!home.join(".cursor").exists(), "{sid}: created ~/.cursor");
    }
}

#[test]
fn followed_store_follows_override() {
    // An unpinned TUI started from a shell whose CURSOR_CONFIG_DIR is B
    // follows A's current.json, and without one, A's newest store.
    let _g = env_lock();
    let root = TempDir::new("hardening-followed-store");
    let _env = bare_env(&root.0.join("home"));
    let (a, b) = (root.0.join("a"), root.0.join("b"));
    let _cfg = EnvVar::set("CURSOR_CONFIG_DIR", Some(&b));
    let _ctxviz = EnvVar::set("CTXVIZ_CURSOR_DIR", Some(&a));
    let in_a = make_store_in(&a, "proj", "agent-a");
    let in_b = make_store_in(&b, "proj", "agent-b");
    assert_eq!(ctxviz::tui::watch::followed_store(), Some(in_a.clone()));

    let record = |dir: &Path, sid: &str, store: &Path| {
        let cur =
            CurrentSession { session_id: sid.into(), store: Some(store.to_path_buf()), ts: 1, ..Default::default() };
        cur.write_to(&dir.join("ctxviz/current.json")).unwrap();
    };
    let other_a = make_store_in(&a, "proj", "agent-a2");
    record(&a, "agent-a2", &other_a);
    record(&b, "agent-b", &in_b);
    assert_eq!(ctxviz::tui::watch::followed_store(), Some(other_a));
}

#[test]
fn deep_groups_fail_fast() {
    let blob = vec![0x0bu8; 300_000];
    let start = std::time::Instant::now();
    assert!(pb::decode(&blob).is_err());
    assert!(pb::find_first(&blob, 1).is_err());
    assert!(!ctxviz::schema::is_root(&blob));
    assert!(ctxviz::model::breakdown::decode_root(&blob).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1), "{:?}", start.elapsed());
}

#[test]
fn random_bytes_never_panic() {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..2000 {
        let len = (next() % 512) as usize;
        let mut blob: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        // Bias some blobs toward group and length markers.
        if next() % 4 == 0 {
            for b in blob.iter_mut() {
                let r = next();
                if r % 3 == 0 {
                    *b = [0x0b, 0x0c, 0x0a, 0x08][((r >> 8) % 4) as usize];
                }
            }
        }
        let _ = pb::decode(&blob);
        let _ = pb::group(&blob);
        let _ = pb::find_first(&blob, 5);
        let _ = pb::find_all(&blob, 1);
        let _ = ctxviz::schema::is_root(&blob);
        let _ = ctxviz::model::breakdown::decode_root(&blob);
    }
}

#[test]
fn oversized_blobs_are_skipped() {
    let dir = TempDir::new("hardening-bigblob");
    let path = make_store(&dir.0, "ok");
    let c = Connection::open(&path).unwrap();
    c.execute("insert into blobs values('big', zeroblob(?))", [store::MAX_BLOB_BYTES + 1]).unwrap();
    drop(c);
    let store = Store::open(&path).unwrap();
    let rows = store.scan_rows(0).unwrap();
    assert_eq!(rows.len(), 3, "the oversized row still advances the rowid");
    let big = rows.iter().find(|r| r.id == "big").unwrap();
    assert!(big.data.is_empty());
    assert!(store.blob("big").is_err());
    assert_eq!(store.blob("r1").unwrap().map(|b| b.len()), Some(root_blob(1000).len()));
    assert_eq!(Timeline::scan(&store).unwrap().last_rowid, 3);
}
