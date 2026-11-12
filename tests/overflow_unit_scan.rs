// SPDX-License-Identifier: Apache-2.0
//! After an event overflow the split root's watch is re-placed and the units
//! are scanned again: a child unit created while events were lost is found
//! (and a search over the split root never answers fresh without it).
//! Rescans are effectively off, so only the unit scan can find it. Linux only.
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
fn overflow_finds_a_unit_created_while_events_were_lost() {
    let t = tempfile::tempdir().unwrap();
    let s = t.path().join("ws");
    let a = s.join("a");
    std::fs::create_dir_all(a.join(".git")).unwrap();
    std::fs::write(a.join("a.txt"), "a_first\n").unwrap();
    let cfg = Config {
        roots: vec![s.to_string_lossy().into_owned()],
        split_roots: vec![s.to_string_lossy().into_owned()],
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
    let au = a.to_string_lossy().into_owned();
    wait_for("initial index", Duration::from_secs(20), || {
        find(&e, &a, "a_first") == (1, true)
    });
    wait_for("initial clean", Duration::from_secs(20), || {
        e.rescan_clean(&au).is_some()
    });
    unumsearch::watch::DROP_WATCHES
        .lock()
        .unwrap()
        .push(s.clone());
    wait_for("drop consumed", Duration::from_secs(5), || {
        unumsearch::watch::DROP_WATCHES.lock().unwrap().is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    let n = s.join("new");
    std::fs::create_dir_all(n.join(".git")).unwrap();
    std::fs::write(n.join("n.txt"), "new_unit_marker\n").unwrap();
    unumsearch::watch::FORCE_OVERFLOW.store(true, Ordering::Relaxed);
    wait_for("file in the new unit", Duration::from_secs(10), || {
        find(&e, &s, "new_unit_marker") == (1, true)
    });
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
