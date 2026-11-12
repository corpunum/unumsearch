// SPDX-License-Identifier: Apache-2.0
//! A directory that holds no indexed file (empty, or only ignored files) is
//! watched too: a file created there is found at once, not at the next
//! rescan. Rescans are effectively off here (rescan_secs 3600).

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
fn file_created_in_a_dir_without_indexed_files_is_seen() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("empty/deeper")).unwrap();
    std::fs::create_dir_all(root.join("only_ignored")).unwrap();
    std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(root.join("only_ignored/x.log"), "nothing\n").unwrap();
    std::fs::write(root.join("src/a.txt"), "first_marker\n").unwrap();
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
    // Clean = rebuilt with nothing pending, so the next file can only be
    // found through its own watch (not by a pending rebuild's listing).
    let settle = |what: &str| {
        wait_for(what, Duration::from_secs(20), || {
            e.rescan_clean(&unit).is_some()
        })
    };
    wait_for("initial index never fresh", Duration::from_secs(20), || {
        find(&e, &root, "first_marker") == (1, true)
    });
    settle("initial index never clean");
    for (rel, marker) in [
        ("empty/deeper/n.txt", "deep_empty_marker"),
        ("only_ignored/n.txt", "ignored_dir_marker"),
    ] {
        std::fs::write(root.join(rel), format!("{marker}\n")).unwrap();
        wait_for(rel, Duration::from_secs(5), || {
            find(&e, &root, marker) == (1, true)
        });
        settle(rel);
    }
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
