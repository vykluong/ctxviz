//! Change detection thread: every 250 ms compare the store's write stamp;
//! on change probe `latestRootBlobId` and rebuild the Snapshot only when it
//! moved. Unpinned (no `--chat`), it also follows `current.json` once a second.

use crate::model::snapshot::{Snapshot, SnapshotOptions};
use crate::session::CurrentSession;
use crate::store::{self, Store, WriteStamp};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

pub const POLL: Duration = Duration::from_millis(250);
pub const FOLLOW_POLL: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub enum WatchEvent {
    Snapshot(Box<Snapshot>),
    /// Errors are reported and the thread keeps polling.
    Error(String),
    /// Unpinned mode: `current.json` now points at this store.
    Switched(PathBuf),
}

/// Store an unpinned TUI should show: `current.json`'s store, else the most
/// recently written chat anywhere.
pub fn followed_store() -> Option<PathBuf> {
    if let Some(p) = CurrentSession::read().and_then(|c| c.store).filter(|p| p.is_file()) {
        return Some(p);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    store::find_store(None, &cwd, true).ok()
}

/// Spawn the watcher. `Some(store)` pins it; `None` follows `current.json`.
/// The first Snapshot is sent immediately. The thread exits when the
/// receiver is dropped.
pub fn spawn(pinned: Option<PathBuf>) -> Receiver<WatchEvent> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("ctxviz-watch".into())
        .spawn(move || run(pinned, tx))
        .expect("spawning watch thread");
    rx
}

struct Watched {
    path: PathBuf,
    stamp: Option<WriteStamp>,
    root: Option<Option<String>>,
}

impl Watched {
    fn new(path: PathBuf) -> Watched {
        Watched { path, stamp: None, root: None }
    }

    /// Returns false when the receiver is gone.
    fn poll(&mut self, tx: &Sender<WatchEvent>) -> bool {
        let stamp = store::last_write(&self.path);
        if self.stamp.as_ref() == Some(&stamp) {
            return true;
        }
        match self.check(&self.path.clone()) {
            Ok(Some(snap)) => {
                self.stamp = Some(stamp);
                tx.send(WatchEvent::Snapshot(Box::new(snap))).is_ok()
            }
            Ok(None) => {
                self.stamp = Some(stamp);
                true
            }
            Err(e) => {
                // Leave the stamp unset so the next tick retries (e.g. mid-write).
                self.stamp = None;
                tx.send(WatchEvent::Error(format!("{e:#}"))).is_ok()
            }
        }
    }

    fn check(&mut self, path: &Path) -> anyhow::Result<Option<Snapshot>> {
        let st = Store::open(path)?;
        let root = st.latest_root_id()?;
        if self.root.as_ref() == Some(&root) {
            return Ok(None);
        }
        let snap = Snapshot::load(&st, SnapshotOptions { tree: true, timeline: true })?;
        self.root = Some(root);
        Ok(snap)
    }
}

fn run(pinned: Option<PathBuf>, tx: Sender<WatchEvent>) {
    let follow = pinned.is_none();
    let mut cur = pinned.or_else(followed_store).map(Watched::new);
    if cur.is_none() && tx.send(WatchEvent::Error("no chat store found (current.json empty)".into())).is_err() {
        return;
    }
    let mut last_follow = Instant::now();
    loop {
        if follow && last_follow.elapsed() >= FOLLOW_POLL {
            last_follow = Instant::now();
            if let Some(p) = CurrentSession::read().and_then(|c| c.store).filter(|p| p.is_file()) {
                if cur.as_ref().map(|w| &w.path) != Some(&p) {
                    if tx.send(WatchEvent::Switched(p.clone())).is_err() {
                        return;
                    }
                    cur = Some(Watched::new(p));
                }
            }
        }
        if let Some(w) = cur.as_mut() {
            if !w.poll(&tx) {
                return;
            }
        }
        std::thread::sleep(POLL);
    }
}
