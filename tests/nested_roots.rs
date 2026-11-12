// SPDX-License-Identifier: Apache-2.0
//! Roots may nest (A and A/x/c). Renaming a directory of A away and back
//! removes the watches of the nested root's directories too; that root must
//! be rebuilt and watched again, not left `fresh` with dead watches. Rescans
//! are effectively off. Linux only.
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
fn nested_root_is_watched_again_after_an_ancestor_directory_is_renamed() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    let c = a.join("x/c");
    std::fs::create_dir_all(a.join(".git")).unwrap();
    std::fs::create_dir_all(c.join(".git")).unwrap();
    std::fs::write(a.join("a.txt"), "outer_first\n").unwrap();
    std::fs::write(c.join("c.txt"), "inner_first\n").unwrap();
    let cfg = Config {
        roots: vec![
            a.to_string_lossy().into_owned(),
            c.to_string_lossy().into_owned(),
        ],
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
    let (au, cu) = (
        a.to_string_lossy().into_owned(),
        c.to_string_lossy().into_owned(),
    );
    wait_for("initial index", Duration::from_secs(20), || {
        find(&e, &c, "inner_first") == (1, true) && find(&e, &a, "outer_first") == (1, true)
    });
    wait_for("initial clean", Duration::from_secs(20), || {
        e.rescan_clean(&au).is_some() && e.rescan_clean(&cu).is_some()
    });
    let (x, old) = (a.join("x"), a.join("x.old"));
    std::fs::rename(&x, &old).unwrap();
    std::fs::rename(&old, &x).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    wait_for("clean after renames", Duration::from_secs(20), || {
        e.rescan_clean(&au).is_some() && e.rescan_clean(&cu).is_some()
    });
    std::fs::write(c.join("new.txt"), "inner_new_marker\n").unwrap();
    wait_for("file in the nested root", Duration::from_secs(5), || {
        find(&e, &c, "inner_new_marker") == (1, true)
    });
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
