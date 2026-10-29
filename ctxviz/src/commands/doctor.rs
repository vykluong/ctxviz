//! `ctxviz doctor`: setup checks (dirs, status line wiring, `current.json`),
//! then store diagnostics and a field dump of the latest root, so `schema.rs`
//! can be remapped if Cursor renumbers fields. Read-only. Exits 1 only when a
//! WARN line was printed.

use super::StoreArgs;
use crate::build_info;
use crate::model::breakdown;
use crate::session::{self, CurrentSession, CTXVIZ_STATE_DIR_ENV};
use crate::store::{self, CursorDirSource, Store};
use crate::text::sanitize_single_line;
use crate::{pb, schema};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct Args {
    #[command(flatten)]
    pub store: StoreArgs,
    /// Max nesting depth for the field dump.
    #[arg(long, default_value_t = 4)]
    pub depth: usize,
    /// Max entries shown per repeated field.
    #[arg(long, default_value_t = 3)]
    pub repeat: usize,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    let mut out = std::io::stdout().lock();
    // Before the store lookup, so a failed one still shows where it looked.
    let mut warns = setup_report(&mut out, &Setup::from_process())?;
    match args.store.resolve() {
        Ok(path) => {
            writeln!(out)?;
            match report(&path, &args, &mut out) {
                Ok(code) => warns += usize::from(code != 0),
                Err(e) => {
                    writeln!(out, "WARN: store: {}", show(&format!("{e:#}")))?;
                    warns += 1;
                }
            }
        }
        Err(e) => writeln!(out, "INFO: no store: {}", show(&format!("{e:#}")))?,
    }
    Ok(if warns > 0 { 1 } else { 0 })
}

/// Everything [`setup_report`] reads from the process, so tests can build it
/// from temp dirs instead.
pub struct Setup {
    /// The resolved Cursor CLI config dir and which variable chose it.
    pub cursor: anyhow::Result<(PathBuf, CursorDirSource)>,
    /// Every candidate dir, the resolved one first ([`store::candidate_cursor_dirs`]).
    pub candidates: Vec<(PathBuf, CursorDirSource)>,
    pub state_dir: anyhow::Result<PathBuf>,
    /// This binary, which the status line command should run.
    pub exe: Option<PathBuf>,
    /// `KITTY_LISTEN_ON` is set.
    pub in_kitty: bool,
    /// The variables that pick the dirs, as seen.
    pub vars: Vec<(&'static str, Option<OsString>)>,
    pub now: SystemTime,
}

impl Setup {
    pub fn from_process() -> Setup {
        let vars =
            [store::CTXVIZ_CURSOR_DIR_ENV, store::CURSOR_CONFIG_DIR_ENV, "XDG_CONFIG_HOME", CTXVIZ_STATE_DIR_ENV]
                .map(|k| (k, std::env::var_os(k)))
                .to_vec();
        Setup {
            cursor: store::try_cursor_dir_with_source(),
            candidates: store::candidate_cursor_dirs_from_env(),
            state_dir: session::try_state_dir(),
            exe: std::env::current_exe().ok(),
            in_kitty: std::env::var_os("KITTY_LISTEN_ON").is_some_and(|v| !v.is_empty()),
            vars,
            now: SystemTime::now(),
        }
    }
}

/// Writes lines through [`show`] and counts the WARN ones.
struct Lines<'a> {
    out: &'a mut dyn Write,
    warns: usize,
}

impl Lines<'_> {
    fn line(&mut self, s: &str) -> std::io::Result<()> {
        writeln!(self.out, "{}", show(s))
    }

    fn ok(&mut self, s: &str) -> std::io::Result<()> {
        self.line(&format!("OK: {s}"))
    }

    fn info(&mut self, s: &str) -> std::io::Result<()> {
        self.line(&format!("INFO: {s}"))
    }

    fn warn(&mut self, s: &str) -> std::io::Result<()> {
        self.warns += 1;
        self.line(&format!("WARN: {s}"))
    }
}

