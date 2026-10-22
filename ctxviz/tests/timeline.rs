//! Timeline scan and cache tests. Every check runs on the synthetic store
//! from `tests/common`, with golden checkpoint values asserted; with
//! `CTXVIZ_REAL_FIXTURE=1` the same checks also run on the private real-store
//! fixture (`tests/fixtures/store.db`, made by `scripts/make-fixture.sh`) and
//! fail if it is missing. Caches go to a per-test temp dir.

mod common;

use common::{synthetic, TempDir};
use ctxviz::model::breakdown;
use ctxviz::model::timeline::{Timeline, CACHE_VERSION};
use ctxviz::store::Store;
use std::collections::HashSet;
use std::path::PathBuf;

/// `cache.json` in a fresh temp dir (removed with the guard).
fn temp_cache(name: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(&format!("timeline-{name}"));
    let path = dir.join("cache.json");
    (dir, path)
}

fn truncated(full: &Timeline, mid: i64) -> Timeline {
    let mut t = full.clone();
    t.checkpoints.retain(|c| c.rowid <= mid);
    t.last_rowid = mid;
    t
}

fn synthetic_store(name: &str) -> (common::SyntheticStore, Store) {
    let fx = common::SyntheticStore::new(name);
    let store = Store::open(&fx.path).unwrap();
    (fx, store)
}

fn real_store() -> Option<Store> {
    Some(Store::open(common::real_fixture("store.db")?).unwrap())
}

// --------------------------------------------------------------------------
// Checks shared by the synthetic and real stores

fn check_incremental(store: &Store) {
    let full = Timeline::scan(store).unwrap();
    assert!(full.checkpoints.len() >= 2, "store should have several checkpoints");
    assert_eq!(full.version, CACHE_VERSION);
    assert_eq!(full.last_rowid, store.max_rowid().unwrap());

    // Resume at every checkpoint boundary and just before it.
    for c in &full.checkpoints {
        for mid in [c.rowid, c.rowid - 1] {
            let mut partial = truncated(&full, mid);
            let added = partial.extend(store).unwrap();
            assert_eq!(partial, full, "resume from rowid {mid}");
            assert_eq!(added, full.checkpoints.iter().filter(|c| c.rowid > mid).count());
        }
    }

    // Same through the cache file.
    let (_dir, path) = temp_cache("incremental");
    let mid = full.checkpoints[full.checkpoints.len() / 2].rowid;
    truncated(&full, mid).save(&path).unwrap();
    assert_eq!(Timeline::load_or_scan_at(store, &path).unwrap(), full);
    assert_eq!(Timeline::load(&path).unwrap(), full, "cache rewritten after extend");
    // Warm load is a no-op.
    assert_eq!(Timeline::load_or_scan_at(store, &path).unwrap(), full);
}

fn check_twins(store: &Store) {
    let t = Timeline::scan(store).unwrap();
    let mut keys = HashSet::new();
    for c in &t.checkpoints {
        assert!(keys.insert(c.key()), "duplicate twin key {:?} at rowid {}", c.key(), c.rowid);
    }
    let roots = store
        .scan_rows(0)
        .unwrap()
        .into_iter()
        .filter(|r| ctxviz::schema::is_root(&r.data))
        .filter(|r| breakdown::token_details_only(&r.data).unwrap().is_some_and(|d| d.used_tokens > 0))
        .count();
    assert!(roots > t.checkpoints.len(), "expected twin roots: {roots} roots, {} checkpoints", t.checkpoints.len());
}

fn check_order_and_views(store: &Store) {
    let t = Timeline::scan(store).unwrap();
    for w in t.checkpoints.windows(2) {
        assert!(w[0].rowid < w[1].rowid, "rowids strictly increasing");
        assert!(w[0].message_count <= w[1].message_count, "message_count non-decreasing");
    }
    assert_eq!(t.deltas().len(), t.checkpoints.len() - 1);
    assert_eq!(t.per_turn().len(), t.checkpoints.len() - 1);
    assert_eq!(t.last_delta(), t.deltas().last().copied());
    let recent = t.recent_used(3);
    assert_eq!(recent.len(), 3.min(t.checkpoints.len()));
    assert_eq!(recent.last().copied(), t.checkpoints.last().map(|c| c.used_tokens));
}

fn check_latest_root(store: &Store) {
    let t = Timeline::scan(store).unwrap();
    let id = store.latest_root_id().unwrap().unwrap();
    let info = breakdown::decode_root(&store.blob(&id).unwrap().unwrap()).unwrap();
    let td = info.details.expect("latest root has token details");
    let key = (info.message_ids.len(), td.used_tokens);
    assert!(t.checkpoints.iter().any(|c| c.root_id == id || c.key() == key), "latest root {id} (or its twin) missing");
    assert_eq!(t.checkpoints.last().unwrap().key(), key, "latest root is the last checkpoint");
    assert_eq!(t.store_created_at, info.created_at);
}

