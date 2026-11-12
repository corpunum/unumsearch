// SPDX-License-Identifier: Apache-2.0
//! A configured root that is removed must stop answering `fresh: true`:
//! nothing watches its parent, so a later re-creation raises no event.
//! Rescans are effectively off. Linux only (the other platforms watch the
//! root recursively and are not covered here).
#![cfg(target_os = "linux")]

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
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn removed_root_is_not_fresh() {
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
    wait_for("initial index", Duration::from_secs(20), || {
        find(&e, &root, "first_marker") == (1, true)
    });
    wait_for("initial clean", Duration::from_secs(20), || {
        e.rescan_clean(&unit).is_some()
    });
    std::fs::remove_dir_all(&root).unwrap();
    wait_for("removed root still fresh", Duration::from_secs(10), || {
        !find(&e, &root, "first_marker").1
    });
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !find(&e, &root, "first_marker").1,
        "removed root became fresh again"
    );
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
