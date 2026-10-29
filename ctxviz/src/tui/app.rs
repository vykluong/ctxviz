//! TUI state: snapshot, panel focus, explorer tree view state, key handling.
//! Pure logic (no terminal), so it is unit tested directly.

use crate::model::snapshot::Snapshot;
use crate::model::timeline::{Checkpoint, Timeline};
use crate::model::tree::{Node, Tree};
use crate::tui::watch::WatchEvent;
use crate::tui::LayoutOpts;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Tree,
    Timeline,
    Detail,
}

impl Focus {
    pub fn next(self) -> Focus {
        match self {
            Focus::Tree => Focus::Timeline,
            Focus::Timeline => Focus::Detail,
            Focus::Detail => Focus::Tree,
        }
    }
}

/// One visible explorer row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Index into `Tree::nodes`.
    pub idx: usize,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// Flatten `tree` into visible rows. Children are ordered by `Tree::ordered`
/// (conversation chronological, everything else by size). With a non-empty
/// `filter` (case-insensitive label substring) only matching nodes and their
/// ancestors are shown; ancestors of a match are expanded automatically, and
/// a matching node's own children appear only when it is expanded.
pub fn visible_rows(tree: &Tree, expanded: &HashSet<String>, filter: &str) -> Vec<Row> {
    let kids = tree.children();
    let needle = filter.to_lowercase();
    // keep[i]: node i matches or has a matching descendant.
    let keep: Option<Vec<bool>> = (!needle.is_empty()).then(|| {
        let mut keep: Vec<bool> = tree.nodes.iter().map(|n| n.label.to_lowercase().contains(&needle)).collect();
        let index: HashMap<&str, usize> = tree.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        for i in 0..tree.nodes.len() {
            if !keep[i] || !tree.nodes[i].label.to_lowercase().contains(&needle) {
                continue;
            }
            let mut p = tree.nodes[i].parent_id.as_deref();
            while let Some(pid) = p {
                let Some(&pi) = index.get(pid) else { break };
                keep[pi] = true;
                p = tree.nodes[pi].parent_id.as_deref();
            }
        }
        keep
    });
    let matches = |i: usize| needle.is_empty() || tree.nodes[i].label.to_lowercase().contains(&needle);

    let mut rows = Vec::new();
    // (parent id, depth, show all children regardless of filter)
    #[allow(clippy::too_many_arguments)]
    fn walk(
        tree: &Tree,
        kids: &HashMap<Option<&str>, Vec<usize>>,
        parent: Option<&str>,
        depth: usize,
        all: bool,
        expanded: &HashSet<String>,
        keep: &Option<Vec<bool>>,
        matches: &dyn Fn(usize) -> bool,
        rows: &mut Vec<Row>,
    ) {
        let Some(ch) = kids.get(&parent) else { return };
        for i in tree.ordered(parent, ch) {
            let n = &tree.nodes[i];
            if !all && keep.as_ref().is_some_and(|k| !k[i]) {
                continue;
            }
            let has_children = kids.get(&Some(n.id.as_str())).is_some_and(|c| !c.is_empty());
            let user_open = expanded.contains(&n.id);
            // Under a filter an ancestor of a match is forced open.
            let forced = !all && keep.is_some() && !matches(i);
            let open = has_children && (user_open || forced);
            rows.push(Row { idx: i, depth, has_children, expanded: open });
            if open {
                // Below a user-expanded match (or inside an unfiltered view) show everything.
                let child_all = all || keep.is_none() || (matches(i) && user_open);
                walk(tree, kids, Some(&n.id), depth + 1, child_all, expanded, keep, matches, rows);
            }
        }
    }
    walk(tree, &kids, None, 0, false, expanded, &keep, &matches, &mut rows);
    rows
}

/// Something the event loop must do outside the pure state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Open this text in `$PAGER` (Kitty overlay).
    OpenPager {
        label: String,
        text: String,
    },
}

/// Local OSC 99 threshold override cycled by `n`: a band crossing notifies
/// only when the percent is at least this value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NotifyThreshold {
    #[default]
    P80,
    P90,
    P95,
    Off,
}

impl NotifyThreshold {
    pub fn next(self) -> NotifyThreshold {
        match self {
            NotifyThreshold::P80 => NotifyThreshold::P90,
            NotifyThreshold::P90 => NotifyThreshold::P95,
            NotifyThreshold::P95 => NotifyThreshold::Off,
            NotifyThreshold::Off => NotifyThreshold::P80,
        }
    }

