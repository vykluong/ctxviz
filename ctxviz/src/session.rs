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
}

/// The one state dir: `$CTXVIZ_STATE_DIR` if set and non-empty, else
/// `ctxviz` in the Cursor CLI config dir ([`crate::store::try_cursor_dir`],
/// `~/.cursor/ctxviz` by default). Errors when neither is usable.
pub fn try_state_dir() -> Result<PathBuf> {
    match std::env::var_os("CTXVIZ_STATE_DIR") {
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

/// Write `bytes` to `path` atomically: a fresh temp file in the same dir
/// (`create_new`, mode 0o600, unique name), `sync_all`, then rename. Missing
/// parent dirs are created with mode 0o700. An existing parent dir owned by
/// us with looser bits is tightened to 0o700, and the renamed file to 0o600
/// (symlinks are never chmodded). Paths under [`crate::store::NO_HOME`] are
/// refused.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.starts_with(crate::store::NO_HOME) {
        // The sentinel path is internal; do not show it to the user.
        bail!("not writing state: HOME is not set (or CURSOR_CONFIG_DIR or XDG_CONFIG_HOME is not absolute) and CTXVIZ_STATE_DIR is empty");
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
        f.sync_all().with_context(|| format!("syncing {}", tmp.display()))?;
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

    /// Refuses a `session_id` that is not [`crate::text::valid_id`].
    pub fn write_to(&self, path: &Path) -> Result<()> {
        if !crate::text::valid_id(&self.session_id) {
            bail!("invalid session id {:?}", self.session_id);
        }
        write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn now_ts() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }
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
        assert!(!err.contains(crate::store::NO_HOME), "{err}");
    }
}
