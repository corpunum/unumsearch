// SPDX-License-Identifier: Apache-2.0
//! Regression test for issue #6: while a rebuild is running, the unit's
//! pending changes must stay visible to queries. A rebuild used to clear the
//! dirty flag and the pending overlay when it *started* and install the new
//! shards only when it *finished*, so a query landing in between was served
//! from the old index with no overlay and still claimed `fresh:true`. Slow
//! shard fsyncs (`F_FULLFSYNC` on macOS CI) made that window wide enough to
//! hit in the watcher differential test (seed 1024301).
//!
//! The rebuild is stretched deterministically with a test hook, so this
//! fails on every platform without the fix.

use std::sync::Arc;
use std::time::Duration;
use unumsearch::engine::{Change, Engine, SearchOpts};
use unumsearch::Config;

fn setup() -> (tempfile::TempDir, std::path::PathBuf, Arc<Engine>) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/b/old.txt"), "old_token\n").unwrap();
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.path().join("index").to_string_lossy().into_owned()),
        threads: 2,
        rescan_secs: 3600,
        watch: false,
        ..Config::default()
    };
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    e.index_all(false);
    (t, root, e)
}

fn q(e: &Engine, root: &std::path::Path, pat: &str) -> (Vec<String>, bool) {
    let r = e
        .search(&SearchOpts {
            pattern: pat.into(),
            root: root.to_path_buf(),
            files_only: true,
            ..Default::default()
        })
        .unwrap();
    (r.files, r.fresh)
}

/// Run a stalled rebuild of `unit` in the background; returns its handle
/// once the build has started (its listing is taken right away).
fn stalled_build(e: &Arc<Engine>, unit: &str, ms: u64) -> std::thread::JoinHandle<bool> {
    e.set_build_stall_ms(ms);
    let (e2, u2) = (e.clone(), unit.to_string());
    let h = std::thread::spawn(move || e2.build_unit(&u2, false).is_some());
    // Let the build take its listing and enter the stall.
    std::thread::sleep(Duration::from_millis(ms / 4));
    h
}

#[test]
fn pending_file_stays_visible_while_a_rebuild_runs() {
    let (_t, root, e) = setup();
    let unit = root.to_string_lossy().into_owned();
    assert!(q(&e, &root, "new_token").1);

    std::fs::write(root.join("a/b/new4.txt"), "new_token\n").unwrap();
    e.mark_dirty(&unit, Change::File("a/b/new4.txt".into()));
    // Before the rebuild: found through the pending overlay, exact, fresh.
    let (files, fresh) = q(&e, &root, "new_token");
    assert_eq!(files.len(), 1);
    assert!(fresh);

    let h = stalled_build(&e, &unit, 600);
    // During the rebuild: a fresh answer must be exact.
    for _ in 0..5 {
        let (files, fresh) = q(&e, &root, "new_token");
        assert!(
            !fresh || files.len() == 1,
            "fresh answer during a rebuild lost a pending file: {files:?}"
        );
        assert_eq!(files.len(), 1, "pending file not searched during rebuild");
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(h.join().unwrap());
    e.set_build_stall_ms(0);
    let (files, fresh) = q(&e, &root, "new_token");
    assert_eq!(files.len(), 1);
    assert!(fresh);
    assert_eq!(e.status()["units_dirty"], 0);
}

#[test]
fn unknown_change_is_not_fresh_while_a_rebuild_runs() {
    let (_t, root, e) = setup();
    let unit = root.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("c")).unwrap();
    std::fs::write(root.join("c/x.txt"), "dir_token\n").unwrap();
    e.mark_dirty(&unit, Change::Unknown);
    assert!(!q(&e, &root, "dir_token").1);

    let h = stalled_build(&e, &unit, 600);
    for _ in 0..5 {
        let (files, fresh) = q(&e, &root, "dir_token");
        assert!(
            !fresh || files.len() == 1,
            "an unrepresentable change was reported fresh during the rebuild that resolves it"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(h.join().unwrap());
    e.set_build_stall_ms(0);
    let (files, fresh) = q(&e, &root, "dir_token");
    assert_eq!(files.len(), 1);
    assert!(fresh);
}

#[test]
fn changes_arriving_during_a_rebuild_survive_it() {
    let (_t, root, e) = setup();
    let unit = root.to_string_lossy().into_owned();
    std::fs::write(root.join("a/b/first.txt"), "first_token\n").unwrap();
    e.mark_dirty(&unit, Change::File("a/b/first.txt".into()));

    let h = stalled_build(&e, &unit, 600);
    // Written after the build's listing: the build cannot index it.
    std::fs::write(root.join("a/b/late.txt"), "late_token\n").unwrap();
    e.mark_dirty(&unit, Change::File("a/b/late.txt".into()));
    std::fs::write(root.join("a/late_dir.txt"), "late_unknown\n").unwrap();
    assert!(h.join().unwrap());
    e.set_build_stall_ms(0);

    // The early change is indexed; the late one is still pending (dirty,
    // searched directly) and therefore still exact.
    let (files, fresh) = q(&e, &root, "first_token");
    assert_eq!(files.len(), 1);
    assert!(fresh);
    let (files, fresh) = q(&e, &root, "late_token");
    assert_eq!(files.len(), 1, "a change made during the rebuild was lost");
    assert!(fresh);
    assert_eq!(e.status()["units_dirty"], 1);

    // An unknown change during a rebuild keeps the unit not fresh after it.
    let h = stalled_build(&e, &unit, 300);
    e.mark_dirty(&unit, Change::Unknown);
    assert!(h.join().unwrap());
    e.set_build_stall_ms(0);
    assert!(!q(&e, &root, "late_unknown").1);
    assert!(e.build_unit(&unit, false).is_some());
    let (files, fresh) = q(&e, &root, "late_unknown");
    assert_eq!(files.len(), 1);
    assert!(fresh);
    assert_eq!(e.status()["units_dirty"], 0);
}
