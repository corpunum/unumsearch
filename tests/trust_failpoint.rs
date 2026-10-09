// SPDX-License-Identifier: Apache-2.0
//! A failed rebuild must not lose pending changes or report fresh. Own test
//! binary: the failpoint is process-global.

use std::sync::atomic::Ordering;
use unumsearch::engine::{Change, Engine, SearchOpts, FAIL_SHARD_WRITES};
use unumsearch::Config;

#[test]
fn failed_rebuild_keeps_old_index_pending_changes_and_is_not_fresh() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn old_token() {}\n").unwrap();
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.path().join("index").to_string_lossy().into_owned()),
        threads: 2,
        rescan_secs: 3600,
        watch: false,
        ..Config::default()
    };
    let e = Engine::open(cfg, true).unwrap();
    e.index_all(false);
    let unit = root.to_string_lossy().into_owned();
    let q = |p: &str| {
        e.search(&SearchOpts {
            pattern: p.into(),
            root: root.clone(),
            files_only: true,
            ..Default::default()
        })
        .unwrap()
    };
    assert!(q("old_token").fresh);

    // A change arrives, then the disk starts failing.
    std::fs::write(root.join("src/a.rs"), "fn changed_token() {}\n").unwrap();
    std::fs::write(root.join("src/new.rs"), "fn added_token() {}\n").unwrap();
    e.mark_dirty(&unit, Change::File("src/a.rs".into()));
    e.mark_dirty(&unit, Change::File("src/new.rs".into()));
    FAIL_SHARD_WRITES.store(true, Ordering::SeqCst);
    assert!(e.build_unit(&unit, false).is_none());
    let r = q("changed_token");
    // The pending changes are still searched directly...
    assert_eq!(r.files.len(), 1);
    assert_eq!(q("added_token").files.len(), 1);
    // ...but the answer is no longer claimed fresh, and the old index is kept.
    assert!(!r.fresh, "a failed rebuild must not report fresh");
    assert!(std::fs::read_dir(t.path().join("index"))
        .unwrap()
        .flatten()
        .any(|e| e.path().extension().is_some_and(|x| x == "shard")));
    // Repeated failures keep it not-fresh and don't lose anything.
    assert!(e.build_unit(&unit, true).is_none());
    assert!(!q("changed_token").fresh);
    assert_eq!(q("added_token").files.len(), 1);
    let st = e.status();
    assert_eq!(st["units_dirty"], 1);

    // The disk recovers: the next build succeeds and the unit is fresh again.
    FAIL_SHARD_WRITES.store(false, Ordering::SeqCst);
    assert!(e.build_unit(&unit, false).is_some());
    let r = q("added_token");
    assert_eq!(r.files.len(), 1);
    assert!(r.fresh);
    assert_eq!(q("old_token").files.len(), 0);
}