fn check_stale_cache(store: &Store) {
    let full = Timeline::scan(store).unwrap();
    let (_dir, path) = temp_cache("stale");
    let check = |bad: Timeline, why: &str| {
        bad.save(&path).unwrap();
        assert_eq!(Timeline::load_or_scan_at(store, &path).unwrap(), full, "{why}");
        assert_eq!(Timeline::load(&path).unwrap(), full, "{why}: cache rewritten");
    };

    // Wrong createdAt over a bogus history that claims to be current.
    let mut stale = full.clone();
    stale.store_created_at = Some(1);
    stale.checkpoints.truncate(1);
    stale.checkpoints[0].used_tokens += 7;
    check(stale, "wrong store_created_at");

    let mut moved = full.clone();
    moved.store_path = Some("/elsewhere/store.db".into());
    moved.checkpoints.clear();
    check(moved, "wrong store_path");

    let mut old = full.clone();
    old.version = 0;
    old.checkpoints.clear();
    check(old, "old version");

    // Claims to be current but the latest root is missing.
    let mut lost = full.clone();
    lost.checkpoints.pop();
    check(lost, "latest root missing");

    let mut ahead = full.clone();
    ahead.last_rowid += 1000;
    ahead.checkpoints.clear();
    check(ahead, "last_rowid beyond store");

    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(Timeline::load_or_scan_at(store, &path).unwrap(), full, "garbage cache");
}

// --------------------------------------------------------------------------
// Synthetic store (always runs)

#[test]
fn synthetic_checkpoints_match_golden() {
    let (fx, store) = synthetic_store("timeline-golden");
    let t = Timeline::scan(&store).unwrap();
    // Two roots without usage are skipped; each twin pair is one point.
    assert_eq!(t.checkpoints.len(), synthetic::CHECKPOINTS.len(), "timeline point count");
    let keys: Vec<(usize, u64)> = t.checkpoints.iter().map(|c| c.key()).collect();
    assert_eq!(keys, synthetic::CHECKPOINTS);
    // The lower-rowid twin is kept.
    let roots: Vec<String> = t.checkpoints.iter().map(|c| c.root_id.clone()).collect();
    let want: Vec<String> = (1..=3).map(|cp| common::hex(&synthetic::root_id(cp, 0))).collect();
    assert_eq!(roots, want);
    assert!(t.checkpoints.iter().all(|c| c.max_tokens == synthetic::MAX_TOKENS && !c.compaction));
    assert_eq!(t.deltas(), [600, 500]);
    assert_eq!(t.last_rowid, synthetic::ROWS);
    assert_eq!(t.store_created_at, Some(synthetic::CREATED_AT));
    assert_eq!(t.store_path.as_deref(), Some(fx.path.to_str().unwrap()));
    // Last checkpoint carries the full breakdown in server order.
    let last: Vec<(String, u64)> = synthetic::BREAKDOWN.iter().map(|(id, _, n)| (id.to_string(), *n)).collect();
    assert_eq!(t.checkpoints.last().unwrap().categories, last);
    assert_eq!(t.checkpoints[0].category("conversation"), 1500 - 1320);
    // Every structural root is seen, with and without usage.
    let rows = store.scan_rows(0).unwrap();
    assert_eq!(rows.iter().filter(|r| ctxviz::schema::is_root(&r.data)).count(), synthetic::ROOTS);
}

#[test]
fn synthetic_cache_written_on_first_load() {
    let (_fx, store) = synthetic_store("timeline-cache");
    let (_dir, path) = temp_cache("first-load");
    let t = Timeline::load_or_scan_at(&store, &path).unwrap();
    assert_eq!(t.checkpoints.len(), synthetic::CHECKPOINTS.len());
    assert_eq!(Timeline::load(&path).unwrap(), t, "cache written on first load");
}

#[test]
fn incremental_equals_full_scan() {
    let (_fx, store) = synthetic_store("timeline-incremental");
    check_incremental(&store);
}

#[test]
fn twins_are_deduped() {
    let (_fx, store) = synthetic_store("timeline-twins");
    check_twins(&store);
}

#[test]
fn monotone_order_and_views() {
    let (_fx, store) = synthetic_store("timeline-order");
    check_order_and_views(&store);
}

#[test]
fn latest_root_present() {
    let (_fx, store) = synthetic_store("timeline-latest");
    check_latest_root(&store);
}

#[test]
fn stale_or_corrupt_cache_rescans() {
    let (_fx, store) = synthetic_store("timeline-stale");
    check_stale_cache(&store);
}

// --------------------------------------------------------------------------
// Real store (CTXVIZ_REAL_FIXTURE=1)

#[test]
fn real_store_timeline_checks() {
    let Some(store) = real_store() else { return };
    check_incremental(&store);
    check_twins(&store);
    check_order_and_views(&store);
    check_latest_root(&store);
    check_stale_cache(&store);
}
