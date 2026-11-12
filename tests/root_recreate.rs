// SPDX-License-Identifier: Apache-2.0
//! A unit's root directory that is removed and created again (`rm -rf repo &&
//! git clone ... repo`) must be watched again, not only listed again: after
//! it is clean, a file created in it is found through its watch. Rescans are
//! fast here because a deleted root may only be rediscovered by one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
use unumsearch::watch::RESCAN_MS;
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

#[test]
fn unit_root_removed_and_created_again_is_watched_again() {
    RESCAN_MS.store(300, Ordering::Relaxed);
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("a.txt"), "first_marker\n").unwrap();
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.path().join("index").to_string_lossy().into_owned()),
        threads: 2,
        debounce_ms: 50,
        rescan_secs: 3600,
        ..Config::default()
    };
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    let unit = root.to_string_lossy().into_owned();
    wait_for("initial index never fresh", Duration::from_secs(20), || {
        find(&e, &root, "first_marker") == (1, true)
    });
    wait_for("initial index never clean", Duration::from_secs(20), || {
        e.rescan_clean(&unit).is_some()
    });
    std::fs::remove_dir_all(&root).unwrap();
    wait_for("removal not seen", Duration::from_secs(10), || {
        find(&e, &root, "first_marker").0 == 0
    });
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("b.txt"), "second_marker\n").unwrap();
    wait_for(
        "re-created root not indexed",
        Duration::from_secs(15),
        || find(&e, &root, "second_marker") == (1, true),
    );
    wait_for(
        "re-created unit never clean",
        Duration::from_secs(20),
        || e.rescan_clean(&unit).is_some(),
    );
    std::fs::write(root.join("c.txt"), "third_marker\n").unwrap();
    wait_for(
        "file in the re-created root",
        Duration::from_secs(5),
        || find(&e, &root, "third_marker") == (1, true),
    );
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
