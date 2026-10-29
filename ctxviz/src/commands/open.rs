//! `ctxviz open`: open the TUI in a Kitty vsplit next to the agent window for
//! the session in `current.json`, or report the split that is already open.

use crate::kitty::{self, Opened};
use crate::session::CurrentSession;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    /// Store to show (default: the store in current.json).
    #[arg(long)]
    pub chat: Option<PathBuf>,
    /// Focus an already open split (default: keep the agent window focused).
    #[arg(long)]
    pub focus: bool,
    // Not applied here: forwarded as `--theme`/`--theme-file` to the launched `ctxviz tui`.
    #[command(flatten)]
    pub theme: super::ThemeArgs,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    match open(args) {
        Ok(()) => Ok(0),
        Err(msg) => {
            let msg = format!("ctxviz open: {msg}");
            eprintln!("{msg}");
            // A `launch --type=background` from kitty.conf has no window for
            // stderr, so also leave the reason where the user can find it.
            log_line(&log_path(), CurrentSession::now_ts(), &msg);
            Ok(1)
        }
    }
}

/// `<state dir>/open.log`: one line per failed `ctxviz open`.
pub fn log_path() -> PathBuf {
    crate::session::state_dir().join("open.log")
}

/// `open.log` is rotated to `open.log.1` (replacing any older one) once it
/// grows past this, so a failing kitty.conf hook cannot fill the disk.
pub const LOG_MAX_BYTES: u64 = 256 * 1024;

/// Best effort: append `<unix secs> <msg>` to `path`; errors are ignored.
/// The file is created with mode 0o600 (its parent dirs 0o700), written as a
/// single sanitized line, and rotated once it exceeds [`LOG_MAX_BYTES`]. A
/// symlink or other non-file at `path` is left alone.
fn log_line(path: &Path, ts: u64, msg: &str) {
    if path.starts_with(crate::store::NO_HOME) {
        return; // No state dir: nowhere to log, and never create the placeholder.
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Never append through a symlink (or to anything but a regular file).
        // The state dir is 0o700, so this check is not raced by other users.
        if !meta.is_file() {
            return;
        }
        if meta.len() > LOG_MAX_BYTES {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let old = PathBuf::from(old);
            if std::fs::rename(path, &old).is_ok() {
                // A log written before ctxviz enforced 0o600 may be looser.
                #[cfg(unix)]
                let _ = crate::session::tighten(&old, |m| m.is_file(), 0o600);
            }
        }
    }
    let opened = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path);
    if let Ok(mut f) = opened {
        // `mode` only applies on create, so tighten a pre-existing looser log
        // through the open handle (no second path lookup).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if f.metadata().is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
        }
        let _ = writeln!(f, "{ts} {}", crate::text::sanitize_single_line(msg));
    }
}

/// Resolve `--chat` against the caller's cwd (the split starts in the agent's
/// cwd, so a relative path would point elsewhere) and resolve symlinks.
/// Kitty does not expand `$` or `~` in program arguments; it only substitutes
/// an argument that is exactly an `@` placeholder such as `@selection`, which
/// an absolute path can never be. `kitty::launch_split` re-checks this.
fn resolve_chat(p: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(p).map_err(|e| format!("--chat {}: {e}", p.display()))
}

