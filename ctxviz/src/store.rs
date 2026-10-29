//! Read-only access to a cursor-agent chat store
//! (`<cursor dir>/chats/<md5(cwd)>/<agentId>/store.db`, tables `blobs(id, data)`
//! and `meta(key, value)`). The cursor dir is resolved like the Cursor CLI
//! does it; see [`try_cursor_dir`].
//!
//! A store is untrusted input: it is opened read-only with defensive SQLite
//! settings, `blobs` and `meta` must be real tables, and oversized values are
//! skipped rather than loaded.

use crate::{schema, text};
use anyhow::{bail, Context, Result};
use rusqlite::{config::DbConfig, types::ValueRef, Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Parsed `meta` row '0' (hex-encoded JSON). Unknown keys stay in `raw`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaRow {
    pub agent_id: Option<String>,
    pub name: Option<String>,
    pub latest_root_blob_id: Option<String>,
    pub blob_encryption_key: Option<String>,
    pub created_at: Option<serde_json::Value>,
    pub mode: Option<String>,
    pub last_used_model: Option<String>,
    #[serde(skip)]
    pub raw: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct BlobRow {
    pub rowid: i64,
    pub id: String,
    pub data: Vec<u8>,
}

pub struct Store {
    pub path: PathBuf,
    conn: Connection,
}

/// Blobs larger than this are never loaded: `scan_rows` yields them with
/// empty data and `blob` returns an error.
pub const MAX_BLOB_BYTES: i64 = 64 * 1024 * 1024;

/// Upper bound on the hex-encoded meta value.
pub const MAX_META_BYTES: i64 = 16 * 1024 * 1024;

fn value_bytes(v: ValueRef<'_>) -> Vec<u8> {
    match v {
        ValueRef::Blob(b) | ValueRef::Text(b) => b.to_vec(),
        _ => Vec::new(),
    }
}

/// Defensive settings for a connection to an untrusted database.
fn harden(conn: &Connection) -> Result<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    conn.pragma_update(None, "query_only", 1)?;
    Ok(())
}

impl Store {
    /// Open read-only, no mutex, 50 ms busy timeout, then harden the
    /// connection: `SQLITE_DBCONFIG_DEFENSIVE`, `trusted_schema=OFF`,
    /// `query_only=1`. Fails unless `blobs` and `meta` are real tables (a
    /// crafted store could define them as views over arbitrary SQL).
    ///
    /// Sidecar files: a read-only open does create files in the chat dir.
    /// For a WAL-mode store with no `-wal`/`-shm` present (writer closed),
    /// the first query creates both `store.db-wal` (empty) and
    /// `store.db-shm`, and they remain after the connection closes. With only
    /// `-wal` present, `-shm` is created. Read-only mode does not stop this:
    /// the WAL index must exist for any reader. This matches what
    /// cursor-agent itself leaves while a chat is live. Verified empirically
    /// with the bundled SQLite (`readonly_open_sidecars` test in
    /// `tests/store_hardening.rs`).
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref().to_path_buf();
        let conn =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_millis(50))?;
        harden(&conn).with_context(|| format!("hardening connection to {}", path.display()))?;
        for table in ["blobs", "meta"] {
            let kind: Option<String> = conn
                .query_row("select type from sqlite_master where name=?", [table], |r| r.get(0))
                .optional()
                .with_context(|| format!("reading schema of {}", path.display()))?;
            match kind.as_deref() {
                Some("table") => {}
                Some(other) => bail!("{}: `{table}` is a {other}, not a table", path.display()),
                None => bail!("{}: no `{table}` table; not a cursor-agent store", path.display()),
            }
        }
        Ok(Store { path, conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Raw decoded meta JSON, or an empty map when the row is missing.
    pub fn meta_json(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let row: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                "select octet_length(value), case when octet_length(value) <= ?2 then value end \
                 from meta where key=?1",
                rusqlite::params![schema::META_KEY, MAX_META_BYTES],
                |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), r.get(1)?)),
            )
            .optional()?;
        let Some((len, hex)) = row else { return Ok(Default::default()) };
        let Some(hex) = hex else { bail!("meta value is {len} bytes (limit {MAX_META_BYTES})") };
        let bytes = schema::unhex(&hex).context("meta value is not hex")?;
        serde_json::from_slice(&bytes).context("meta value is not JSON")
    }

    pub fn meta(&self) -> Result<MetaRow> {
        let raw = self.meta_json()?;
        let mut m: MetaRow =
            serde_json::from_value(serde_json::Value::Object(raw.clone())).context("unexpected meta shape")?;
        m.raw = raw;
        Ok(m)
    }

    /// Cheap probe for change detection.
    pub fn latest_root_id(&self) -> Result<Option<String>> {
        Ok(self.meta()?.latest_root_blob_id)
    }

    /// Blob data by id; an error when it exceeds [`MAX_BLOB_BYTES`].
    pub fn blob(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let row = self
            .conn
            .query_row(
                "select octet_length(data), case when octet_length(data) <= ?2 then data end \
                 from blobs where id=?1",
                rusqlite::params![id, MAX_BLOB_BYTES],
                |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), value_bytes(r.get_ref(1)?))),
            )
            .optional()?;
        match row {
            Some((len, _)) if len > MAX_BLOB_BYTES => bail!("blob {id} is {len} bytes (limit {MAX_BLOB_BYTES})"),
            Some((_, data)) => Ok(Some(data)),
            None => Ok(None),
        }
    }

    /// Rows with `rowid > after_rowid` in insertion order, collected. Prefer
    /// [`for_each_row`](Self::for_each_row), which streams.
    pub fn scan_rows(&self, after_rowid: i64) -> Result<Vec<BlobRow>> {
        let mut out = Vec::new();
        self.for_each_row(after_rowid, |r| {
            out.push(r);
            Ok(())
        })?;
        Ok(out)
    }

    /// Stream rows with `rowid > after_rowid` in insertion order, one at a
    /// time. Rows whose data exceeds [`MAX_BLOB_BYTES`] are passed with empty
    /// `data` (their content is never read), so callers still see the rowid.
    pub fn for_each_row(&self, after_rowid: i64, mut f: impl FnMut(BlobRow) -> Result<()>) -> Result<()> {
        let mut st = self.conn.prepare_cached(
            "select rowid, id, case when octet_length(data) <= ?2 then data end \
             from blobs where rowid > ?1 order by rowid",
        )?;
        let mut rows = st.query(rusqlite::params![after_rowid, MAX_BLOB_BYTES])?;
        while let Some(r) = rows.next()? {
            f(BlobRow { rowid: r.get(0)?, id: r.get(1)?, data: value_bytes(r.get_ref(2)?) })?;
        }
        Ok(())
    }

    pub fn max_rowid(&self) -> Result<i64> {
        Ok(self.conn.query_row("select coalesce(max(rowid), 0) from blobs", [], |r| r.get(0))?)
    }
}