/// "5s ago", "3m ago", "2h ago", "4d ago", or "in the future".
fn age(now: SystemTime, t: SystemTime) -> String {
    let Ok(d) = now.duration_since(t) else { return "in the future".into() };
    match d.as_secs() {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// What `<dir>/cli-config.json` says about the status line.
enum CliConfig {
    Missing,
    /// Over the size cap or unreadable.
    Unreadable,
    Unparsable,
    NoStatusLine,
    StatusLine(serde_json::Value),
}

/// Reads only the `statusLine` key of `<dir>/cli-config.json`.
fn cli_config(dir: &Path) -> CliConfig {
    let path = dir.join("cli-config.json");
    if std::fs::symlink_metadata(&path).is_err() {
        return CliConfig::Missing;
    }
    let Some(bytes) = session::read_capped(&path) else { return CliConfig::Unreadable };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(mut m)) => match m.remove("statusLine") {
            Some(v) if !v.is_null() => CliConfig::StatusLine(v),
            _ => CliConfig::NoStatusLine,
        },
        _ => CliConfig::Unparsable,
    }
}

fn has_status_line(dir: &Path) -> bool {
    matches!(cli_config(dir), CliConfig::StatusLine(_))
}

/// `current.json` in `state_dir`, if readable.
fn current_in(state_dir: &Path) -> Option<CurrentSession> {
    CurrentSession::read_from(&state_dir.join("current.json"))
}

/// `p` canonicalized, or as given when that fails.
fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The setup checks: version, variables, dirs, `cli-config.json` status line
/// wiring, other candidate dirs, chats, `current.json`, stale temp files, and
/// Kitty. Read-only. Returns how many WARN lines were written.
pub fn setup_report(out: &mut impl Write, s: &Setup) -> anyhow::Result<usize> {
    let mut l = Lines { out, warns: 0 };
    l.line(&format!("ctxviz {} ({})", build_info::VERSION, build_info::BUILD))?;
    for (k, v) in &s.vars {
        let v = v.as_ref().map_or_else(|| "-".into(), |v| v.to_string_lossy().into_owned());
        l.line(&format!("{k}: {v}"))?;
    }

    let cursor = match &s.cursor {
        Ok((dir, source)) => {
            l.line(&format!("cursor dir: {} (from {source})", dir.display()))?;
            if !dir.is_dir() {
                l.warn(&format!("cursor dir {} is not a directory", dir.display()))?;
            }
            Some(dir.as_path())
        }
        Err(e) => {
            l.warn(&format!("no cursor dir: {e:#}"))?;
            None
        }
    };
    if let Some(dir) = cursor {
        status_line(&mut l, s, dir)?;
        other_candidates(&mut l, s, dir)?;
        chats(&mut l, s, &dir.join("chats"))?;
    }
    state(&mut l, s, cursor)?;

    if s.in_kitty {
        l.ok("KITTY_LISTEN_ON set")?;
    } else {
        l.info("not in Kitty (ctxviz open unavailable)")?;
    }
    Ok(l.warns)
}

/// The resolved dir's `cli-config.json` runs this binary as its status line.
fn status_line(l: &mut Lines, s: &Setup, dir: &Path) -> std::io::Result<()> {
    let path = dir.join("cli-config.json");
    let p = path.display();
    let sl = match cli_config(dir) {
        CliConfig::Missing => {
            return l.warn(&format!(
                "{p} is missing, so no statusLine: the Cursor CLI has not used this dir, or it is the wrong dir"
            ));
        }
        CliConfig::Unreadable => return l.warn(&format!("{p} is unreadable or over 1 MiB")),
        CliConfig::Unparsable => return l.warn(&format!("{p} is not a JSON object")),
        CliConfig::NoStatusLine => {
            l.warn(&format!("{p} has no statusLine; add:"))?;
            let exe = s.exe.as_deref().map_or_else(|| "ctxviz".into(), |e| e.display().to_string());
            let block = serde_json::json!({
                "statusLine": { "type": "command", "command": format!("{exe} statusline"), "padding": 2 }
            });
            for line in serde_json::to_string_pretty(&block).unwrap_or_default().lines() {
                l.line(&format!("  {line}"))?;
            }
            return Ok(());
        }
        CliConfig::StatusLine(v) => v,
    };
    let kind = sl.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if kind != "command" {
        return l.warn(&format!("statusLine.type in {p} is {kind:?}, expected \"command\""));
    }
    let command = sl.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let Some(prog) = command.split_whitespace().next().map(Path::new) else {
        return l.warn(&format!("statusLine.command in {p} is empty"));
    };
    if !prog.is_file() {
        return l.warn(&format!("statusLine.command {} in {p} is not an existing file", prog.display()));
    }
    let Some(exe) = s.exe.as_deref() else {
        return l.info(&format!("statusLine in {p} runs {}; this binary's path is unknown", prog.display()));
    };
    let (got, want) = (canon(prog), canon(exe));
    if got == want {
        return l.ok(&format!("statusLine in {p} runs this binary"));
    }
    // The canonical path too, when it differs from the one given.
    let both = |given: &Path, real: &Path| {
        if given == real {
            given.display().to_string()
        } else {
            format!("{} ({})", given.display(), real.display())
        }
    };
    l.warn(&format!("statusLine in {p} runs {}, not this binary {}", both(prog, &got), both(exe, &want)))
}

