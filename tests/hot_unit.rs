// SPDX-License-Identifier: Apache-2.0
//! A "hot" unit: a directory in which an indexed file is rewritten all the
//! time (a state file, a journal). Without a limit the watcher rebuilt such a
//! unit back to back, rewriting a shard and the manifest every couple of
//! seconds for as long as the writer ran. The engine now keeps a minimum
//! interval between two rebuilds of a unit, proportional to how long its
//! last rebuild took; searches stay exact meanwhile because pending files are
//! verified directly.

use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Change, Engine, SearchOpts};
use unumsearch::Config;

fn setup(factor: u64, max_ms: u64) -> (tempfile::TempDir, std::path::PathBuf, Arc<Engine>) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/state.json"), "token_0\n").unwrap();
    for i in 0..20 {
        std::fs::write(root.join(format!("f{i}.txt")), format!("cold_{i}\n")).unwrap();
    }
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.path().join("index").to_string_lossy().into_owned()),
        threads: 2,
        rescan_secs: 3600,
        watch: false,
        debounce_ms: 10,
        max_wait_ms: 50,
        rebuild_backoff_factor: factor,
        rebuild_backoff_max_ms: max_ms,
        ..Config::default()
    };
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    e.index_all(false);
    (t, root, e)
}

fn q(e: &Engine, root: &std::path::Path, pat: &str) -> (Vec<String>, bool) {
    let r = e
        .search(&SearchOpts {
            pattern: pat.into(),
            root: root.to_path_buf(),
            files_only: true,
            ..Default::default()
        })
        .unwrap();
    (r.files, r.fresh)
}

/// Rewrite the hot file every 25 ms for `for_ms`, running the watcher's
/// "build what is due" step each round, with every build stretched to at
/// least `stall` ms. Every round checks that a search is exact. Returns how
/// many rebuilds ran.
fn hammer(e: &Arc<Engine>, root: &std::path::Path, for_ms: u64, stall: u64) -> usize {
    let unit = root.to_string_lossy().into_owned();
    e.set_build_stall_ms(stall);
    let end = Instant::now() + Duration::from_millis(for_ms);
    let (mut builds, mut i) = (0usize, 0u64);
    while Instant::now() < end {
        i += 1;
        std::fs::write(root.join("data/state.json"), format!("token_{i}\n")).unwrap();
        e.mark_dirty(&unit, Change::File("data/state.json".into()));
        for u in e.due_units() {
            assert!(e.build_unit(&u, false).is_some());
            builds += 1;
        }
        // Exact at every moment: the current content is found, the previous
        // content is not, a file that never changed still is, and the answer
        // says it is fresh.
        let (files, fresh) = q(e, root, &format!("token_{i}"));
        assert_eq!(files.len(), 1, "round {i}: current content not found");
        assert!(fresh, "round {i}: not fresh");
        let (files, _) = q(e, root, &format!("token_{}", i - 1));
        assert!(files.is_empty(), "round {i}: stale content still found");
        assert_eq!(q(e, root, "cold_7").0.len(), 1);
        std::thread::sleep(Duration::from_millis(25));
    }
    e.set_build_stall_ms(0);
    builds
}

#[test]
fn hot_unit_is_rebuilt_at_a_bounded_rate_and_stays_exact() {
    // Builds take >= 60 ms, so the interval is >= 20 x 60 = 1.2 s: at most
    // two rebuilds fit into 1.3 s (the first is due at once: the initial
    // index took a few ms).
    let (_t, root, e) = setup(20, 300_000);
    let builds = hammer(&e, &root, 1300, 60);
    assert!((1..=2).contains(&builds), "rebuilds: {builds}");
    // Once the writer stops and the interval has passed, the unit is
    // rebuilt and clean again.
    let unit = root.to_string_lossy().into_owned();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        for u in e.due_units() {
            e.build_unit(&u, false);
        }
        let st = e.status();
        let dirty = st["units"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["path"] == unit.as_str())
            .unwrap()["dirty"]
            .as_bool()
            .unwrap();
        if !dirty {
            break;
        }
        assert!(Instant::now() < end, "unit never became clean");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(e.due_units().is_empty());
}

#[test]
fn without_the_backoff_the_same_unit_is_rebuilt_back_to_back() {
    let (_t, root, e) = setup(0, 300_000);
    let builds = hammer(&e, &root, 1300, 60);
    assert!(builds >= 3, "rebuilds: {builds}");
}

#[test]
fn the_interval_is_capped() {
    // A build of >= 100 ms would mean an interval of >= 2 s; the cap is 200 ms.
    let (_t, root, e) = setup(20, 200);
    let unit = root.to_string_lossy().into_owned();
    let wait_due = |limit_ms: u64| {
        let t0 = Instant::now();
        while e.due_units().is_empty() {
            assert!(
                t0.elapsed() < Duration::from_millis(limit_ms),
                "not due after {limit_ms} ms"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    e.set_build_stall_ms(100);
    e.mark_dirty(&unit, Change::File("data/state.json".into()));
    wait_due(5000);
    assert!(e.build_unit(&unit, false).is_some());
    e.set_build_stall_ms(0);
    e.mark_dirty(&unit, Change::File("data/state.json".into()));
    assert!(e.due_units().is_empty(), "due right after a build");
    wait_due(1500);
}

#[test]
fn an_unknown_change_waits_only_twice_the_build_time() {
    let (_t, root, e) = setup(20, 300_000);
    let unit = root.to_string_lossy().into_owned();
    // One stretched build so that build_ms is about 100.
    e.set_build_stall_ms(100);
    std::fs::write(root.join("data/state.json"), "token_x\n").unwrap();
    e.mark_dirty(&unit, Change::File("data/state.json".into()));
    let t0 = Instant::now();
    while e.due_units().is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "first build never due"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(e.build_unit(&unit, false).is_some());
    e.set_build_stall_ms(0);
    // A change the overlay cannot represent: the unit is not fresh, so it
    // must not wait 20 x build time (2 s), only 2 x (about 200 ms).
    e.mark_dirty(&unit, Change::Unknown);
    assert!(!q(&e, &root, "token_x").1);
    std::thread::sleep(Duration::from_millis(30));
    assert!(e.due_units().is_empty(), "rebuilt right after a build");
    let t0 = Instant::now();
    while e.due_units().is_empty() {
        assert!(
            t0.elapsed() < Duration::from_millis(1500),
            "waited too long"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(e.build_unit(&unit, false).is_some());
    assert!(q(&e, &root, "token_x").1);
}