/// Change-detection tuple: WAL size is not monotonic (truncated on checkpoint),
/// so every component is compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteStamp {
    pub db_mtime: Option<SystemTime>,
    pub wal_mtime: Option<SystemTime>,
    pub wal_size: Option<u64>,
    pub shm_mtime: Option<SystemTime>,
}

impl WriteStamp {
    /// Python `last_write`: newest of store.db and store.db-wal.
    pub fn latest(&self) -> Option<SystemTime> {
        self.db_mtime.max(self.wal_mtime)
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn last_write(path: &Path) -> WriteStamp {
    let md = |p: &Path| std::fs::metadata(p).ok();
    let db = md(path);
    let wal = md(&sidecar(path, "-wal"));
    let shm = md(&sidecar(path, "-shm"));
    WriteStamp {
        db_mtime: db.and_then(|m| m.modified().ok()),
        wal_mtime: wal.as_ref().and_then(|m| m.modified().ok()),
        wal_size: wal.map(|m| m.len()),
        shm_mtime: shm.and_then(|m| m.modified().ok()),
    }
}

/// `$HOME`, which must be set, non-empty, and absolute. There is no fallback
/// to `/`: without a home (and no `CURSOR_CONFIG_DIR` or `XDG_CONFIG_HOME`)
/// there is no chats dir and no state dir.
pub fn home() -> Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() && Path::new(&h).is_absolute() => Ok(PathBuf::from(h)),
        Some(h) if !h.is_empty() => bail!("HOME is not an absolute path: {}", Path::new(&h).display()),
        _ => bail!("HOME is not set"),
    }
}