/// The other candidate dirs, where the agent may really be running.
fn other_candidates(l: &mut Lines, s: &Setup, dir: &Path) -> std::io::Result<()> {
    let here_has = has_status_line(dir);
    let state = s.state_dir.as_deref().ok();
    let here_ts = state.and_then(current_in).map(|c| c.ts);
    for (other, source) in s.candidates.iter().filter(|(d, _)| d != dir) {
        l.line(&format!("other cursor dir: {} (from {source})", other.display()))?;
        if !here_has && has_status_line(other) {
            l.warn(&format!(
                "statusLine is configured in {}/cli-config.json but not in {}/cli-config.json",
                other.display(),
                dir.display()
            ))?;
        }
        let other_state = other.join("ctxviz");
        if state.is_some_and(|d| canon(d) == canon(&other_state)) {
            continue;
        }
        if current_in(&other_state).is_some_and(|cur| here_ts.is_none_or(|t| cur.ts > t)) {
            let o = other.display();
            let r = dir.display();
            let than = match here_ts {
                Some(_) => format!("is newer than {r}/ctxviz/current.json"),
                None => format!("exists and {r} has none"),
            };
            l.warn(&format!(
                "{o}/ctxviz/current.json {than}: if agent runs under {o}, export CTXVIZ_CURSOR_DIR={o}; \
                 if it is stale, start agent once under {r} and it will be superseded"
            ))?;
        }
    }
    Ok(())
}

/// Projects and stores under the chats dir (a two-level walk).
fn chats(l: &mut Lines, s: &Setup, chats: &Path) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(chats) else {
        return l.warn(&format!("chats dir {} is missing", chats.display()));
    };
    let projects: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    let stores: Vec<PathBuf> = projects
        .iter()
        .filter_map(|p| std::fs::read_dir(p).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path().join("store.db")))
        .filter(|p| p.is_file())
        .collect();
    let summary = format!("chats dir: {} ({} projects, {} stores", chats.display(), projects.len(), stores.len());
    if stores.is_empty() {
        l.line(&format!("{summary})"))?;
        return l.info("no chat has a checkpoint yet: send one message in agent");
    }
    match stores.iter().filter_map(|p| store::last_write(p).latest()).max() {
        Some(t) => l.ok(&format!("{summary}, newest written {})", age(s.now, t))),
        None => l.ok(&format!("{summary})")),
    }
}