fn open(args: Args) -> Result<(), String> {
    let mut k = kitty::detect().map_err(|reason| {
        format!("not in Kitty ({reason}; a kitty.conf `launch --type=background` needs --allow-remote-control)")
    })?;
    // Without a usable cursor dir (and no CTXVIZ_STATE_DIR) the state path is an
    // internal placeholder, so report the cause instead of that path.
    let current = crate::session::try_state_dir()
        .map_err(|e| format!("no current session ({e:#}); run ctxviz doctor"))?
        .join("current.json");
    let cur = CurrentSession::read_from(&current).ok_or_else(|| {
        format!("no current session ({} missing or unreadable); run ctxviz doctor", current.display())
    })?;
    // The agent window recorded by the status line wins: a `--type=background`
    // launch from kitty.conf has no KITTY_WINDOW_ID of its own.
    if let Some(w) = cur.kitty_window_id.clone().filter(|w| !w.is_empty()) {
        k.window_id = Some(w);
    }
    // Validate before it reaches a `window_id:N` / `id:N` match expression.
    if let Some(w) = &k.window_id {
        let id = kitty::window_id_arg(w).map_err(|e| format!("{e:#}"))?;
        k.window_id = Some(id.to_string());
    }
    let store = match args.chat {
        Some(p) => resolve_chat(&p)?,
        None => cur.store.clone().ok_or("current session has no store path")?,
    };
    let cwd = match cur.cwd.clone() {
        Some(c) => c,
        None => std::env::current_dir().map_err(|e| format!("current directory: {e}"))?,
    };
    // The split starts in the agent's cwd, so pin a relative theme file to ours.
    let theme_file = args
        .theme
        .theme_file
        .as_deref()
        .map(std::path::absolute)
        .transpose()
        .map_err(|e| format!("--theme-file: {e}"))?;
    let theme = args.theme.theme.as_deref();
    match kitty::focus_or_launch(&k, &cur.session_id, &store, &cwd, theme, theme_file.as_deref(), args.focus) {
        Ok(Opened::Existing(id)) => {
            let what = if args.focus { "focused" } else { "already open" };
            println!("{id} ({what})");
            Ok(())
        }
        Ok(Opened::Launched(id)) => {
            println!("{id}");
            Ok(())
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_appends_one_line_per_call() {
        let dir = crate::test_support::TempDir::new("open-log");
        let path = dir.join("nested/open.log");
        log_line(&path, 100, "ctxviz open: not in Kitty");
        log_line(&path, 101, "ctxviz open: kitten @ ls: boom\nsecond line");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "100 ctxviz open: not in Kitty\n101 ctxviz open: kitten @ ls: boom second line\n");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn log_line_rotates_past_cap_and_sanitizes() {
        let dir = crate::test_support::TempDir::new("open-log-rot");
        let path = dir.join("open.log");
        std::fs::write(&path, vec![b'x'; LOG_MAX_BYTES as usize + 1]).unwrap();
        log_line(&path, 5, "after\x1b]0;pwn\x07");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "5 after\u{FFFD}]0;pwn\u{FFFD}\n");
        assert_eq!(std::fs::metadata(dir.join("open.log.1")).unwrap().len(), LOG_MAX_BYTES + 1);
    }

    #[test]
    fn log_line_tightens_rotated_and_existing_logs() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = crate::test_support::TempDir::new("open-log-perms");
        let path = dir.join("open.log");
        // An existing 0o644 log under the cap is tightened in place.
        std::fs::write(&path, "1 old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        log_line(&path, 2, "new");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1 old\n2 new\n");
        // A 0o644 log past the cap rotates to a 0o600 `open.log.1`.
        std::fs::write(&path, vec![b'x'; LOG_MAX_BYTES as usize + 1]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        log_line(&path, 3, "after");
        assert_eq!(mode(&dir.join("open.log.1")), 0o600);
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn log_line_skips_no_home_placeholder() {
        let path = PathBuf::from(crate::store::NO_HOME).join(".cursor/ctxviz/open.log");
        log_line(&path, 1, "x");
        assert!(!Path::new(crate::store::NO_HOME).exists());
    }

    #[test]
    fn log_line_does_not_follow_symlink() {
        let dir = crate::test_support::TempDir::new("open-log-link");
        let target = dir.join("victim");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("open.log")).unwrap();
        log_line(&dir.join("open.log"), 1, "x");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[test]
    fn chat_paths_are_canonicalized() {
        let dir = crate::test_support::TempDir::new("open-chat");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("store.db"), "").unwrap();
        let got = resolve_chat(&dir.join("sub/../store.db")).unwrap();
        assert!(got.is_absolute());
        assert_eq!(got, std::fs::canonicalize(dir.join("store.db")).unwrap());
        assert!(resolve_chat(&dir.join("missing.db")).is_err());
    }

    #[test]
    fn log_line_ignores_unwritable_path() {
        // A directory where the file should be: open fails, nothing panics.
        let dir = crate::test_support::TempDir::new("open-log-dir");
        log_line(&dir, 1, "x");
    }
}
