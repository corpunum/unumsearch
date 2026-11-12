// SPDX-License-Identifier: Apache-2.0
//! A split root that is removed and created again (`rm -rf workspaces &&
//! mkdir workspaces`) must be watched again: a new child unit created in it
//! is found by the root's own watch, not only by a rescan. Rescans are fast
//! here because a deleted split root may only be rediscovered by one.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
use unumsearch::watch::{RESCANS, RESCAN_MS};
use unumsearch::Config;

fn find(e: &Engine, root: &Path, pat: &str) -> (usize, bool) {
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

fn unit_with(dir: &Path, marker: &str) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join("f.txt"), format!("{marker}\n")).unwrap();
}

#[test]
fn split_root_removed_and_created_again_is_watched_again() {
    RESCAN_MS.store(300, Ordering::Relaxed);
    let t = tempfile::tempdir().unwrap();
    let s = t.path().join("ws");
    let a = s.join("a");
    unit_with(&a, "first_marker");
    let sp = s.to_string_lossy().into_owned();
    let cfg = Config {
        roots: vec![sp.clone()],
        split_roots: vec![sp],
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
    let settle = |dir: &Path| {
        let u = dir.to_string_lossy().into_owned();
        wait_for("unit never clean", Duration::from_secs(20), || {
            e.rescan_clean(&u).is_some()
        });
    };
    wait_for("initial index", Duration::from_secs(20), || {
        find(&e, &a, "first_marker") == (1, true)
    });
    settle(&a);
    std::fs::remove_dir_all(&s).unwrap();
    wait_for("removal not seen", Duration::from_secs(10), || {
        find(&e, &s, "first_marker").0 == 0
    });
    unit_with(&a, "second_marker");
    wait_for("re-created unit", Duration::from_secs(15), || {
        find(&e, &a, "second_marker") == (1, true)
    });
    settle(&a);
    let mut by_watch = false;
    for i in 0..3 {
        let marker = format!("later_marker_{i}");
        let b = s.join(format!("b{i}"));
        let r0 = RESCANS.load(Ordering::Relaxed);
        unit_with(&b, &marker);
        wait_for("new child unit", Duration::from_secs(10), || {
            find(&e, &b, &marker) == (1, true)
        });
        by_watch |= RESCANS.load(Ordering::Relaxed) == r0;
        settle(&b);
    }
    if cfg!(target_os = "linux") {
        assert!(by_watch, "only rescans found new child units");
    }
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