/// The state dir, `current.json`, and leftover temp files.
fn state(l: &mut Lines, s: &Setup, cursor: Option<&Path>) -> std::io::Result<()> {
    let dir = match &s.state_dir {
        Ok(d) => d,
        Err(e) => return l.warn(&format!("no state dir: {e:#}")),
    };
    l.line(&format!("state dir: {}", dir.display()))?;
    let path = dir.join("current.json");
    match current_in(dir) {
        None if std::fs::symlink_metadata(&path).is_err() => {
            l.info(&format!("no {}: the status line has not run with this state dir", path.display()))?;
        }
        None => l.warn(&format!("{} is unreadable or invalid", path.display()))?,
        Some(cur) => {
            let written = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(cur.ts);
            let store = match &cur.store {
                Some(p) if p.is_file() => "store exists",
                Some(_) => "store missing",
                None => "no store",
            };
            l.ok(&format!("current.json: session {}, written {}, {store}", cur.session_id, age(s.now, written)))?;
            if let (Some(rec), Some(here)) = (&cur.cursor_dir, cursor) {
                if canon(rec) != canon(here) {
                    l.warn(&format!(
                        "current.json was written for cursor dir {r}, not {}; export CTXVIZ_CURSOR_DIR={r}",
                        here.display(),
                        r = rec.display()
                    ))?;
                }
            }
            let this = build_info::VERSION;
            match cur.ctxviz_version.as_deref() {
                Some(v) if v == this => {}
                Some(v) => l.warn(&format!(
                    "current.json was written by ctxviz {v}, this is {this}: restart agent after reinstalling"
                ))?,
                None => l.warn(&format!(
                    "current.json was written by an older ctxviz (no version recorded), this is {this}: \
                     restart agent after reinstalling"
                ))?,
            }
        }
    }
    let stale = stale_tmp_files(dir, s.now);
    if stale > 0 {
        l.info(&format!("{stale} stale temp files in {} (ctxviz tui removes them on start)", dir.display()))?;
    }
    Ok(())
}

/// Regular files named like [`session::is_tmp_name`] and older than
/// [`session::TMP_STALE_AFTER`]. Counts only: doctor removes nothing.
fn stale_tmp_files(dir: &Path, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| session::is_tmp_name(&e.file_name().to_string_lossy()))
        .filter_map(|e| std::fs::symlink_metadata(e.path()).ok())
        .filter(|m| m.is_file())
        .filter(|m| {
            m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|a| a > session::TMP_STALE_AFTER)
        })
        .count()
}

/// One-line, control-free rendering of a store-derived value.
fn show(s: &str) -> std::borrow::Cow<'_, str> {
    sanitize_single_line(s)
}

/// The diagnostics for the store at `path`, written to `out`. Every
/// store-derived value is sanitized; `blobEncryptionKey` is reported only as
/// present or absent, never printed.
pub fn report(path: &Path, args: &Args, out: &mut dyn Write) -> anyhow::Result<i32> {
    writeln!(out, "store: {}", show(&path.display().to_string()))?;
    let stamp = crate::store::last_write(path);
    writeln!(out, "wal size: {:?}", stamp.wal_size)?;
    let store = Store::open(path)?;
    let meta = store.meta()?;
    let mut keys: Vec<_> = meta.raw.keys().map(|k| show(k).into_owned()).collect();
    keys.sort();
    writeln!(out, "meta keys: {}", keys.join(", "))?;
    writeln!(out, "agentId: {}", show(meta.agent_id.as_deref().unwrap_or("-")))?;
    writeln!(out, "name: {}", show(meta.name.as_deref().unwrap_or("-")))?;
    // Presence only: the key itself must never reach the terminal or logs.
    writeln!(out, "blobEncryptionKey present: {}", meta.blob_encryption_key.as_deref().is_some_and(|k| !k.is_empty()))?;
    let n: i64 = store.conn().query_row("select count(*) from blobs", [], |r| r.get(0))?;
    writeln!(out, "blobs: {n} (max rowid {})", store.max_rowid()?)?;

    let mut problems = 0;
    let Some(root_id) = meta.latest_root_blob_id.as_deref() else {
        writeln!(out, "WARN: meta has no latestRootBlobId (new chat?)")?;
        return Ok(1);
    };
    writeln!(out, "latestRootBlobId: {}", show(root_id))?;
    let Some(root) = store.blob(root_id)? else {
        writeln!(out, "WARN: latest root blob is missing from blobs")?;
        return Ok(1);
    };
    let is_root = schema::is_root(&root);
    writeln!(
        out,
        "latest root: {} bytes, structural root test: {}",
        root.len(),
        if is_root { "pass" } else { "FAIL" }
    )?;
    if !is_root {
        problems += 1;
    }
    if !schema::looks_like_json(&root) && pb::decode(&root).is_err() {
        writeln!(out, "WARN: latest root is neither JSON nor decodable protobuf; blobs may be encrypted")?;
        problems += 1;
    }

    // Sample a few message blobs: they should be JSON.
    if let Ok(info) = breakdown::decode_root(&root) {
        writeln!(
            out,
            "message ids: {}, createdAt (field {}): {:?}",
            info.message_ids.len(),
            schema::root::CREATED_AT,
            info.created_at
        )?;
        match &info.details {
            Some(td) => writeln!(
                out,
                "token details: used {} / max {}, {} categories",
                td.used_tokens,
                td.max_tokens,
                td.categories.len()
            )?,
            None => writeln!(out, "WARN: latest root has no token details (field {})", schema::root::TOKEN_DETAILS)?,
        }
        let sample: Vec<_> = info.message_ids.iter().take(5).collect();
        let mut opaque = 0;
        for id in &sample {
            if let Some(b) = store.blob(id)? {
                if !schema::looks_like_json(&b) {
                    opaque += 1;
                }
            }
        }
        if opaque > 0 {
            writeln!(
                out,
                "WARN: {opaque}/{} sampled message blobs are not JSON; blobs may be encrypted",
                sample.len()
            )?;
            problems += 1;
        }
    }

    writeln!(out, "\nroot field tree (number:wire_type, ×count, len, preview):")?;
    dump(out, &root, 1, args.depth, args.repeat)?;
    Ok(if problems > 0 { 1 } else { 0 })
}