    pub fn percent(self) -> Option<f64> {
        match self {
            NotifyThreshold::P80 => Some(80.0),
            NotifyThreshold::P90 => Some(90.0),
            NotifyThreshold::P95 => Some(95.0),
            NotifyThreshold::Off => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NotifyThreshold::P80 => "80%",
            NotifyThreshold::P90 => "90%",
            NotifyThreshold::P95 => "95%",
            NotifyThreshold::Off => "off",
        }
    }
}

#[derive(Default)]
pub struct App {
    pub snapshot: Option<Snapshot>,
    /// Store currently shown (from the watch thread).
    pub store: Option<PathBuf>,
    pub focus: Focus,
    /// Expanded node ids.
    pub expanded: HashSet<String>,
    /// Selected node id in the explorer.
    pub selected: Option<String>,
    /// First visible explorer row; the tree panel adjusts it while drawing.
    pub scroll: Cell<usize>,
    pub filter: String,
    /// `/` typed: keys edit `filter` until enter/esc.
    pub filter_input: bool,
    /// Detail text scroll (lines).
    pub detail_scroll: u16,
    /// Selected checkpoint index into `Timeline::checkpoints` (Timeline focus,
    /// j/k/g/G). `None` follows the latest checkpoint.
    pub timeline_selected: Option<usize>,
    /// Transient message shown in the footer (errors, "not in kitty", ...).
    pub status: Option<String>,
    pub should_quit: bool,
    /// OSC 99 notify threshold (`n` cycles it).
    pub notify_threshold: NotifyThreshold,
    /// Exact explorer width set by `[`/`]` (wins over `explorer_pct`).
    pub explorer_cols: Option<u16>,
    /// Explorer width as a percent of the body (`--split`).
    pub explorer_pct: Option<u8>,
    /// Exact timeline height (`--timeline-rows`).
    pub timeline_rows: Option<u16>,
    /// Body width at the last draw, so `[`/`]` nudge from the effective width.
    pub body_width: Cell<u16>,
    /// `tui.json` to save `[`/`]` widths to; `None` disables persistence.
    pub state_path: Option<PathBuf>,
    rows: Vec<Row>,
    first_snapshot: bool,
}

impl App {
    pub fn new() -> App {
        App { first_snapshot: true, ..App::default() }
    }

    pub fn layout_opts(&self) -> LayoutOpts {
        LayoutOpts {
            explorer_cols: self.explorer_cols,
            explorer_pct: self.explorer_pct,
            timeline_rows: self.timeline_rows,
        }
    }

    /// Widen (`delta > 0`) or narrow the explorer from its current effective
    /// width, clamped to 24..=80% of the last drawn body width.
    fn nudge_explorer(&mut self, delta: i32) {
        let w = self.body_width.get();
        if w == 0 {
            return;
        }
        let cur = crate::tui::explorer_width(w, &self.layout_opts()) as i32;
        let next = (cur + delta).clamp(0, u16::MAX as i32) as u16;
        self.explorer_cols = Some(crate::tui::clamp_explorer(next, w));
        self.explorer_pct = None;
        self.save_state();
    }

    /// Best-effort write of the remembered layout to `state_path`; errors
    /// are ignored so state I/O never disturbs the TUI.
    fn save_state(&self) {
        let Some(path) = &self.state_path else { return };
        let mut st = crate::session::TuiState::read_from(path).unwrap_or_default();
        st.explorer_cols = self.explorer_cols;
        let _ = st.write_to(path);
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.snapshot.as_ref().map(|s| &s.tree)
    }

    pub fn timeline(&self) -> Option<&Timeline> {
        self.snapshot.as_ref().and_then(|s| s.timeline.as_ref())
    }

