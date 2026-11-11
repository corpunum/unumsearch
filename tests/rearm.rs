// SPDX-License-Identifier: Apache-2.0
//! Issue #7: notify 8's Windows backend can drop a watch silently (buffer
//! overflow), so on Windows each rescan drops and re-adds every unit's
//! watches before listing it again. This test forces that path on every
//! platform (its own process: the hook is global) and checks the re-added
//! watches still deliver changes, with the unit fresh afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
use unumsearch::Config;

fn find(e: &Engine, root: &std::path::Path, pat: &str) -> (usize, bool) {
    let r = e
        .search(&SearchOpts {
            pattern: pat.into(),
            root: root.to_path_buf(),
            files_only: true,
            ..Default::default()
        })
        .unwrap();
    (r.files.len(), r.fresh)
}

fn wait_for(what: &str, limit: Duration, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < limit, "{what} (after {:?})", t0.elapsed());
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn rescan_rearms_watches_and_they_still_deliver() {
    unumsearch::watch::FORCE_REARM.store(true, Ordering::Relaxed);
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    std::fs::write(root.join("src/deep/a.txt"), "first_marker\n").unwrap();
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.path().join("index").to_string_lossy().into_owned()),
        threads: 2,
        debounce_ms: 50,
        // The shortest interval the writer loop allows.
        rescan_secs: 10,
        ..Config::default()
    };
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));

    wait_for("initial index never fresh", Duration::from_secs(20), || {
        find(&e, &root, "first_marker") == (1, true)
    });
    wait_for(
        "no rescan re-armed the watches",
        Duration::from_secs(30),
        || unumsearch::watch::REARMS.load(Ordering::Relaxed) > 0,
    );
    // Fresh again once the re-listing re-added the watches.
    wait_for(
        "unit not fresh after re-arm",
        Duration::from_secs(10),
        || find(&e, &root, "first_marker") == (1, true),
    );
    // The next rescan is ~10 s away: a change seen within a few seconds came
    // through the re-added watch, not through a listing.
    let rearms = unumsearch::watch::REARMS.load(Ordering::Relaxed);
    std::fs::write(root.join("src/deep/b.txt"), "second_marker\n").unwrap();
    wait_for(
        "change after re-arm not seen",
        Duration::from_secs(5),
        || find(&e, &root, "second_marker") == (1, true),
    );
    assert_eq!(
        unumsearch::watch::REARMS.load(Ordering::Relaxed),
        rearms,
        "a rescan ran meanwhile; the test proves nothing"
    );
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