fn wt_name(wt: u8) -> &'static str {
    match wt {
        0 => "varint",
        1 => "fixed64",
        2 => "bytes",
        3 => "group",
        5 => "fixed32",
        _ => "?",
    }
}

fn printable(b: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(b).ok()?;
    (!s.is_empty() && s.chars().all(|c| !c.is_control() || c == '\n' || c == '\t' || c == '\r')).then_some(s)
}

fn preview(b: &[u8]) -> String {
    if let Some(s) = printable(b) {
        let s: String = s.chars().take(60).collect::<String>().replace('\n', "⏎");
        // `{:?}` escapes C0/C1, but not every character a terminal treats as
        // a line break (U+2028/9): sanitize the quoted form too.
        return show(&format!("{s:?}")).into_owned();
    }
    let head = schema::hex(&b[..b.len().min(16)]);
    if b.len() > 16 {
        format!("0x{head}…")
    } else {
        format!("0x{head}")
    }
}

fn dump(out: &mut dyn Write, buf: &[u8], level: usize, max_depth: usize, repeat: usize) -> std::io::Result<()> {
    let pad = "  ".repeat(level);
    let fields = match pb::decode(buf) {
        Ok(f) => f,
        Err(e) => {
            return writeln!(out, "{pad}<undecodable: {}>", show(&e.to_string()));
        }
    };
    // Group runs of the same field number.
    let mut i = 0;
    while i < fields.len() {
        let f = fields[i];
        let mut j = i;
        while j < fields.len() && fields[j].number == f.number {
            j += 1;
        }
        let run = &fields[i..j];
        let count = if run.len() > 1 { format!(" ×{}", run.len()) } else { String::new() };
        for (k, g) in run.iter().enumerate().take(repeat) {
            let tag =
                if k == 0 { format!("{}:{}{}", g.number, wt_name(g.wire_type), count) } else { "  ...".to_string() };
            match g.value {
                pb::Value::Varint(v) => writeln!(out, "{pad}{tag} = {v}")?,
                pb::Value::Fixed64(v) => writeln!(out, "{pad}{tag} = {v} (f64 {})", f64::from_bits(v))?,
                pb::Value::Fixed32(v) => writeln!(out, "{pad}{tag} = {v} (f32 {})", f32::from_bits(v))?,
                pb::Value::Bytes(b) | pb::Value::Group(b) => {
                    writeln!(out, "{pad}{tag} len={} {}", b.len(), preview(b))?;
                    let nested = printable(b).is_none() && !b.is_empty() && pb::decode(b).is_ok();
                    let is_id = level == 1 && g.number == schema::root::MESSAGE_IDS;
                    if nested && level < max_depth && !is_id {
                        dump(out, b, level + 1, max_depth, repeat)?;
                    }
                }
            }
        }
        i = j;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    const SECRET: &str = "SECRET-KEY-6b1f0c9e2d";

    /// The test binary, standing in for the installed ctxviz.
    fn exe() -> PathBuf {
        std::env::current_exe().unwrap()
    }

    fn write_json(path: &Path, v: &serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    fn status_line_config(command: &str) -> serde_json::Value {
        serde_json::json!({ "version": 1, "statusLine": { "type": "command", "command": command, "padding": 2 } })
    }

    fn current(dir: &Path, ts: u64, version: Option<&str>) -> CurrentSession {
        CurrentSession {
            session_id: "sess-1".into(),
            store: Some(dir.join("chats/proj/agent-1/store.db")),
            cursor_dir: Some(dir.to_path_buf()),
            ctxviz_version: version.map(String::from),
            ts,
            ..Default::default()
        }
    }

    /// A setup with nothing to warn about: a cursor dir with one store, a
    /// status line running [`exe`], and a fresh `current.json` from this
    /// version. Returns the cursor dir too.
    fn ok_setup(root: &TempDir) -> (Setup, PathBuf) {
        let dir = root.join("cursor");
        let store = dir.join("chats/proj/agent-1/store.db");
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::write(&store, b"").unwrap();
        write_json(&dir.join("cli-config.json"), &status_line_config(&format!("{} statusline", exe().display())));
        let state = dir.join("ctxviz");
        std::fs::create_dir_all(&state).unwrap();
        current(&dir, CurrentSession::now_ts(), Some(build_info::VERSION))
            .write_to(&state.join("current.json"))
            .unwrap();
        let setup = Setup {
            cursor: Ok((dir.clone(), CursorDirSource::Home)),
            candidates: vec![(dir.clone(), CursorDirSource::Home)],
            state_dir: Ok(state),
            exe: Some(exe()),
            in_kitty: true,
            vars: vec![("CTXVIZ_CURSOR_DIR", None), ("CURSOR_CONFIG_DIR", Some("/x\x1b[2J".into()))],
            now: SystemTime::now(),
        };
        (setup, dir)
    }

    /// The WARN count and the output, which must be clean.
    fn run_setup(s: &Setup) -> (usize, String) {
        let mut out = Vec::new();
        let warns = setup_report(&mut out, s).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(crate::text::is_clean(&text), "{text:?}");
        assert_eq!(warns, text.lines().filter(|l| l.starts_with("WARN: ")).count(), "{text}");
        (warns, text)
    }

    #[test]
    fn setup_all_ok() {
        let root = TempDir::new("doctor-ok");
        let (s, _) = ok_setup(&root);
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 0, "{text}");
        assert!(text.starts_with(&format!("ctxviz {} ({})\n", build_info::VERSION, build_info::BUILD)), "{text}");
        assert!(text.contains("CTXVIZ_CURSOR_DIR: -\n"), "{text}");
        assert!(text.contains("CURSOR_CONFIG_DIR: /x\u{FFFD}[2J\n"), "{text}");
        assert!(text.contains("(from ~/.cursor)"), "{text}");
        assert!(text.contains("runs this binary"), "{text}");
        assert!(text.contains("(1 projects, 1 stores, newest written"), "{text}");
        assert!(text.contains("OK: current.json: session sess-1, written 0s ago, store exists"), "{text}");
        assert!(text.contains("OK: KITTY_LISTEN_ON set"), "{text}");
        assert!(!text.contains("INFO:"), "{text}");
    }

    #[test]
    fn setup_info_paths_do_not_warn() {
        let root = TempDir::new("doctor-info");
        let (mut s, dir) = ok_setup(&root);
        s.in_kitty = false;
        std::fs::remove_file(dir.join("chats/proj/agent-1/store.db")).unwrap();
        let state = dir.join("ctxviz");
        std::fs::remove_file(state.join("current.json")).unwrap();
        let old = std::fs::File::create(state.join(".current.json.tmp.1.2.0")).unwrap();
        old.set_modified(SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        std::fs::write(state.join(".current.json.tmp.1.3.0"), b"fresh").unwrap();
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 0, "{text}");
        assert!(text.contains("INFO: not in Kitty (ctxviz open unavailable)"), "{text}");
        assert!(text.contains("INFO: no chat has a checkpoint yet: send one message in agent"), "{text}");
        assert!(text.contains("(1 projects, 0 stores)"), "{text}");
        assert!(text.contains("current.json: the status line has not run with this state dir"), "{text}");
        assert!(text.contains("INFO: 1 stale temp files in"), "{text}");
        assert!(text.contains("ctxviz tui removes them on start"), "{text}");
        // Read-only: the stale file is still there.
        assert!(state.join(".current.json.tmp.1.2.0").exists());
    }

    #[test]
    fn setup_warns_on_cli_config_problems() {
        let root = TempDir::new("doctor-cli-config");
        let (s, dir) = ok_setup(&root);
        let cfg = dir.join("cli-config.json");
        let other = root.join("other-ctxviz");
        std::fs::write(&other, b"").unwrap();
        let other_cfg = status_line_config(&format!("{} statusline", other.display())).to_string();
        let cases: [(Option<&str>, &str); 7] = [
            (None, "the Cursor CLI has not used this dir, or it is the wrong dir"),
            (Some("{not json"), "is not a JSON object"),
            (Some(r#"{"version": 1}"#), "has no statusLine; add:"),
            (Some(r#"{"statusLine": {"type": "text\u001b[31m", "command": "x"}}"#), "expected \"command\""),
            (Some(r#"{"statusLine": {"type": "command", "command": "  "}}"#), "statusLine.command in"),
            (
                Some(r#"{"statusLine": {"type": "command", "command": "/nonexistent/ctxviz x"}}"#),
                "/nonexistent/ctxviz in",
            ),
            (Some(&other_cfg), "not this binary"),
        ];
        for (i, (body, want)) in cases.into_iter().enumerate() {
            match body {
                Some(b) => std::fs::write(&cfg, b).unwrap(),
                None => std::fs::remove_file(&cfg).unwrap(),
            }
            let (warns, text) = run_setup(&s);
            assert_eq!(warns, 1, "case {i}: {text}");
            assert!(text.contains(want), "case {i}: {text}");
            if i == 2 {
                assert!(text.contains(&format!("\"command\": \"{} statusline\",", exe().display())), "{text}");
                assert!(text.contains("\"type\": \"command\""), "{text}");
                assert!(text.contains("\"padding\": 2"), "{text}");
            }
            if i == 6 {
                assert!(text.contains(&other.display().to_string()), "{text}");
                assert!(text.contains(&exe().display().to_string()), "{text}");
            }
        }
    }

    #[test]
    fn setup_warns_about_other_candidates() {
        let root = TempDir::new("doctor-other");
        let (mut s, dir) = ok_setup(&root);
        let other = root.join("other");
        s.candidates.push((other.clone(), CursorDirSource::ConfigDirEnv));
        // Configured only in the other dir.
        write_json(&dir.join("cli-config.json"), &serde_json::json!({ "version": 1 }));
        write_json(&other.join("cli-config.json"), &status_line_config("/bin/sh statusline"));
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 2, "{text}");
        let want = format!(
            "WARN: statusLine is configured in {}/cli-config.json but not in {}/cli-config.json",
            other.display(),
            dir.display()
        );
        assert!(text.contains(&want), "{text}");
        assert!(text.contains(&format!("other cursor dir: {} (from $CURSOR_CONFIG_DIR)", other.display())), "{text}");

        // A newer current.json in the other dir: the status line runs there.
        write_json(&dir.join("cli-config.json"), &status_line_config(&format!("{} statusline", exe().display())));
        let here = dir.join("ctxviz/current.json");
        current(&dir, CurrentSession::now_ts() - 100, Some(build_info::VERSION)).write_to(&here).unwrap();
        std::fs::create_dir_all(other.join("ctxviz")).unwrap();
        let there = other.join("ctxviz/current.json");
        current(&other, CurrentSession::now_ts(), Some(build_info::VERSION)).write_to(&there).unwrap();
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 1, "{text}");
        let want = format!(
            "WARN: {o}/ctxviz/current.json is newer than {r}/ctxviz/current.json: \
             if agent runs under {o}, export CTXVIZ_CURSOR_DIR={o}; \
             if it is stale, start agent once under {r} and it will be superseded",
            o = other.display(),
            r = dir.display()
        );
        assert!(text.contains(&want), "{text}");

        // No current.json here at all.
        std::fs::remove_file(&here).unwrap();
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 1, "{text}");
        let want = format!(
            "WARN: {o}/ctxviz/current.json exists and {r} has none: if agent runs under {o}",
            o = other.display(),
            r = dir.display()
        );
        assert!(text.contains(&want), "{text}");
        current(&dir, CurrentSession::now_ts() - 100, Some(build_info::VERSION)).write_to(&here).unwrap();

        // An older one is fine.
        current(&other, CurrentSession::now_ts() - 1000, Some(build_info::VERSION)).write_to(&there).unwrap();
        assert_eq!(run_setup(&s).0, 0);
    }

    #[test]
    fn setup_warns_on_current_json() {
        let root = TempDir::new("doctor-current");
        let (s, dir) = ok_setup(&root);
        let path = dir.join("ctxviz/current.json");
        let now = CurrentSession::now_ts();

        let elsewhere = root.join("elsewhere");
        current(&elsewhere, now, Some(build_info::VERSION)).write_to(&path).unwrap();
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 1, "{text}");
        assert!(text.contains(&format!("export CTXVIZ_CURSOR_DIR={}", elsewhere.display())), "{text}");
        assert!(text.contains("store missing"), "{text}");

        for version in [Some("0.0.1"), None] {
            current(&dir, now, version).write_to(&path).unwrap();
            let (warns, text) = run_setup(&s);
            assert_eq!(warns, 1, "{version:?}: {text}");
            assert!(text.contains("restart agent after reinstalling"), "{text}");
        }

        std::fs::write(&path, br#"{"session_id": "../x", "ts": 1}"#).unwrap();
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 1, "{text}");
        assert!(text.contains("current.json is unreadable or invalid"), "{text}");
    }

    #[test]
    fn setup_warns_on_missing_dirs() {
        let root = TempDir::new("doctor-dirs");
        let (mut s, _) = ok_setup(&root);
        let missing = root.join("cur\x1b]52;c;QUJD\x07sor");
        s.cursor = Ok((missing.clone(), CursorDirSource::CtxvizEnv));
        s.candidates = vec![(missing, CursorDirSource::CtxvizEnv)];
        s.state_dir = Ok(root.join("state"));
        let (warns, text) = run_setup(&s);
        // Not a directory, no cli-config.json, no chats dir.
        assert_eq!(warns, 3, "{text}");
        assert!(text.contains("is not a directory"), "{text}");
        assert!(text.contains("is missing"), "{text}");
        assert!(text.contains("(from $CTXVIZ_CURSOR_DIR)"), "{text}");

        s.cursor = Err(anyhow::anyhow!("HOME is not set"));
        s.candidates.clear();
        s.state_dir = Err(anyhow::anyhow!("HOME is not set\x1b[2J"));
        let (warns, text) = run_setup(&s);
        assert_eq!(warns, 2, "{text}");
        assert!(text.contains("WARN: no cursor dir: HOME is not set"), "{text}");
        assert!(text.contains("WARN: no state dir: HOME is not set"), "{text}");
    }

    /// A store.db with only a meta row, in a unique dir removed on drop.
    fn store_with_meta(name: &str, meta: serde_json::Value) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new(&format!("doctor-{name}"));
        let path = dir.0.join("store.db");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "create table blobs(id text primary key, data blob); create table meta(key text primary key, value text);",
        )
        .unwrap();
        let hex = schema::hex(&serde_json::to_vec(&meta).unwrap());
        c.execute("insert into meta(key, value) values (?, ?)", rusqlite::params![schema::META_KEY, hex]).unwrap();
        drop(c);
        (dir, path)
    }

    #[test]
    fn never_prints_the_key_and_sanitizes_meta() {
        let (_dir, path) = store_with_meta(
            "key",
            serde_json::json!({
                "agentId": "\u{9b}31magent",
                "name": "\x1b]52;c;QUJD\x07chat",
                "blobEncryptionKey": SECRET,
                "latestRootBlobId": "\x1bP@kitty-cmd{\"cmd\":\"launch\"}\x1b\\",
                "\x1b[2Jkey": 1,
            }),
        );
        let mut out = Vec::new();
        let code = report(&path, &Args::default(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(code, 1, "{text}");
        assert!(!text.contains(SECRET), "{text}");
        assert!(text.contains("blobEncryptionKey present: true"), "{text}");
        assert!(crate::text::is_clean(&text), "{text:?}");
        assert!(text.contains("name: \u{FFFD}]52;c;QUJD\u{FFFD}chat"), "{text}");
    }
}
