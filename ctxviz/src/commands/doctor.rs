//! `ctxviz doctor`: store diagnostics and a field dump of the latest root, so
//! `schema.rs` can be remapped if Cursor renumbers fields.

use super::StoreArgs;
use crate::model::breakdown;
use crate::store::Store;
use crate::text::sanitize_single_line;
use crate::{pb, schema};
use std::io::Write;
use std::path::Path;

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
    // Before the lookup, so a failed one still shows where it looked.
    dirs(&mut out)?;
    let path = args.store.resolve()?;
    report(&path, &args, &mut out)
}

/// The Cursor CLI config dir (and which variable chose it), the chats dir, and
/// the state dir.
fn dirs(out: &mut dyn Write) -> anyhow::Result<()> {
    match crate::store::try_cursor_dir_with_source() {
        Ok((dir, source)) => {
            writeln!(out, "cursor dir: {} (from {source})", show(&dir.display().to_string()))?;
            writeln!(out, "chats dir: {}", show(&dir.join("chats").display().to_string()))?;
        }
        Err(e) => writeln!(out, "WARN: no cursor dir: {}", show(&format!("{e:#}")))?,
    }
    match crate::session::try_state_dir() {
        Ok(dir) => writeln!(out, "state dir: {}", show(&dir.display().to_string()))?,
        Err(e) => writeln!(out, "WARN: no state dir: {}", show(&format!("{e:#}")))?,
    }
    Ok(())
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

    const SECRET: &str = "SECRET-KEY-6b1f0c9e2d";

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

    /// A store.db with only a meta row, in a unique dir removed on drop.
    fn store_with_meta(name: &str, meta: serde_json::Value) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new("ctxviz-doctor", name);
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