    /// The checkpoint the timeline cursor is on (latest when not moved).
    pub fn selected_checkpoint(&self) -> Option<&Checkpoint> {
        let cps = &self.timeline()?.checkpoints;
        match self.timeline_selected {
            Some(i) => cps.get(i.min(cps.len().saturating_sub(1))),
            None => cps.last(),
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn selected_row(&self) -> Option<usize> {
        let tree = self.tree()?;
        let id = self.selected.as_deref()?;
        self.rows.iter().position(|r| tree.nodes[r.idx].id == id)
    }

    pub fn selected_node(&self) -> Option<&Node> {
        let tree = self.tree()?;
        let id = self.selected.as_deref()?;
        tree.nodes.iter().find(|n| n.id == id)
    }

    pub fn on_watch(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::Snapshot(s) => {
                self.store = Some(s.store.clone());
                self.set_snapshot(*s);
                self.status = None;
            }
            WatchEvent::Error(e) => self.status = Some(e),
            WatchEvent::Switched(p) => {
                self.status = Some(format!("following {}", p.display()));
                self.store = Some(p);
                self.snapshot = None;
                self.expanded.clear();
                self.selected = None;
                self.timeline_selected = None;
                self.first_snapshot = true;
                self.rows.clear();
            }
        }
    }

    /// Replace the snapshot, keeping the selection on the same node id. If
    /// that node vanished (or is now hidden), select the nearest visible row.
    pub fn set_snapshot(&mut self, snap: Snapshot) {
        let old_row = self.selected_row();
        let old_node = self.selected_node().cloned();
        if self.first_snapshot {
            self.first_snapshot = false;
            let kids = snap.tree.children();
            for &i in kids.get(&None).map(Vec::as_slice).unwrap_or_default() {
                self.expanded.insert(snap.tree.nodes[i].id.clone());
            }
        }
        self.snapshot = Some(snap);
        self.refresh_rows();
        if self.selected_row().is_some() {
            return;
        }
        // Hidden under a collapsed ancestor: select the closest visible ancestor.
        if let (Some(tree), Some(_)) = (self.tree(), &old_node) {
            let index: HashMap<&str, &Node> = tree.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            let mut p = self.selected.as_deref().and_then(|id| index.get(id)).and_then(|n| n.parent_id.clone());
            while let Some(pid) = p {
                if self.rows.iter().any(|r| tree.nodes[r.idx].id == pid) {
                    self.selected = Some(pid);
                    return;
                }
                p = index.get(pid.as_str()).and_then(|n| n.parent_id.clone());
            }
        }
        let row = old_row.unwrap_or(0).min(self.rows.len().saturating_sub(1));
        self.select_row(row);
    }

    fn refresh_rows(&mut self) {
        self.rows = match self.tree() {
            Some(t) => visible_rows(t, &self.expanded, &self.filter),
            None => Vec::new(),
        };
    }

    fn select_row(&mut self, row: usize) {
        let id = self.tree().and_then(|t| self.rows.get(row).map(|r| t.nodes[r.idx].id.clone()));
        if id != self.selected {
            self.detail_scroll = 0;
        }
        self.selected = id;
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.selected_row().unwrap_or(0) as isize;
        let max = self.rows.len() as isize - 1;
        self.select_row((cur + delta).clamp(0, max) as usize);
    }

    fn expand(&mut self) {
        let Some(r) = self.selected_row().map(|i| self.rows[i].clone()) else { return };
        if r.has_children {
            if r.expanded {
                // Already open: step into the first child.
                self.move_by(1);
            } else if let Some(id) = self.selected.clone() {
                self.expanded.insert(id);
                self.refresh_rows();
            }
        }
    }

    fn collapse(&mut self) {
        let Some(r) = self.selected_row().map(|i| self.rows[i].clone()) else { return };
        if r.has_children && r.expanded {
            if let Some(id) = self.selected.clone() {
                self.expanded.remove(&id);
                self.refresh_rows();
                // Forced open by the filter: nothing changed, fall through to parent.
                if self.selected_row().is_some_and(|i| !self.rows[i].expanded) {
                    return;
                }
            }
        }
        let parent = self.selected_node().and_then(|n| n.parent_id.clone());
        if let Some(p) = parent {
            self.selected = Some(p);
            self.detail_scroll = 0;
        }
    }

    fn set_filter(&mut self, f: String) {
        self.filter = f;
        self.refresh_rows();
        if self.selected_row().is_none() {
            self.select_row(0);
        }
    }

    fn move_timeline(&mut self, delta: isize, to: Option<bool>) {
        let n = self.timeline().map_or(0, |t| t.checkpoints.len());
        if n == 0 {
            return;
        }
        let cur = self.timeline_selected.unwrap_or(n - 1) as isize;
        let next = match to {
            Some(true) => 0,
            Some(false) => n as isize - 1,
            None => (cur + delta).clamp(0, n as isize - 1),
        } as usize;
        // Reaching the end resumes following the latest checkpoint.
        self.timeline_selected = (next != n - 1).then_some(next);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Action::Quit;
        }
        if self.filter_input {
            match key.code {
                KeyCode::Esc => {
                    self.filter_input = false;
                    self.set_filter(String::new());
                }
                KeyCode::Enter => self.filter_input = false,
                KeyCode::Backspace => {
                    let mut f = self.filter.clone();
                    f.pop();
                    self.set_filter(f);
                }
                KeyCode::Char(c) => {
                    let mut f = self.filter.clone();
                    f.push(c);
                    self.set_filter(f);
                }
                _ => {}
            }
            return Action::None;
        }
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return Action::Quit;
            }
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::BackTab => self.focus = self.focus.next().next(),
            KeyCode::Char('/') => {
                self.focus = Focus::Tree;
                self.filter_input = true;
            }
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(String::new()),
            KeyCode::Char('[') => self.nudge_explorer(-4),
            KeyCode::Char(']') => self.nudge_explorer(4),
            KeyCode::Char('n') => {
                self.notify_threshold = self.notify_threshold.next();
                self.status = Some(format!("notify threshold {}", self.notify_threshold.label()));
            }
            KeyCode::Char('o') => {
                if let Some(n) = self.selected_node() {
                    let text = n.text.clone().unwrap_or_default();
                    return Action::OpenPager { label: n.label.clone(), text };
                }
            }
            _ => match self.focus {
                Focus::Tree => self.tree_key(key.code),
                Focus::Detail => self.detail_key(key.code),
                Focus::Timeline => match key.code {
                    KeyCode::Char('j') | KeyCode::Down => self.move_timeline(1, None),
                    KeyCode::Char('k') | KeyCode::Up => self.move_timeline(-1, None),
                    KeyCode::Char('g') | KeyCode::Home => self.move_timeline(0, Some(true)),
                    KeyCode::Char('G') | KeyCode::End => self.move_timeline(0, Some(false)),
                    _ => {}
                },
            },
        }
        Action::None
    }

    fn tree_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Char('g') | KeyCode::Home => self.select_row(0),
            KeyCode::Char('G') | KeyCode::End => self.select_row(self.rows.len().saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.expand(),
            KeyCode::Char('h') | KeyCode::Left => self.collapse(),
            KeyCode::Char('J') => self.detail_key(KeyCode::Char('j')),
            KeyCode::Char('K') => self.detail_key(KeyCode::Char('k')),
            _ => {}
        }
    }

    fn detail_key(&mut self, code: KeyCode) {
        let s = &mut self.detail_scroll;
        match code {
            KeyCode::Char('j') | KeyCode::Char('J') | KeyCode::Down => *s = s.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Char('K') | KeyCode::Up => *s = s.saturating_sub(1),
            KeyCode::PageDown => *s = s.saturating_add(10),
            KeyCode::PageUp => *s = s.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => *s = 0,
            KeyCode::Char('G') | KeyCode::End => *s = u16::MAX / 2,
            _ => {}
        }
    }
}

