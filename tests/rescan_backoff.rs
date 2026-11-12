// SPDX-License-Identifier: Apache-2.0
//! The periodic rescan backs off for a watched unit that stays clean: it is
//! re-listed every 2nd, then every 4th rescan (`rescan_backoff_max`), not at
//! every one. A rescan that finds a change the watcher missed (here a file
//! created in a directory that holds no indexed file, so has no watch) puts
//! every unit back to every rescan. Where watches are re-armed at each
//! rescan (Windows, forced here) nothing is skipped. Its own process: the
//! hooks are global.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
use unumsearch::watch::{FORCE_REARM, REARMS, RESCANS, RESCAN_MS, RESCAN_SKIPS};
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
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn rescans() -> u64 {
    RESCANS.load(Ordering::Relaxed)
}

fn skips() -> u64 {
    RESCAN_SKIPS.load(Ordering::Relaxed)
}

fn wait_rescans(n: u64) {
    let target = rescans() + n;
    wait_for("rescans stopped", Duration::from_secs(30), || {
        rescans() >= target
    });
}

#[test]
fn clean_watched_unit_is_listed_less_often_until_a_miss() {
    RESCAN_MS.store(150, Ordering::Relaxed);
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // No indexed file inside: the directory gets no watch.
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("src/a.txt"), "first_marker\n").unwrap();
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.path().join("index").to_string_lossy().into_owned()),
        threads: 2,
        debounce_ms: 50,
        rescan_secs: 3600,
        rescan_backoff_max: 4,
        ..Config::default()
    };
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));

    wait_for("initial index never fresh", Duration::from_secs(20), || {
        find(&e, &root, "first_marker") == (1, true)
    });

    // Steady state: listed at rescans 1, 3, 7, 11, ... (not at 2, 4-6, 8-10).
    let (r0, s0) = (rescans(), skips());
    wait_rescans(12);
    let (ran, skipped) = (rescans() - r0, skips() - s0);
    assert!(
        skipped * 2 >= ran,
        "a clean watched unit was listed at {} of {ran} rescans",
        ran - skipped
    );
    // A change the watcher sees is not delayed by the back-off.
    std::fs::write(root.join("src/b.txt"), "watched_marker\n").unwrap();
    wait_for("watched change not seen", Duration::from_secs(5), || {
        find(&e, &root, "watched_marker") == (1, true)
    });
    // ... and rebuilt (until then it is searched directly): that rebuild's
    // listing must not be the one that finds the next file.
    let unit = root.to_string_lossy().into_owned();
    wait_for("watched change not rebuilt", Duration::from_secs(5), || {
        e.rescan_clean(&unit).is_some()
    });

    // A change no watch sees is found by a listing within the back-off.
    let r1 = rescans();
    std::fs::write(root.join("docs/hole.txt"), "hole_marker\n").unwrap();
    wait_for(
        "missed change never listed",
        Duration::from_secs(20),
        || find(&e, &root, "hole_marker").0 == 1,
    );
    assert!(
        rescans() - r1 <= 5,
        "found only after {} rescans",
        rescans() - r1
    );
    // That rescan found what the watcher missed: back to listing at every
    // rescan, then every 2nd. (Still backed off: 3 skips in a row.)
    let s1 = skips();
    wait_rescans(3);
    assert!(
        skips() - s1 <= 1,
        "a miss did not reset the back-off ({} skips in 3 rescans)",
        skips() - s1
    );

    // Watches re-armed at every rescan (Windows): every unit is listed.
    FORCE_REARM.store(true, Ordering::Relaxed);
    wait_rescans(2);
    let (s2, rearms) = (skips(), REARMS.load(Ordering::Relaxed));
    wait_rescans(6);
    assert_eq!(skips(), s2, "a rescan that re-arms watches skipped a unit");
    assert!(REARMS.load(Ordering::Relaxed) > rearms);
    wait_for("not fresh after re-arms", Duration::from_secs(10), || {
        find(&e, &root, "hole_marker") == (1, true)
    });

    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