/// Overrides the Cursor CLI config dir, which holds `chats/` and `cli-config.json`.
pub const CURSOR_CONFIG_DIR_ENV: &str = "CURSOR_CONFIG_DIR";

/// ctxviz's own override for the Cursor CLI config dir, for when the shell's
/// `CURSOR_CONFIG_DIR` is not the one the agent uses.
pub const CTXVIZ_CURSOR_DIR_ENV: &str = "CTXVIZ_CURSOR_DIR";

/// Where the Cursor CLI config dir came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDirSource {
    /// `$CTXVIZ_CURSOR_DIR`.
    CtxvizEnv,
    /// `$CURSOR_CONFIG_DIR`.
    ConfigDirEnv,
    /// `$XDG_CONFIG_HOME/cursor`.
    XdgConfigHome,
    /// `~/.cursor`.
    Home,
}

impl std::fmt::Display for CursorDirSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CursorDirSource::CtxvizEnv => "$CTXVIZ_CURSOR_DIR",
            CursorDirSource::ConfigDirEnv => "$CURSOR_CONFIG_DIR",
            CursorDirSource::XdgConfigHome => "$XDG_CONFIG_HOME/cursor",
            CursorDirSource::Home => "~/.cursor",
        })
    }
}

/// The Cursor CLI config dir: `$CTXVIZ_CURSOR_DIR` if set, else resolved the
/// way the CLI does it: `$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`,
/// else `~/.cursor`. A value that is empty or only whitespace counts as unset.
/// Unlike the CLI, a relative path is an error: the status line runs in the
/// workspace, so it would resolve somewhere else.
pub fn try_cursor_dir() -> Result<PathBuf> {
    Ok(try_cursor_dir_with_source()?.0)
}

/// [`try_cursor_dir`] and which variable chose it.
pub fn try_cursor_dir_with_source() -> Result<(PathBuf, CursorDirSource)> {
    resolve_cursor_dir(
        std::env::var_os(CTXVIZ_CURSOR_DIR_ENV),
        std::env::var_os(CURSOR_CONFIG_DIR_ENV),
        std::env::var_os("XDG_CONFIG_HOME"),
        home,
    )
}

/// A variable's value as a path, or `None` when unset, empty, or only whitespace.
fn set_path(v: Option<std::ffi::OsString>) -> Option<PathBuf> {
    v.filter(|v| !v.to_string_lossy().trim().is_empty()).map(PathBuf::from)
}

/// [`try_cursor_dir_with_source`] over explicit values, for tests.
pub fn resolve_cursor_dir(
    ctxviz_env: Option<std::ffi::OsString>,
    config_dir: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: impl FnOnce() -> Result<PathBuf>,
) -> Result<(PathBuf, CursorDirSource)> {
    let (dir, source) = match (set_path(ctxviz_env), set_path(config_dir), set_path(xdg_config_home)) {
        (Some(d), _, _) => (d, CursorDirSource::CtxvizEnv),
        (None, Some(d), _) => (d, CursorDirSource::ConfigDirEnv),
        (None, None, Some(x)) => (x.join("cursor"), CursorDirSource::XdgConfigHome),
        (None, None, None) => (home()?.join(".cursor"), CursorDirSource::Home),
    };
    if !dir.is_absolute() {
        bail!("{source} is not an absolute path: {}", dir.display());
    }
    Ok((dir, source))
}

