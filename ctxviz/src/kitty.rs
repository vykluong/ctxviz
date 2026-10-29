//! Kitty integration: remote control, OSC 99 notifications, OSC 9;4 progress.
//! Remote control talks to Kitty over its socket (`KITTY_LISTEN_ON`); the
//! notification and progress features write escape sequences to the terminal.
//! Every feature degrades to `Err(reason)` or a no-op outside Kitty.

use crate::text::{sanitize_single_line, valid_id};
use crate::theme::{Rgb, Role, Theme};
use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Remote control commands are killed after this long.
pub const REMOTE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default)]
pub struct Kitty {
    pub window_id: Option<String>,
    pub listen_on: Option<String>,
    pub is_kitty_term: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Ok,
    Warn,
    Critical,
}

impl Band {
    pub fn as_u8(self) -> u8 {
        match self {
            Band::Ok => 0,
            Band::Warn => 1,
            Band::Critical => 2,
        }
    }

    pub fn from_u8(v: u8) -> Option<Band> {
        match v {
            0 => Some(Band::Ok),
            1 => Some(Band::Warn),
            2 => Some(Band::Critical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Clear = 0,
    Indeterminate = 3,
}

/// Read TERM, KITTY_WINDOW_ID, KITTY_LISTEN_ON. `Err(reason)` outside Kitty.
pub fn detect() -> std::result::Result<Kitty, String> {
    let k = Kitty {
        window_id: std::env::var("KITTY_WINDOW_ID").ok(),
        listen_on: std::env::var("KITTY_LISTEN_ON").ok(),
        is_kitty_term: std::env::var("TERM").is_ok_and(|t| t == "xterm-kitty"),
    };
    if k.listen_on.is_none() {
        return Err("KITTY_LISTEN_ON not set".into());
    }
    Ok(k)
}

/// Usage band with hysteresis: warn at 80, critical at 90; a band is left
/// only below 75 (warn) or 85 (critical).
pub fn band_for(percent: f64, prev: Option<Band>) -> Band {
    let fresh = if percent >= 90.0 {
        Band::Critical
    } else if percent >= 80.0 {
        Band::Warn
    } else {
        Band::Ok
    };
    match prev {
        Some(Band::Critical) if percent >= 85.0 => Band::Critical,
        Some(Band::Critical) | Some(Band::Warn) if percent >= 75.0 => fresh.max_with(Band::Warn),
        _ => fresh,
    }
}

impl Band {
    fn max_with(self, other: Band) -> Band {
        if self.as_u8() >= other.as_u8() {
            self
        } else {
            other
        }
    }
}

// ---------------------------------------------------------------------------
// Remote control

/// `kitten @ --to <listen_on> <args...>`, killed after `REMOTE_TIMEOUT`.
/// Returns stdout; a non-zero exit becomes `Err` with stderr.
pub fn remote(k: &Kitty, args: &[&str]) -> Result<String> {
    let to = k.listen_on.as_deref().ok_or_else(|| anyhow!("KITTY_LISTEN_ON not set"))?;
    let mut child = Command::new("kitten")
        .arg("@")
        .arg("--to")
        .arg(to)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning kitten")?;
    // Drain pipes on threads so a large `ls` cannot block the child.
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + REMOTE_TIMEOUT;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("kitten @ {}: timed out after {:?}", sanitize_single_line(&args.join(" ")), REMOTE_TIMEOUT);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    if !status.success() {
        bail!(
            "kitten @ {}: {} ({})",
            sanitize_single_line(args.first().unwrap_or(&"")),
            sanitize_single_line(stderr.trim()),
            status
        );
    }
    Ok(stdout)
}

/// The id of the first window whose user var `name` equals `value`, from
/// `kitten @ ls` (os_windows -> tabs -> windows -> user_vars).
pub fn find_window_by_var(k: &Kitty, name: &str, value: &str) -> Result<Option<u64>> {
    let json = remote(k, &["ls"])?;
    find_in_ls(&json, name, value)
}

fn find_in_ls(json: &str, name: &str, value: &str) -> Result<Option<u64>> {
    let v: serde_json::Value = serde_json::from_str(json).context("kitten @ ls: output is not JSON")?;
    let empty = Vec::new();
    for os in v.as_array().unwrap_or(&empty) {
        for tab in os["tabs"].as_array().unwrap_or(&empty) {
            for w in tab["windows"].as_array().unwrap_or(&empty) {
                if w["user_vars"][name].as_str() == Some(value) {
                    if let Some(id) = w["id"].as_u64() {
                        return Ok(Some(id));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Parse a Kitty window id (from `KITTY_WINDOW_ID` or `current.json`) before
/// it is spliced into a `window_id:N` / `id:N` match expression. Only positive
/// decimal integers are accepted: anything else could widen the match (a
/// negative id counts down from the newest window, and a non-numeric value
/// would be read as a different match expression).
pub fn window_id_arg(s: &str) -> Result<u64> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        bail!("invalid Kitty window id {:?}: expected a positive integer", sanitize_single_line(s));
    }
    match t.parse::<u64>() {
        Ok(id) if id > 0 => Ok(id),
        _ => bail!("invalid Kitty window id {:?}: expected a positive integer", sanitize_single_line(s)),
    }
}

fn agent_window(k: &Kitty) -> Result<u64> {
    let raw = k
        .window_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("no Kitty window id (KITTY_WINDOW_ID or current.json)"))?;
    window_id_arg(raw)
}

/// Check a `--cwd` value for `kitten @ launch`. Kitty runs a literal `--cwd`
/// through Python's `os.path.expanduser` and `os.path.expandvars` (see
/// `kitty/child.py`), which have no escape syntax, so a path containing `$` or
/// starting with `~` would be rewritten; and the bare words `current`,
/// `oldest`, `last_reported`, and `root` have special meanings. Requiring an
/// absolute path without `$` rules all of that out.
fn check_launch_cwd(cwd: &Path) -> Result<()> {
    let s = cwd.to_string_lossy();
    if !cwd.is_absolute() || s.contains('$') || s.chars().any(|c| c.is_control()) {
        bail!(
            "refusing working directory {:?}: Kitty would expand or misread it (must be absolute, no `$`)",
            sanitize_single_line(&s)
        );
    }
    Ok(())
}

/// Check one program argument for `kitten @ launch`. Kitty replaces an
/// argument that is exactly one of its `@` placeholders (`@selection`,
/// `@active-kitty-window-id`, `@line-count`, ...) and does not expand `$`
/// references inside program arguments (see `kitty/launch.py`). Refusing a
/// leading `@` therefore rules out substitution; control characters are
/// refused because they have no business in a path or theme name.
fn check_launch_arg(what: &str, arg: &str) -> Result<()> {
    if arg.starts_with('@') || arg.chars().any(|c| c.is_control()) {
        bail!("refusing {what} {:?}: Kitty would substitute or misread it", sanitize_single_line(arg));
    }
    Ok(())
}

/// Check one `--env` value for `kitten @ launch`: everything
/// [`check_launch_arg`] refuses, plus `$` and surrounding whitespace, since
/// Kitty parses `--env` like the kitty.conf `env` option, which strips the
/// value and expands `$VAR` references in it.
fn check_launch_env(name: &str, value: &str) -> Result<()> {
    check_launch_arg(name, value)?;
    if value.contains('$') || value.trim() != value {
        bail!("refusing {name} {:?}: Kitty would expand or trim it", sanitize_single_line(value));
    }
    Ok(())
}

/// The window id `kitten @ launch` prints (its last non-empty line).
fn parse_window_id(stdout: &str) -> Option<u64> {
    stdout.lines().rev().map(str::trim).find(|l| !l.is_empty())?.parse().ok().filter(|&id| id > 0)
}

/// The `kitten @` arguments for [`launch_split`]: a vsplit next to window
/// `agent` running `<exe> tui --chat <store> [--theme ..] [--theme-file ..]`,
/// tagged with the `ctxviz_session` user var and with each `env` pair as
/// `--env=K=V`. Every path, value, and the cwd is checked first, so nothing
/// Kitty would substitute or expand reaches it. `session_id` must already be
/// [`valid_id`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn launch_args(
    agent: u64,
    session_id: &str,
    exe: &Path,
    store: &Path,
    cwd: &Path,
    theme: Option<&str>,
    theme_file: Option<&Path>,
    env: &[(&str, &str)],
) -> Result<Vec<String>> {
    check_launch_cwd(cwd)?;
    let exe = exe.to_string_lossy();
    let store = store.to_string_lossy();
    let theme_file = theme_file.map(Path::to_string_lossy);
    check_launch_arg("ctxviz binary path", &exe)?;
    check_launch_arg("store path", &store)?;
    if let Some(t) = theme {
        check_launch_arg("theme", t)?;
    }
    if let Some(f) = &theme_file {
        check_launch_arg("theme file", f)?;
    }
    for (k, v) in env {
        check_launch_env(k, v)?;
    }
    let mut args: Vec<String> = ["launch", "--type=window", "--location=vsplit"].map(String::from).into();
    args.extend(["--match".into(), format!("window_id:{agent}"), "--next-to".into(), format!("id:{agent}")]);
    args.extend(["--keep-focus".into(), "--cwd".into(), cwd.to_string_lossy().into_owned()]);
    args.extend(["--title", "ctxviz", "--var"].map(String::from));
    args.push(format!("ctxviz_session={session_id}"));
    args.extend(env.iter().map(|(k, v)| format!("--env={k}={v}")));
    args.extend(["--".into(), exe.into_owned(), "tui".into(), "--chat".into(), store.into_owned()]);
    if let Some(t) = theme {
        args.extend(["--theme".into(), t.to_string()]);
    }
    if let Some(f) = theme_file {
        args.extend(["--theme-file".into(), f.into_owned()]);
    }
    Ok(args)
}

/// Open `ctxviz tui --chat <store> [--theme <theme>] [--theme-file <path>]` in
/// a vsplit next to the agent window, keeping focus on the agent. Returns the
/// new window id. Kitty starts the split with its own environment, not ours,
/// so the resolved Cursor and state dirs are passed as `CTXVIZ_CURSOR_DIR` and
/// `CTXVIZ_STATE_DIR` (each only when it resolves) for the TUI to use the same
/// dirs as this process.
pub fn launch_split(
    k: &Kitty,
    session_id: &str,
    store: &Path,
    cwd: &Path,
    theme: Option<&str>,
    theme_file: Option<&Path>,
) -> Result<u64> {
    let agent = agent_window(k)?;
    if !valid_id(session_id) {
        bail!("refusing session id {:?}: not a plain identifier", sanitize_single_line(session_id));
    }
    check_launch_cwd(cwd)?;
    let exe = std::env::current_exe().context("locating the ctxviz binary")?;
    let cursor_dir = crate::store::try_cursor_dir().ok().map(|d| d.to_string_lossy().into_owned());
    let state_dir = crate::session::try_state_dir().ok().map(|d| d.to_string_lossy().into_owned());
    let mut env = Vec::new();
    if let Some(d) = &cursor_dir {
        env.push((crate::store::CTXVIZ_CURSOR_DIR_ENV, d.as_str()));
    }
    if let Some(d) = &state_dir {
        env.push((crate::session::CTXVIZ_STATE_DIR_ENV, d.as_str()));
    }
    let args = launch_args(agent, session_id, &exe, store, cwd, theme, theme_file, &env)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = remote(k, &args)?;
    parse_window_id(&out).ok_or_else(|| anyhow!("kitten @ launch: no window id in its output ({})", out.trim()))
}

pub fn focus_window(k: &Kitty, id: u64) -> Result<()> {
    remote(k, &["focus-window", "--match", &format!("id:{id}")]).map(|_| ())
}

/// Outcome of `focus_or_launch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// A window for the session already existed (focused if requested).
    Existing(u64),
    Launched(u64),
}

/// Find the session's split by user var; focus it when `focus`, else leave the
/// agent focused. Launch a new split when none exists.
pub fn focus_or_launch(
    k: &Kitty,
    session_id: &str,
    store: &Path,
    cwd: &Path,
    theme: Option<&str>,
    theme_file: Option<&Path>,
    focus: bool,
) -> Result<Opened> {
    if !valid_id(session_id) {
        bail!("refusing session id {:?}: not a plain identifier", sanitize_single_line(session_id));
    }
    if let Some(id) = find_window_by_var(k, "ctxviz_session", session_id)? {
        if focus {
            focus_window(k, id)?;
        }
        return Ok(Opened::Existing(id));
    }
    launch_split(k, session_id, store, cwd, theme, theme_file).map(Opened::Launched)
}

/// Tab bg for themes without truecolor paints (Ansi, Plain): Modus dark warn.
const FALLBACK_WARN: Rgb = Rgb(0xd2, 0x99, 0x22);
/// Tab bg for themes without truecolor paints (Ansi, Plain): Modus dark crit.
const FALLBACK_CRIT: Rgb = Rgb(0xff, 0x7b, 0x72);
/// Tab fg on a colored tab. Fixed: Kitty's tab surface does not follow the
/// terminal theme.
const TAB_FG: &str = "#eff1f5";

/// Tab colors per band from the theme's status roles; `Ok` resets to Kitty's.
fn tab_color_specs(band: Band, theme: &Theme) -> [String; 4] {
    let bg = match band {
        Band::Ok => {
            return ["active_fg=NONE", "active_bg=NONE", "inactive_fg=NONE", "inactive_bg=NONE"].map(String::from)
        }
        Band::Warn => theme.paint(Role::Warn).rgb_or(FALLBACK_WARN).hex(),
        Band::Critical => theme.paint(Role::Crit).rgb_or(FALLBACK_CRIT).hex(),
    };
    [
        format!("active_fg={TAB_FG}"),
        format!("active_bg={bg}"),
        format!("inactive_fg={TAB_FG}"),
        format!("inactive_bg={bg}"),
    ]
}

/// Color the agent window's tab by band, using the process theme. Matches by
/// `window_id:` because a bare `id:` matches tab ids first.
pub fn set_tab_color(k: &Kitty, band: Band) -> Result<()> {
    let m = format!("window_id:{}", agent_window(k)?);
    let specs = tab_color_specs(band, crate::theme::current());
    let mut args = vec!["set-tab-color", "--match", m.as_str()];
    args.extend(specs.iter().map(String::as_str));
    remote(k, &args).map(|_| ())
}

// ---------------------------------------------------------------------------
// Escape sequences (written to /dev/tty, never stdout)

const OSC: &str = "\x1b]";
const ST: &str = "\x1b\\";
/// Kitty limits a chunk's payload to 2048 bytes before encoding.
const MAX_CHUNK_BYTES: usize = 2048;
/// OSC 99 `i` value; one live ctxviz notification at a time.
pub const NOTIFY_ID: &str = "ctxviz-context";

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

/// Split `text` into pieces of at most `max` UTF-8 bytes on char boundaries.
fn split_utf8(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out.push(rest);
    out
}

/// OSC 99 desktop notification: a title chunk then body chunks (each at most
/// 2048 bytes), all `e=1` base64 with the same `i`, `d=0` on all but the last.
pub fn osc99_notify(title: &str, body: &str) -> String {
    let mut chunks: Vec<(&str, &str)> = split_utf8(title, MAX_CHUNK_BYTES).into_iter().map(|t| ("title", t)).collect();
    if !body.is_empty() {
        chunks.extend(split_utf8(body, MAX_CHUNK_BYTES).into_iter().map(|b| ("body", b)));
    }
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(i, (p, payload))| {
            let d = if i < last { ":d=0" } else { "" };
            format!("{OSC}99;i={NOTIFY_ID}{d}:p={p}:e=1;{}{ST}", b64(payload.as_bytes()))
        })
        .collect()
}

/// OSC 9;4 progress (`3` indeterminate, `0` clear), terminated by ST.
pub fn osc_progress(state: ProgressState) -> String {
    format!("{OSC}9;4;{}{ST}", state as u8)
}

fn in_kitty_tty() -> bool {
    std::env::var("TERM").is_ok_and(|t| t == "xterm-kitty") || std::env::var_os("KITTY_WINDOW_ID").is_some()
}

fn write_tty(seq: &str) -> Result<()> {
    if !in_kitty_tty() {
        return Ok(());
    }
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty").context("opening /dev/tty")?;
    tty.write_all(seq.as_bytes())?;
    tty.flush()?;
    Ok(())
}

/// Write `osc99_notify(title, body)` to /dev/tty; no-op outside Kitty.
pub fn emit_osc99(title: &str, body: &str) -> Result<()> {
    write_tty(&osc99_notify(title, body))
}

/// Write `osc_progress(state)` to /dev/tty; no-op outside Kitty.
pub fn emit_progress(state: ProgressState) -> Result<()> {
    write_tty(&osc_progress(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64("ctxviz".as_bytes()), "Y3R4dml6");
    }

    #[test]
    fn osc99_short() {
        let s = osc99_notify("ctxviz", "context at 85%");
        assert_eq!(
            s,
            "\x1b]99;i=ctxviz-context:d=0:p=title:e=1;Y3R4dml6\x1b\\\
             \x1b]99;i=ctxviz-context:p=body:e=1;Y29udGV4dCBhdCA4NSU=\x1b\\"
        );
    }

    #[test]
    fn osc99_title_only_has_no_d0() {
        assert_eq!(osc99_notify("hi", ""), "\x1b]99;i=ctxviz-context:p=title:e=1;aGk=\x1b\\");
    }

    #[test]
    fn osc99_multi_chunk_body() {
        let body = "é".repeat(1500); // 3000 bytes, splits on a char boundary
        let s = osc99_notify("t", &body);
        let parts: Vec<&str> = s.split_inclusive("\x1b\\").collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with("\x1b]99;i=ctxviz-context:d=0:p=title:e=1;"));
        assert!(parts[1].starts_with("\x1b]99;i=ctxviz-context:d=0:p=body:e=1;"));
        assert!(parts[2].starts_with("\x1b]99;i=ctxviz-context:p=body:e=1;"));
        assert_eq!(
            parts[1],
            format!("\x1b]99;i=ctxviz-context:d=0:p=body:e=1;{}\x1b\\", b64("é".repeat(1024).as_bytes()))
        );
        assert_eq!(parts[2], format!("\x1b]99;i=ctxviz-context:p=body:e=1;{}\x1b\\", b64("é".repeat(476).as_bytes())));
    }

    #[test]
    fn progress_sequences() {
        assert_eq!(osc_progress(ProgressState::Indeterminate), "\x1b]9;4;3\x1b\\");
        assert_eq!(osc_progress(ProgressState::Clear), "\x1b]9;4;0\x1b\\");
    }

    #[test]
    fn band_thresholds_and_hysteresis() {
        use Band::*;
        assert_eq!(band_for(10.0, None), Ok);
        assert_eq!(band_for(50.0, None), Ok);
        assert_eq!(band_for(90.0, None), Critical);
        assert_eq!(band_for(79.9, Some(Ok)), Ok);
        assert_eq!(band_for(79.9, None), Ok);
        assert_eq!(band_for(80.0, None), Warn);
        assert_eq!(band_for(90.0, Some(Ok)), Critical);
        // warn holds until below 75
        assert_eq!(band_for(76.0, Some(Warn)), Warn);
        assert_eq!(band_for(74.9, Some(Warn)), Ok);
        assert_eq!(band_for(91.0, Some(Warn)), Critical);
        // critical holds until below 85, then warn until below 75
        assert_eq!(band_for(86.0, Some(Critical)), Critical);
        assert_eq!(band_for(84.0, Some(Critical)), Warn);
        assert_eq!(band_for(84.9, Some(Critical)), Warn);
        assert_eq!(band_for(76.0, Some(Critical)), Warn);
        assert_eq!(band_for(70.0, Some(Critical)), Ok);
        // rising from ok does not apply the lower exit thresholds
        assert_eq!(band_for(77.0, Some(Ok)), Ok);
        assert_eq!(band_for(89.0, Some(Warn)), Warn);
        for b in [Ok, Warn, Critical] {
            assert_eq!(Band::from_u8(b.as_u8()), Some(b));
        }
        assert_eq!(Band::from_u8(9), None);
    }

    #[test]
    fn ls_walk_finds_user_var() {
        let json = r#"[{"tabs":[{"windows":[{"id":15,"user_vars":{}},
            {"id":31,"user_vars":{"ctxviz_session":"abc"}}]}]}]"#;
        assert_eq!(find_in_ls(json, "ctxviz_session", "abc").unwrap(), Some(31));
        assert_eq!(find_in_ls(json, "ctxviz_session", "zzz").unwrap(), None);
    }

    #[test]
    fn launch_output_id() {
        assert_eq!(parse_window_id("42\n"), Some(42));
        assert_eq!(parse_window_id("warn\n42\n\n"), Some(42));
        assert_eq!(parse_window_id(""), None);
    }

    #[test]
    fn window_id_arg_accepts_only_positive_integers() {
        assert_eq!(window_id_arg("12").unwrap(), 12);
        assert_eq!(window_id_arg(" 7\n").unwrap(), 7);
        for bad in ["", "0", "-1", "+3", "1.5", "abc", "12 or id:3", "id:4", "1e3", "99999999999999999999999"] {
            assert!(window_id_arg(bad).is_err(), "{bad:?}");
        }
        let k =
            Kitty { window_id: Some("-1".into()), listen_on: Some("unix:/nonexistent".into()), is_kitty_term: true };
        assert!(agent_window(&k).is_err());
    }

    #[test]
    fn launch_cwd_and_args_are_checked() {
        assert!(check_launch_cwd(Path::new("/home/u/proj")).is_ok());
        assert!(check_launch_cwd(Path::new("/home/u/$HOME")).is_err());
        assert!(check_launch_cwd(Path::new("~/proj")).is_err());
        assert!(check_launch_cwd(Path::new("current")).is_err());
        assert!(check_launch_cwd(Path::new("/a\nb")).is_err());
        assert!(check_launch_arg("store path", "/x/$y/@z").is_ok());
        assert!(check_launch_arg("store path", "@selection").is_err());
        assert!(check_launch_arg("theme", "dark\x1b]0;x\x07").is_err());
    }

    #[test]
    fn launch_args_pass_env_pairs() {
        let env = [("CTXVIZ_CURSOR_DIR", "/h/cli/.cursor"), ("CTXVIZ_STATE_DIR", "/h/cli/.cursor/ctxviz")];
        let (exe, store, cwd) = (Path::new("/bin/ctxviz"), Path::new("/s/store.db"), Path::new("/w"));
        let args = launch_args(7, "abc", exe, store, cwd, Some("dark"), None, &env).unwrap();
        let dash = args.iter().position(|a| a == "--").unwrap();
        let envs: Vec<&str> = args[..dash].iter().map(String::as_str).filter(|a| a.starts_with("--env=")).collect();
        assert_eq!(envs, ["--env=CTXVIZ_CURSOR_DIR=/h/cli/.cursor", "--env=CTXVIZ_STATE_DIR=/h/cli/.cursor/ctxviz"]);
        assert_eq!(&args[dash..], ["--", "/bin/ctxviz", "tui", "--chat", "/s/store.db", "--theme", "dark"]);
        for want in ["window_id:7", "id:7", "/w", "ctxviz_session=abc"] {
            assert!(args[..dash].iter().any(|a| a == want), "{want}: {args:?}");
        }
        assert!(launch_args(7, "abc", exe, store, cwd, None, None, &[])
            .unwrap()
            .iter()
            .all(|a| !a.starts_with("--env")));
        for bad in ["@selection", "/a/$HOME", " /lead", "/trail ", "/a\nb", "/a\x1bb"] {
            let err = launch_args(7, "abc", exe, store, cwd, None, None, &[("CTXVIZ_STATE_DIR", bad)]).unwrap_err();
            assert!(format!("{err}").contains("CTXVIZ_STATE_DIR"), "{bad:?}: {err}");
            assert!(!format!("{err}").contains('\x1b'));
        }
        assert!(launch_args(7, "abc", exe, store, Path::new("rel"), None, None, &env).is_err());
    }

    #[test]
    fn launch_refuses_bad_session_id_before_spawning() {
        let k = Kitty { window_id: Some("3".into()), listen_on: Some("unix:/nonexistent".into()), is_kitty_term: true };
        let err = launch_split(&k, "../x\x1b", Path::new("/s.db"), Path::new("/"), None, None).unwrap_err();
        assert!(format!("{err}").contains("session id"), "{err}");
        assert!(!format!("{err}").contains('\x1b'));
    }

    #[test]
    fn remote_outside_kitty_is_err() {
        assert!(remote(&Kitty::default(), &["ls"]).is_err());
        assert!(set_tab_color(&Kitty::default(), Band::Ok).is_err());
    }

    #[test]
    fn tab_colors_follow_theme() {
        use crate::theme::{Family, Mode};
        let has = |specs: [String; 4], want: &str| specs.iter().any(|s| s == want);
        let light = Theme::preset(Family::Modus, Mode::Light);
        assert!(has(tab_color_specs(Band::Warn, &light), "active_bg=#9a6700"));
        assert!(has(tab_color_specs(Band::Warn, &light), "active_fg=#eff1f5"));
        assert!(has(tab_color_specs(Band::Critical, &light), "active_bg=#cf222e"));
        let ansi = Theme::ansi();
        assert!(has(tab_color_specs(Band::Warn, &ansi), "inactive_bg=#d29922"));
        assert!(has(tab_color_specs(Band::Critical, &ansi), "active_bg=#ff7b72"));
        assert!(has(tab_color_specs(Band::Ok, &light), "active_bg=NONE"));
    }
}
