//! `<state dir>/current.json`: the session the status line last saw, so
//! `ctxviz open` and an unpinned TUI can follow it.
//!
//! Also owns the state dir (`$CTXVIZ_STATE_DIR` or `<cursor dir>/ctxviz`) and the
//! hardened atomic writer every state file goes through.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Largest JSON state file ctxviz reads (`current.json`, `tui.json`, timeline
/// caches). Larger files read as missing.
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CurrentSession {
    pub session_id: String,
    pub store: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub kitty_window_id: Option<String>,
    pub model: Option<String>,
    /// Unix seconds of the last write.
    pub ts: u64,
    /// The Cursor CLI config dir the store was found in. Absent in files
    /// written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_dir: Option<PathBuf>,
    /// Version of the ctxviz binary that wrote the file. Absent in older files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctxviz_version: Option<String>,
}

/// Overrides the state dir.
pub const CTXVIZ_STATE_DIR_ENV: &str = "CTXVIZ_STATE_DIR";

/// The one state dir: `$CTXVIZ_STATE_DIR` if set and non-empty, else
/// `ctxviz` in the Cursor CLI config dir ([`crate::store::try_cursor_dir`],
/// `~/.cursor/ctxviz` by default). Errors when neither is usable.
pub fn try_state_dir() -> Result<PathBuf> {
    match std::env::var_os(CTXVIZ_STATE_DIR_ENV) {
        Some(d) if !d.is_empty() => Ok(PathBuf::from(d)),
        _ => Ok(crate::store::try_cursor_dir()?.join("ctxviz")),
    }
}

/// Infallible [`try_state_dir`]: without a usable cursor dir (and no override) it
/// returns a path under [`crate::store::NO_HOME`], which reads as empty and
/// which [`write_atomic`] refuses to write.
pub fn state_dir() -> PathBuf {
    try_state_dir().unwrap_or_else(|_| PathBuf::from(crate::store::NO_HOME).join(".cursor/ctxviz"))
}

/// Read at most [`MAX_STATE_BYTES`]; `None` when missing, unreadable, or larger.
pub fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_STATE_BYTES + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= MAX_STATE_BYTES).then_some(buf)
}

pub fn current_path() -> PathBuf {
    state_dir().join("current.json")
}

/// `<state dir>/tui.json`: TUI layout remembered across runs.
pub fn tui_state_path() -> PathBuf {
    state_dir().join("tui.json")
}

/// Contents of `tui.json`. Every field is optional (`#[serde(default)]`) and
/// unknown fields are ignored, so fields can be added without breaking old files.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiState {
    /// Explorer width in columns, set with `[`/`]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explorer_cols: Option<u16>,
}

impl TuiState {
    /// Missing or corrupt files read as `None`.
    pub fn read_from(path: &Path) -> Option<TuiState> {
        serde_json::from_slice(&read_capped(path)?).ok()
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }
}

/// Whether [`write_atomic_with`] flushes the temp file to disk before the rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// `sync_all` before the rename: the new contents survive a crash.
    Sync,
    /// Rename only. Readers still see the old or the new file, never a torn
    /// one, but a crash can lose the latest write. Fine for a cache that is
    /// rewritten on every status line tick.
    NoSync,
}

/// [`write_atomic_with`] with [`Durability::Sync`].
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_with(path, bytes, Durability::Sync)
}

