// SPDX-License-Identifier: Apache-2.0
//! The watched-directory record must not drift from the real watches while a
//! unit answers `fresh: true`. Rescans are effectively off (rescan_secs 3600),
//! so only a live watch can find a new file. The tests share process-global
//! hooks, hence one #[test].

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
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

fn start(cfg: Config) -> (Arc<Engine>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    (e, stop, h)
}

fn cfg(t: &Path, roots: &[&Path], split: &[&Path]) -> Config {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    Config {
        roots: roots.iter().map(|p| s(p)).collect(),
        split_roots: split.iter().map(|p| s(p)).collect(),
        index_dir: Some(s(&t.join("index"))),
        threads: 2,
        debounce_ms: 50,
        rescan_secs: 3600,
        ..Config::default()
    }
}

fn overflow_with_stale_entry() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("lost")).unwrap();
    std::fs::write(root.join("src/a.txt"), "first_marker\n").unwrap();
    let (e, stop, h) = start(cfg(t.path(), &[&root], &[]));
    let unit = root.to_string_lossy().into_owned();
    let settle = |what: &str| {
        wait_for(what, Duration::from_secs(20), || {
            e.rescan_clean(&unit).is_some()
        })
    };
    wait_for("initial index", Duration::from_secs(20), || {
        find(&e, &root, "first_marker") == (1, true)
    });
    settle("initial clean");
    unumsearch::watch::DROP_WATCHES
        .lock()
        .unwrap()
        .push(root.join("lost"));
    wait_for("drop consumed", Duration::from_secs(5), || {
        unumsearch::watch::DROP_WATCHES.lock().unwrap().is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    unumsearch::watch::FORCE_OVERFLOW.store(true, Ordering::Relaxed);
    wait_for("overflow consumed", Duration::from_secs(5), || {
        !unumsearch::watch::FORCE_OVERFLOW.load(Ordering::Relaxed)
    });
    std::thread::sleep(Duration::from_millis(300));
    settle("clean after overflow");
    std::fs::write(root.join("lost/x.txt"), "lost_marker\n").unwrap();
    wait_for("file in a lost dir", Duration::from_secs(5), || {
        find(&e, &root, "lost_marker") == (1, true)
    });
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}

fn split_root_event() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let s = t.path().join("ws");
    let child = s.join("child");
    std::fs::create_dir_all(child.join(".git")).unwrap();
    std::fs::write(child.join("a.txt"), "child_first\n").unwrap();
    std::fs::write(s.join("loose.txt"), "loose_first\n").unwrap();
    let (e, stop, h) = start(cfg(t.path(), &[&s], &[&s]));
    // (The split root's own unit is flat and never reports clean.)
    let cu = child.to_string_lossy().into_owned();
    let settle = |what: &str| {
        wait_for(what, Duration::from_secs(20), || {
            e.rescan_clean(&cu).is_some()
        })
    };
    wait_for("initial index", Duration::from_secs(20), || {
        find(&e, &child, "child_first") == (1, true)
    });
    settle("initial clean");
    let mode = std::fs::metadata(&s).unwrap().permissions().mode();
    std::fs::set_permissions(&s, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&s, std::fs::Permissions::from_mode(mode & 0o7777)).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    settle("clean after chmod");
    std::fs::write(child.join("new.txt"), "child_new_marker\n").unwrap();
    wait_for("file in a child unit", Duration::from_secs(5), || {
        find(&e, &child, "child_new_marker") == (1, true)
    });
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}

#[test]
fn watch_record_does_not_drift() {
    if !cfg!(target_os = "linux") {
        return;
    }
    overflow_with_stale_entry();
    split_root_event();
}