/// Every Cursor CLI config dir the variables name, in precedence order and
/// deduplicated: the [`resolve_cursor_dir`] result first (when it resolves),
/// then the other absolute candidates. Blank and relative values, and
/// `~/.cursor` without a usable home, are skipped silently. The status line
/// searches these for its session, since the shell and the agent may disagree
/// on the dir.
pub fn candidate_cursor_dirs(
    ctxviz_env: Option<std::ffi::OsString>,
    config_dir: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: impl FnOnce() -> Result<PathBuf>,
) -> Vec<(PathBuf, CursorDirSource)> {
    let all = [
        (set_path(ctxviz_env), CursorDirSource::CtxvizEnv),
        (set_path(config_dir), CursorDirSource::ConfigDirEnv),
        (set_path(xdg_config_home).map(|x| x.join("cursor")), CursorDirSource::XdgConfigHome),
        (home().ok().map(|h| h.join(".cursor")), CursorDirSource::Home),
    ];
    let mut out: Vec<(PathBuf, CursorDirSource)> = Vec::new();
    for (dir, source) in all {
        let Some(dir) = dir.filter(|d| d.is_absolute()) else { continue };
        if !out.iter().any(|(d, _)| *d == dir) {
            out.push((dir, source));
        }
    }
    out
}

/// [`candidate_cursor_dirs`] from the process environment.
pub fn candidate_cursor_dirs_from_env() -> Vec<(PathBuf, CursorDirSource)> {
    candidate_cursor_dirs(
        std::env::var_os(CTXVIZ_CURSOR_DIR_ENV),
        std::env::var_os(CURSOR_CONFIG_DIR_ENV),
        std::env::var_os("XDG_CONFIG_HOME"),
        home,
    )
}

/// `<cursor dir>/chats`, or an error when no cursor dir is usable.
pub fn try_chats_dir() -> Result<PathBuf> {
    Ok(try_cursor_dir()?.join("chats"))
}

/// Infallible form of [`try_chats_dir`] for lookups: without a usable cursor
/// dir it returns a path under [`NO_HOME`] that never exists, so lookups find
/// nothing instead of searching `/`.
pub fn chats_dir() -> PathBuf {
    try_chats_dir().unwrap_or_else(|_| PathBuf::from(NO_HOME).join(".cursor/chats"))
}

/// Placeholder root used when no cursor dir is usable. Nothing is ever read from or
/// written under it: `session::write_atomic` refuses paths below it.
pub const NO_HOME: &str = "/nonexistent/ctxviz-no-home";

/// `os.path.abspath`: join with the process cwd and normalize lexically.
pub fn abspath(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
            _ => {}
        }
    }
    out
}

/// md5 of the absolute workspace path: the project directory name.
pub fn project_hash(cwd: &Path) -> String {
    format!("{:x}", md5::compute(abspath(cwd).as_os_str().as_encoded_bytes()))
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default()
}

/// `<dir>/*/store.db` that exist.
fn stores_in(dir: &Path) -> Vec<PathBuf> {
    subdirs(dir).into_iter().map(|d| d.join("store.db")).filter(|p| p.is_file()).collect()
}

fn newest(stores: Vec<PathBuf>) -> Option<PathBuf> {
    stores.into_iter().max_by_key(|p| last_write(p).latest())
}

/// Store for an exact agent id, searching every project dir. Ids that are not
/// [`text::valid_id`] (path separators, `..`, leading dot) find nothing; use
/// [`try_store_for_agent`] for an error that says why.
pub fn store_for_agent(agent_id: &str) -> Option<PathBuf> {
    try_store_for_agent(agent_id).ok().flatten()
}