/// Pager files older than this are swept on TUI start (left behind when the
/// overlay never ran its cleanup, e.g. Kitty was closed mid-view).
pub const PAGER_STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Script run in the overlay as `sh -c PAGER_SCRIPT sh <file>`. The pager
/// comes from `$CTXVIZ_PAGER` (passed with `launch --env`, which Kitty sets
/// verbatim), so its value is never spliced into the script text. It is
/// expanded unquoted with globbing off, so `less -R` still splits into a
/// command and its flags but no shell syntax in it is evaluated. No `exec`:
/// the shell stays to delete the file once the pager exits.
const PAGER_SCRIPT: &str = r#"set -f; ${CTXVIZ_PAGER:-less} "$1"; rm -f -- "$1""#;

/// `<state_dir>/pager`, created 0o700 (and tightened to 0o700 if it already
/// exists with looser bits). Refuses a symlink or non-directory there.
pub fn pager_dir() -> anyhow::Result<PathBuf> {
    let dir = crate::session::try_state_dir()?.join("pager");
    ensure_private_dir(&dir)?;
    Ok(dir)
}

fn ensure_private_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Create a fresh `ctxviz-<pid>-<nanos>-<n>-<label>.txt` in `dir` with
/// `create_new` (so an existing file or planted symlink is never followed or
/// reused) and mode 0o600, holding `text` with control characters replaced.
/// Sanitizing matters even though the text is only viewed in a pager: users
/// commonly set `PAGER="less -R"`, which passes raw escape sequences through
/// to the terminal.
pub fn write_pager_file(dir: &Path, label: &str, text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let safe: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(40).collect();
    let mut last_err = None;
    for _ in 0..8 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("ctxviz-{}-{nanos}-{n}-{safe}.txt", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(mut f) => {
                let res = f.write_all(crate::text::sanitize_terminal_text(text).as_bytes()).and_then(|_| f.flush());
                if let Err(e) = res {
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no unique pager file name")))
}

/// Delete regular `ctxviz-*.txt` files in `dir` last modified more than
/// `max_age` ago ([`crate::session::sweep_dir`]). Best effort; returns how many
/// were removed.
pub fn sweep_pager_dir(dir: &Path, max_age: Duration) -> usize {
    crate::session::sweep_dir(dir, max_age, |name| name.starts_with("ctxviz-") && name.ends_with(".txt"))
}

/// Sweep stale pager files on TUI start. Does not create the state dir.
pub fn sweep_stale_pager_files() {
    if let Ok(d) = crate::session::try_state_dir() {
        sweep_pager_dir(&d.join("pager"), PAGER_STALE_AFTER);
    }
}

/// `--next-to=id:N` for the overlay, with the window id validated as a
/// positive integer first. `None` when there is no window id.
pub fn next_to_arg(window_id: Option<&str>) -> anyhow::Result<Option<String>> {
    match window_id.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(w) => Ok(Some(format!("--next-to=id:{}", crate::kitty::window_id_arg(w)?))),
    }
}

/// The `kitten @` arguments for the pager overlay on `file`.
fn pager_launch_args(next_to: Option<&str>, pager: &str, file: &Path) -> Vec<String> {
    let mut args: Vec<String> =
        ["launch", "--type=overlay", "--title", "ctxviz", "--cwd=current"].map(String::from).into();
    args.extend(next_to.map(String::from));
    args.push(format!("--env=CTXVIZ_PAGER={pager}"));
    args.extend(["sh", "-c", PAGER_SCRIPT, "sh"].map(String::from));
    args.push(file.to_string_lossy().into_owned());
    args
}

/// Write `text` to a private file in `dir`, call `launch` with its path, and
/// delete the file again if the launch fails (on success the overlay's shell
/// deletes it when the pager exits; [`sweep_stale_pager_files`] catches the
/// rest). Split out from [`open_in_pager`] so tests can stub the launch.
pub fn pager_roundtrip(
    dir: &Path,
    label: &str,
    text: &str,
    launch: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    let path = write_pager_file(dir, label, text)
        .map_err(|e| anyhow::anyhow!("writing pager file in {}: {e}", dir.display()))?;
    match launch(&path) {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

/// Open `text` in `$PAGER` as a Kitty overlay over this window. Returns a
/// status message for the footer.
pub fn open_in_pager(label: &str, text: &str) -> String {
    let k = match crate::kitty::detect() {
        Ok(k) => k,
        Err(_) => return "not in kitty".into(),
    };
    let next_to = match next_to_arg(k.window_id.as_deref()) {
        Ok(n) => n,
        Err(e) => return format!("{e:#}"),
    };
    let dir = match pager_dir() {
        Ok(d) => d,
        Err(e) => return format!("pager dir: {e:#}"),
    };
    let pager = std::env::var("PAGER").ok().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "less".into());
    let res = pager_roundtrip(&dir, label, text, |file| {
        let args = pager_launch_args(next_to.as_deref(), &pager, file);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        crate::kitty::remote(&k, &refs).map(|_| ())
    });
    let label = crate::text::sanitize_single_line(label);
    match res {
        Ok(_) => format!("opened {label} in {}", crate::text::sanitize_single_line(&pager)),
        Err(e) => format!("{e:#}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::tree::Node;

    fn node(id: &str, parent: Option<&str>, cat: &str, label: &str, tokens: u64) -> Node {
        Node {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            kind: if parent.is_none() { "category" } else { "item" }.into(),
            label: label.into(),
            category_id: cat.into(),
            estimated_tokens: tokens,
            character_count: tokens * 4,
            local_estimate: None,
            text: Some(format!("text of {label}")),
        }
    }

    pub(crate) fn sample_tree() -> Tree {
        Tree {
            nodes: vec![
                node("rules", None, "rules", "Rules", 50),
                node("r1", Some("rules"), "rules", "small rule", 10),
                node("r2", Some("rules"), "rules", "big rule", 40),
                node("conv", None, "conversation", "Conversation", 100),
                node("t1", Some("conv"), "conversation", "Turn 1", 70),
                node("t1a", Some("t1"), "conversation", "user message", 20),
                node("t1b", Some("t1"), "conversation", "assistant reply", 50),
                node("t2", Some("conv"), "conversation", "Turn 2", 30),
            ],
            ..Tree::default()
        }
    }

    pub(crate) fn snapshot(tree: Tree) -> Snapshot {
        Snapshot {
            store: PathBuf::from("/tmp/store.db"),
            agent_id: Some("a".into()),
            name: Some("Test chat".into()),
            root_blob_id: Some("r".into()),
            used: 150,
            max: 200,
            percent: Some(75.0),
            breakdown: vec![],
            message_ids: vec![],
            created_at: None,
            tree,
            timeline: None,
            meta: Default::default(),
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn labels(app: &App) -> Vec<String> {
        let t = app.tree().unwrap();
        app.rows().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), t.nodes[r.idx].label)).collect()
    }

    #[test]
    fn flatten_orders_and_respects_expansion() {
        let t = sample_tree();
        let rows = visible_rows(&t, &HashSet::new(), "");
        // Top level by size.
        assert_eq!(rows.iter().map(|r| t.nodes[r.idx].id.as_str()).collect::<Vec<_>>(), ["conv", "rules"]);
        assert!(rows.iter().all(|r| r.has_children && !r.expanded));
        let exp: HashSet<String> = ["conv", "rules", "t1"].iter().map(|s| s.to_string()).collect();
        let rows = visible_rows(&t, &exp, "");
        let ids: Vec<_> = rows.iter().map(|r| t.nodes[r.idx].id.as_str()).collect();
        // Conversation chronological, rules by size.
        assert_eq!(ids, ["conv", "t1", "t1a", "t1b", "t2", "rules", "r2", "r1"]);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn filter_keeps_ancestors() {
        let t = sample_tree();
        let rows = visible_rows(&t, &HashSet::new(), "REPLY");
        let ids: Vec<_> = rows.iter().map(|r| t.nodes[r.idx].id.as_str()).collect();
        assert_eq!(ids, ["conv", "t1", "t1b"]);
        assert!(rows[0].expanded && rows[1].expanded);
    }

    #[test]
    fn keys_expand_collapse_and_move() {
        let mut app = App::new();
        app.set_snapshot(snapshot(sample_tree()));
        // Top level starts expanded, first row selected.
        assert_eq!(app.selected.as_deref(), Some("conv"));
        assert_eq!(labels(&app).len(), 6);
        app.on_key(key('j'));
        assert_eq!(app.selected.as_deref(), Some("t1"));
        app.on_key(key('l'));
        assert!(app.expanded.contains("t1"));
        app.on_key(key('l')); // already open: step into first child
        assert_eq!(app.selected.as_deref(), Some("t1a"));
        app.on_key(key('h')); // leaf: jump to parent
        assert_eq!(app.selected.as_deref(), Some("t1"));
        app.on_key(key('h')); // collapse
        assert!(!app.expanded.contains("t1"));
        app.on_key(key('h')); // collapsed: jump to parent
        assert_eq!(app.selected.as_deref(), Some("conv"));
        app.on_key(key('G'));
        assert_eq!(app.selected.as_deref(), Some("r1"));
        app.on_key(key('g'));
        assert_eq!(app.selected.as_deref(), Some("conv"));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Timeline);
        assert_eq!(app.on_key(key('q')), Action::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn filter_input_mode() {
        let mut app = App::new();
        app.set_snapshot(snapshot(sample_tree()));
        app.on_key(key('/'));
        for c in "big".chars() {
            app.on_key(key(c));
        }
        assert_eq!(app.filter, "big");
        assert_eq!(labels(&app), ["Rules", "  big rule"]);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.filter.is_empty() && !app.filter_input);
        assert_eq!(labels(&app).len(), 6);
    }

    #[test]
    fn selection_survives_rebuild() {
        let mut app = App::new();
        app.set_snapshot(snapshot(sample_tree()));
        app.on_key(key('j'));
        app.on_key(key('j'));
        assert_eq!(app.selected.as_deref(), Some("t2"));
        // New turn prepended in display order (bigger node lands above): same id kept.
        let mut t = sample_tree();
        t.nodes.insert(3, node("t0", Some("conv"), "conversation", "Turn 0", 5));
        app.set_snapshot(snapshot(t));
        assert_eq!(app.selected.as_deref(), Some("t2"));
        // t2 (row 3 after t0 landed) vanishes: the row now at index 3 is selected.
        let mut t = sample_tree();
        t.nodes.retain(|n| n.id != "t2");
        app.set_snapshot(snapshot(t));
        assert_eq!(app.selected.as_deref(), Some("r2"));
        // Expansion state is kept across rebuilds.
        assert!(app.expanded.contains("conv"));
    }

    #[test]
    fn hidden_selection_moves_to_visible_ancestor() {
        let mut app = App::new();
        app.set_snapshot(snapshot(sample_tree()));
        app.selected = Some("t1b".into()); // hidden: t1 collapsed
        app.set_snapshot(snapshot(sample_tree()));
        assert_eq!(app.selected.as_deref(), Some("t1"));
    }

    #[test]
    fn brackets_nudge_explorer_width() {
        let mut app = App::new();
        app.body_width.set(100);
        app.on_key(key(']'));
        assert_eq!(app.explorer_cols, Some(40));
        for _ in 0..20 {
            app.on_key(key('['));
        }
        assert_eq!(app.explorer_cols, Some(24));
        for _ in 0..30 {
            app.on_key(key(']'));
        }
        assert_eq!(app.explorer_cols, Some(80));
        // Works from any focus, but not while typing a filter.
        app.focus = Focus::Detail;
        app.on_key(key('['));
        assert_eq!(app.explorer_cols, Some(76));
        app.on_key(key('/'));
        app.on_key(key('['));
        assert_eq!(app.explorer_cols, Some(76));
        assert_eq!(app.filter, "[");
    }

    #[test]
    fn brackets_start_from_split_percent() {
        let mut app = App::new();
        app.explorer_pct = Some(50);
        app.body_width.set(120);
        app.on_key(key('['));
        assert_eq!((app.explorer_cols, app.explorer_pct), (Some(56), None));
    }

    #[test]
    fn brackets_save_clamped_width() {
        let dir = crate::test_support::TempDir::new("tui-save");
        let path = dir.join("state/tui.json");
        let mut app = App::new();
        app.state_path = Some(path.clone());
        app.body_width.set(100);
        for _ in 0..30 {
            app.on_key(key(']'));
        }
        let st = crate::session::TuiState::read_from(&path).expect("tui.json written");
        assert_eq!(st.explorer_cols, Some(80));
        app.on_key(key('['));
        assert_eq!(crate::session::TuiState::read_from(&path).unwrap().explorer_cols, Some(76));
    }

    /// A path that does not exist yet (the pager code creates it) inside a
    /// fresh temp dir unique to this test, removed with the guard.
    fn temp_dir(tag: &str) -> (crate::test_support::TempDir, PathBuf) {
        let guard = crate::test_support::TempDir::new(tag);
        let root = guard.join("root");
        (guard, root)
    }

    #[test]
    fn pager_file_is_private_sanitized_and_removed_on_launch_failure() {
        use std::os::unix::fs::PermissionsExt;
        let (_guard, root) = temp_dir("pager");
        let dir = root.join("state/pager");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let mut seen = None;
        let err = pager_roundtrip(&dir, "tool/result 1", "ok\x1b]52;c;QUJD\x07\nline\u{9b}2", |p| {
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            seen = Some((p.to_path_buf(), mode, std::fs::read_to_string(p).unwrap()));
            anyhow::bail!("stub launch failed")
        })
        .unwrap_err();
        assert_eq!(format!("{err}"), "stub launch failed");
        let (path, mode, content) = seen.unwrap();
        assert_eq!(mode, 0o600);
        assert!(path.starts_with(&dir));
        assert!(path.file_name().unwrap().to_string_lossy().ends_with("-tool_result_1.txt"));
        assert_eq!(content, "ok\u{FFFD}]52;c;QUJD\u{FFFD}\nline\u{FFFD}2");
        assert!(crate::text::is_clean(&content));
        assert!(!path.exists(), "pager file must be removed when the launch fails");
    }

    #[test]
    fn pager_files_get_unique_names_and_never_follow_symlinks() {
        let (_guard, root) = temp_dir("pager-uniq");
        ensure_private_dir(&root).unwrap();
        let a = write_pager_file(&root, "x", "a").unwrap();
        let b = write_pager_file(&root, "x", "b").unwrap();
        assert_ne!(a, b);
        // A planted symlink at a would-be path is not written through.
        let victim = root.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::fs::remove_file(&a).unwrap();
        std::os::unix::fs::symlink(&victim, &a).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).create_new(true).open(&a).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn pager_dir_refuses_symlink() {
        let (_guard, root) = temp_dir("pager-link");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("pager")).unwrap();
        assert!(ensure_private_dir(&root.join("pager")).is_err());
    }

    #[test]
    fn pager_script_runs_pager_then_deletes_file_without_injection() {
        let (_guard, root) = temp_dir("pager-sh");
        ensure_private_dir(&root).unwrap();
        let marker = root.join("injected");
        let file = write_pager_file(&root, "x", "hello\n").unwrap();
        let out = std::process::Command::new("sh")
            .args(["-c", PAGER_SCRIPT, "sh"])
            .arg(&file)
            .env("CTXVIZ_PAGER", "cat -u")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n");
        assert!(!file.exists(), "overlay script must delete the file after the pager exits");
        // Shell syntax in $PAGER is not evaluated.
        let file = write_pager_file(&root, "x", "hi").unwrap();
        let evil = format!("cat; touch {} $(touch {})", marker.display(), marker.display());
        let _ = std::process::Command::new("sh")
            .args(["-c", PAGER_SCRIPT, "sh"])
            .arg(&file)
            .env("CTXVIZ_PAGER", evil)
            .output()
            .unwrap();
        assert!(!marker.exists(), "PAGER contents were executed as shell code");
        assert!(!file.exists());
    }

    #[test]
    fn pager_launch_args_keep_pager_out_of_the_script() {
        let args = pager_launch_args(Some("--next-to=id:7"), "less -R; rm -rf ~", Path::new("/s/pager/f.txt"));
        let sh = args.iter().position(|a| a == "sh").unwrap();
        assert_eq!(&args[sh..], ["sh", "-c", PAGER_SCRIPT, "sh", "/s/pager/f.txt"]);
        assert!(args.contains(&"--env=CTXVIZ_PAGER=less -R; rm -rf ~".to_string()));
        assert!(args.contains(&"--next-to=id:7".to_string()));
    }

    #[test]
    fn sweep_removes_only_stale_pager_files() {
        let (_guard, root) = temp_dir("pager-sweep");
        ensure_private_dir(&root).unwrap();
        let old = write_pager_file(&root, "old", "o").unwrap();
        let fresh = write_pager_file(&root, "new", "n").unwrap();
        let other = root.join("keep.txt");
        std::fs::write(&other, "k").unwrap();
        let past = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        for p in [&old, &other] {
            std::fs::File::options().write(true).open(p).unwrap().set_modified(past).unwrap();
        }
        assert_eq!(sweep_pager_dir(&root, PAGER_STALE_AFTER), 1);
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(other.exists());
        assert_eq!(sweep_pager_dir(&root.join("missing"), PAGER_STALE_AFTER), 0);
    }

    #[test]
    fn next_to_window_id_must_be_a_positive_integer() {
        assert_eq!(next_to_arg(Some("42")).unwrap().as_deref(), Some("--next-to=id:42"));
        assert_eq!(next_to_arg(None).unwrap(), None);
        assert_eq!(next_to_arg(Some("")).unwrap(), None);
        for bad in ["-1", "0", "abc", "3 or title:x", "id:3", "1.0", "+2"] {
            assert!(next_to_arg(Some(bad)).is_err(), "{bad:?}");
        }
    }
}