/// Write `bytes` to `path` atomically: a fresh temp file in the same dir
/// (`create_new`, mode 0o600, unique name `.<name>.tmp.<pid>.<nanos>.<n>`),
/// `sync_all` when `durability` is [`Durability::Sync`], then rename. Missing
/// parent dirs are created with mode 0o700. An existing parent dir owned by
/// us with looser bits is tightened to 0o700, and the renamed file to 0o600
/// (symlinks are never chmodded). Paths under [`crate::store::NO_HOME`] are
/// refused. A process killed before the rename leaves its temp file behind;
/// [`sweep_stale_tmp_files`] removes those.
pub fn write_atomic_with(path: &Path, bytes: &[u8], durability: Durability) -> Result<()> {
    if path.starts_with(crate::store::NO_HOME) {
        // The sentinel path is internal; do not show it to the user.
        bail!("not writing state: HOME is not set (or CURSOR_CONFIG_DIR or XDG_CONFIG_HOME is not absolute) and CTXVIZ_STATE_DIR is empty; set CTXVIZ_CURSOR_DIR to the Cursor CLI config dir");
    }
    let name = path.file_name().with_context(|| format!("no file name in {}", path.display()))?;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".tmp.{}.{nanos}.{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let tmp = dir.join(tmp_name);
    let result = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes).with_context(|| format!("writing {}", tmp.display()))?;
        if durability == Durability::Sync {
            f.sync_all().with_context(|| format!("syncing {}", tmp.display()))?;
        }
        #[cfg(unix)]
        {
            // The temp file was just created by us, so its owner is our uid.
            use std::os::unix::fs::MetadataExt;
            let uid = f.metadata().with_context(|| format!("stat {}", tmp.display()))?.uid();
            tighten(dir, |m| m.is_dir() && m.uid() == uid, 0o700)
                .with_context(|| format!("tightening {}", dir.display()))?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        #[cfg(unix)]
        tighten(path, |m| m.is_file(), 0o600).with_context(|| format!("tightening {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Set `path` to `mode` when its permission bits are looser than `mode` and
/// `accept` approves its `symlink_metadata` (a symlink is never accepted, so
/// nothing is chmodded through one). A missing path is left alone.
#[cfg(unix)]
pub(crate) fn tighten(path: &Path, accept: impl Fn(&std::fs::Metadata) -> bool, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() || !accept(&meta) || meta.permissions().mode() & 0o777 & !mode == 0 {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

impl CurrentSession {
    pub fn read() -> Option<CurrentSession> {
        Self::read_from(&current_path())
    }

    /// `None` when missing, over [`MAX_STATE_BYTES`], unparsable, or holding
    /// a `session_id` that is not [`crate::text::valid_id`].
    pub fn read_from(path: &Path) -> Option<CurrentSession> {
        let cur: CurrentSession = serde_json::from_slice(&read_capped(path)?).ok()?;
        crate::text::valid_id(&cur.session_id).then_some(cur)
    }

    pub fn write(&self) -> Result<()> {
        self.write_to(&current_path())
    }

    /// Refuses a `session_id` that is not [`crate::text::valid_id`]. Written
    /// with [`Durability::NoSync`]: the status line rewrites this file on every
    /// tick, the rename alone keeps readers from seeing a torn file, and the
    /// fsync was slow enough that the host killed the status line mid-write.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        if !crate::text::valid_id(&self.session_id) {
            bail!("invalid session id {:?}", self.session_id);
        }
        write_atomic_with(path, &serde_json::to_vec_pretty(self)?, Durability::NoSync)
    }

    pub fn now_ts() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }
}

/// Temp files [`write_atomic_with`] left behind are removed after this long.
pub const TMP_STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Delete regular files in `dir` whose name `keep_name` accepts and that were
/// last modified more than `max_age` ago. Uses `symlink_metadata`, so a
/// symlink is never followed or removed. Best effort; returns how many were
/// removed (0 when `dir` is missing).
pub fn sweep_dir(dir: &Path, max_age: Duration, keep_name: impl Fn(&str) -> bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    let mut removed = 0;
    for e in entries.flatten() {
        if !keep_name(&e.file_name().to_string_lossy()) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
        let old = meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age > max_age);
        if meta.is_file() && old && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether `name` looks like a [`write_atomic_with`] temp file.
pub fn is_tmp_name(name: &str) -> bool {
    name.starts_with('.') && name.contains(".tmp.")
}

/// Remove [`write_atomic_with`] temp files older than [`TMP_STALE_AFTER`] from
/// `dir`. Returns how many were removed.
pub fn sweep_tmp_files_in(dir: &Path) -> usize {
    sweep_dir(dir, TMP_STALE_AFTER, is_tmp_name)
}

/// [`sweep_tmp_files_in`] on the state dir, run on TUI start. Does not create
/// the state dir; 0 when it cannot be resolved.
pub fn sweep_stale_tmp_files() -> usize {
    try_state_dir().map_or(0, |d| sweep_tmp_files_in(&d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_atomic_tightens_existing_loose_dir() {
        let root = crate::test_support::TempDir::new("session-loose-dir");
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_atomic(&state.join("current.json"), b"{}").unwrap();
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join("current.json")), 0o600);
    }

    #[test]
    fn write_atomic_tightens_existing_loose_file() {
        let root = crate::test_support::TempDir::new("session-loose-file");
        let path = root.join("tui.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        TuiState { explorer_cols: Some(30) }.write_to(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(TuiState::read_from(&path), Some(TuiState { explorer_cols: Some(30) }));
    }

    #[test]
    fn write_atomic_leaves_private_dir_at_0700() {
        let root = crate::test_support::TempDir::new("session-strict-dir");
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_atomic(&state.join("x.json"), b"{}").unwrap();
        assert_eq!(mode(&state), 0o700);
    }

    #[test]
    fn tighten_never_chmods_through_symlink() {
        let root = crate::test_support::TempDir::new("session-tighten-link");
        let target = root.join("victim");
        std::fs::write(&target, "keep").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        tighten(&link, |_| true, 0o600).unwrap();
        assert_eq!(mode(&target), 0o644);
        tighten(&root.join("missing"), |_| true, 0o600).unwrap();
    }

    #[test]
    fn no_home_write_error_hides_placeholder_path() {
        let path = PathBuf::from(crate::store::NO_HOME).join(".cursor/ctxviz/current.json");
        let err = format!("{:#}", write_atomic(&path, b"{}").unwrap_err());
        assert!(err.contains("HOME is not set"), "{err}");
        assert!(err.contains("CTXVIZ_CURSOR_DIR"), "{err}");
        assert!(!err.contains(crate::store::NO_HOME), "{err}");
    }

    #[test]
    fn current_session_parses_old_files() {
        let old = br#"{"session_id":"abc","store":"/s/store.db","cwd":"/w","kitty_window_id":"3","model":"m","ts":7}"#;
        let root = crate::test_support::TempDir::new("session-old-current");
        let path = root.join("current.json");
        std::fs::write(&path, old).unwrap();
        let cur = CurrentSession::read_from(&path).unwrap();
        assert_eq!((cur.session_id.as_str(), cur.ts), ("abc", 7));
        assert_eq!((cur.cursor_dir.as_deref(), cur.ctxviz_version.as_deref()), (None, None));
        // Absent fields stay absent on write; present ones round-trip.
        let json = String::from_utf8(serde_json::to_vec(&cur).unwrap()).unwrap();
        assert!(!json.contains("cursor_dir") && !json.contains("ctxviz_version"), "{json}");
        let new = CurrentSession { cursor_dir: Some("/c".into()), ctxviz_version: Some("1.2.3".into()), ..cur };
        new.write_to(&path).unwrap();
        assert_eq!(CurrentSession::read_from(&path), Some(new));
    }

    #[test]
    fn sweep_dir_keeps_fresh_and_skips_symlinks() {
        let root = crate::test_support::TempDir::new("session-sweep");
        let past = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        let set_old = |p: &Path| std::fs::File::options().write(true).open(p).unwrap().set_modified(past).unwrap();
        let stale = root.join(".current.json.tmp.1.2.0");
        let fresh = root.join(".current.json.tmp.1.3.1");
        let other = root.join("current.json");
        for p in [&stale, &fresh, &other] {
            std::fs::write(p, "{}").unwrap();
        }
        set_old(&stale);
        set_old(&other);
        // A fresh symlink to an old file is not removed: its own mtime counts,
        // and a symlink is never a regular file.
        let target = root.join(".victim.tmp.0");
        std::fs::write(&target, "keep").unwrap();
        let link = root.join(".link.tmp.0");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        set_old(&target);
        let sub = root.join(".dir.tmp.0");
        std::fs::create_dir(&sub).unwrap();

        assert_eq!(sweep_dir(&root, TMP_STALE_AFTER, |n| n.starts_with(".current") || n.starts_with(".link")), 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(other.exists());
        assert!(std::fs::symlink_metadata(&link).is_ok());
        assert!(target.exists());
        assert_eq!(sweep_tmp_files_in(&root), 1, "the old target itself matches");
        assert!(sub.is_dir());
        assert_eq!(sweep_dir(&root.join("missing"), TMP_STALE_AFTER, |_| true), 0);
        assert!(!is_tmp_name("current.json") && !is_tmp_name(".hidden") && is_tmp_name(".tui.json.tmp.9.9.9"));
    }
}