/// [`store_for_agent`] that rejects an invalid id (or an unusable `HOME`)
/// with an error.
pub fn try_store_for_agent(agent_id: &str) -> Result<Option<PathBuf>> {
    if !text::valid_id(agent_id) {
        bail!("invalid agent id {agent_id:?}");
    }
    Ok(store_for_agent_in(&try_chats_dir()?, agent_id))
}

/// `<chats>/*/<agent_id>/store.db` that is a file. An id that is not
/// [`text::valid_id`] finds nothing.
pub fn store_for_agent_in(chats: &Path, agent_id: &str) -> Option<PathBuf> {
    if !text::valid_id(agent_id) {
        return None;
    }
    subdirs(chats).into_iter().map(|d| d.join(agent_id).join("store.db")).find(|p| p.is_file())
}

/// Python `find_store`: explicit path, agent id (glob `*/<id>/store.db`),
/// newest store anywhere with `latest`, else newest store for md5(abs cwd).
pub fn find_store(chat: Option<&str>, cwd: &Path, latest: bool) -> Result<PathBuf> {
    if let Some(chat) = chat {
        if Path::new(chat).is_file() {
            return Ok(PathBuf::from(chat));
        }
        if !text::valid_id(chat) {
            bail!("{chat:?} is neither a store file nor a valid agent id");
        }
        return try_store_for_agent(chat)?.with_context(|| format!("no chat store for agent id {chat}"));
    }
    let chats = try_chats_dir()?;
    if latest {
        let all: Vec<PathBuf> = subdirs(&chats).iter().flat_map(|d| stores_in(d)).collect();
        return newest(all).with_context(|| format!("no cursor-agent chats in {}", chats.display()));
    }
    let project = chats.join(project_hash(cwd));
    match newest(stores_in(&project)) {
        Some(p) => Ok(p),
        None => bail!(
            "no cursor-agent chats for {} (looked in {}); pass --cwd <workspace> or --latest; \
             set CTXVIZ_CURSOR_DIR if the agent uses another config dir (ctxviz doctor shows the dirs)",
            cwd.display(),
            project.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    type Dirs = Vec<(PathBuf, CursorDirSource)>;

    fn os(v: Option<&str>) -> Option<OsString> {
        v.map(OsString::from)
    }

    fn resolve(ctxviz: Option<&str>, config: Option<&str>, xdg: Option<&str>) -> Result<(PathBuf, CursorDirSource)> {
        resolve_cursor_dir(os(ctxviz), os(config), os(xdg), || Ok(PathBuf::from("/home/u")))
    }

    fn candidates(ctxviz: Option<&str>, config: Option<&str>, xdg: Option<&str>) -> Dirs {
        candidate_cursor_dirs(os(ctxviz), os(config), os(xdg), || Ok(PathBuf::from("/home/u")))
    }

    #[test]
    fn cursor_dir_follows_the_cli_order() {
        let ok = |c, x| resolve(None, c, x).unwrap();
        assert_eq!(ok(None, None), (PathBuf::from("/home/u/.cursor"), CursorDirSource::Home));
        assert_eq!(ok(None, Some("/xdg")), (PathBuf::from("/xdg/cursor"), CursorDirSource::XdgConfigHome));
        assert_eq!(ok(Some("/cfg"), Some("/xdg")), (PathBuf::from("/cfg"), CursorDirSource::ConfigDirEnv));
        // Empty and whitespace-only values count as unset, as in the CLI.
        assert_eq!(ok(Some(""), Some(" \t")), (PathBuf::from("/home/u/.cursor"), CursorDirSource::Home));
        assert_eq!(ok(Some("  "), Some("/xdg")).1, CursorDirSource::XdgConfigHome);
    }

    #[test]
    fn ctxviz_cursor_dir_wins() {
        let ok = |t, c, x| resolve(t, c, x).unwrap();
        let ctx = (PathBuf::from("/ctx"), CursorDirSource::CtxvizEnv);
        assert_eq!(ok(Some("/ctx"), Some("/cfg"), Some("/xdg")), ctx);
        assert_eq!(ok(Some("/ctx"), None, None), ctx);
        assert_eq!(CursorDirSource::CtxvizEnv.to_string(), "$CTXVIZ_CURSOR_DIR");
        // Blank falls through to the next variable.
        assert_eq!(ok(Some(" "), Some("/cfg"), None), (PathBuf::from("/cfg"), CursorDirSource::ConfigDirEnv));
        assert_eq!(ok(Some(""), None, Some("/xdg")).1, CursorDirSource::XdgConfigHome);
        assert_eq!(ok(Some("\t"), None, None).1, CursorDirSource::Home);
    }

    #[test]
    fn cursor_dir_rejects_relative_paths() {
        let err = format!("{:#}", resolve(None, Some("rel/cfg"), None).unwrap_err());
        assert!(err.contains("$CURSOR_CONFIG_DIR is not an absolute path"), "{err}");
        let err = format!("{:#}", resolve(None, None, Some("rel")).unwrap_err());
        assert!(err.contains("$XDG_CONFIG_HOME/cursor is not an absolute path"), "{err}");
        // A relative override is an error even when the others are usable.
        let err = format!("{:#}", resolve(Some("rel"), Some("/cfg"), None).unwrap_err());
        assert!(err.contains("$CTXVIZ_CURSOR_DIR is not an absolute path: rel"), "{err}");
    }

    #[test]
    fn cursor_dir_needs_home_only_as_the_fallback() {
        let no_home = || -> Result<PathBuf> { bail!("HOME is not set") };
        assert!(resolve_cursor_dir(None, None, None, no_home).is_err());
        let (dir, _) = resolve_cursor_dir(None, Some("/cfg".into()), None, no_home).unwrap();
        assert_eq!(dir, PathBuf::from("/cfg"));
        let (dir, _) = resolve_cursor_dir(Some("/ctx".into()), None, None, no_home).unwrap();
        assert_eq!(dir, PathBuf::from("/ctx"));
        // Without a home, `~/.cursor` is not a candidate.
        let dirs = candidate_cursor_dirs(None, Some("/cfg".into()), None, no_home);
        assert_eq!(dirs, vec![(PathBuf::from("/cfg"), CursorDirSource::ConfigDirEnv)]);
    }

    #[test]
    fn candidate_dirs_order_and_dedup() {
        use CursorDirSource::*;
        let p = PathBuf::from;
        let all = vec![(p("/ctx"), CtxvizEnv), (p("/cfg"), ConfigDirEnv), (p("/xdg/cursor"), XdgConfigHome)];
        let mut with_home = all.clone();
        with_home.push((p("/home/u/.cursor"), Home));
        assert_eq!(candidates(Some("/ctx"), Some("/cfg"), Some("/xdg")), with_home);
        assert_eq!(candidates(None, None, None), vec![(p("/home/u/.cursor"), Home)]);
        // The first source naming a dir keeps it; equal paths (a trailing
        // slash included) appear once.
        assert_eq!(
            candidates(Some("/home/u/.cursor/"), Some("/xdg/cursor"), Some("/xdg")),
            vec![(p("/home/u/.cursor"), CtxvizEnv), (p("/xdg/cursor"), ConfigDirEnv)]
        );
        // Blank and relative values are skipped silently.
        assert_eq!(
            candidates(Some(" "), Some("rel"), Some("/xdg")),
            vec![(p("/xdg/cursor"), XdgConfigHome), (p("/home/u/.cursor"), Home)]
        );
        // The resolved dir always comes first.
        for (t, c, x) in
            [(Some("/ctx"), Some("/cfg"), None), (Some(""), Some("/cfg"), Some("/xdg")), (None, None, Some("/xdg"))]
        {
            assert_eq!(candidates(t, c, x)[0], resolve(t, c, x).unwrap(), "{t:?} {c:?} {x:?}");
        }
    }
}
