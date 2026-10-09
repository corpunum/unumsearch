// SPDX-License-Identifier: Apache-2.0
//! Memory stays bounded however much a query matches, and however many
//! events a busy watcher is sent. A counting allocator measures the heap
//! high-water mark of each phase (the 2026-10-09 daemon OOM: an unbounded
//! watcher queue filled with read events while the loop was busy).

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
use unumsearch::Config;

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let c = CUR.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(c, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                let c = CUR.fetch_add(new - l.size(), Ordering::Relaxed) + new - l.size();
                PEAK.fetch_max(c, Ordering::Relaxed);
            } else {
                CUR.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Phases measure the whole process heap, so they must not overlap.
static SERIAL: Mutex<()> = Mutex::new(());

/// Heap growth above the starting level while `f` runs.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let out = f();
    (out, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

const MB: usize = 1 << 20;

fn corpus(files: usize, lines: usize, width: usize) -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    let line = format!("needle {}\n", "x".repeat(width));
    let body = line.repeat(lines);
    for i in 0..files {
        let d = root.join(format!("d{:03}", i % 100));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("f{i:05}.txt")), &body).unwrap();
    }
    (t, root)
}

fn config(t: &Path, root: &Path) -> Config {
    Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.join("index").to_string_lossy().into_owned()),
        threads: 4,
        debounce_ms: 100,
        rescan_secs: 3600,
        ..Config::default()
    }
}

#[test]
fn a_query_matching_everything_stays_within_its_result_budget() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    // 2,000 files x 20 matching lines of ~1 KB: unbounded, the line results
    // alone would be about 45 MB of heap.
    let (t, root) = corpus(2000, 20, 1000);
    // No watcher here: a recent build is fresh.
    let cfg = Config {
        watch: false,
        ..config(t.path(), &root)
    };
    let e = Engine::open(cfg, true).unwrap();
    e.index_all(true);
    let budget = 4 * MB;
    let opts = SearchOpts {
        pattern: "needle".into(),
        root: root.clone(),
        max_matches: 200_000,
        max_files: 1_000_000,
        max_result_bytes: budget,
        ..Default::default()
    };
    let (r, grew) = peak_growth(|| e.search(&opts).unwrap());
    assert!(r.truncated, "the budget must cut the answer");
    assert!(!r.complete, "a truncated answer is never complete");
    assert!(r.fresh && r.covered);
    let held = r.result_bytes();
    assert!(
        held <= budget,
        "answer holds {held} bytes > budget {budget}"
    );
    assert!(r.matches.len() > 1000, "only {} matches", r.matches.len());
    // Results, plus per-thread read buffers and the candidate list: well
    // below what the full answer would take.
    assert!(
        grew < 16 * MB,
        "heap grew {} MB during the query",
        grew / MB
    );

    // The cut is deterministic (path order, not thread timing) and is a
    // prefix of the full answer.
    let again = e.search(&opts).unwrap();
    assert_eq!(r.files, again.files);
    assert_eq!(r.matches.len(), again.matches.len());
    let full = e
        .search(&SearchOpts {
            max_result_bytes: 1 << 30,
            ..opts.clone()
        })
        .unwrap();
    assert!(!full.truncated && full.complete);
    assert_eq!(full.matches.len(), 40_000);
    assert!(full.result_bytes() > 40 * MB);
    for (a, b) in r.matches.iter().zip(&full.matches) {
        assert_eq!((&a.path, a.line), (&b.path, b.line));
    }

    // Files-only answers are bounded by the same budget.
    let (fo, grew) = peak_growth(|| {
        e.search(&SearchOpts {
            files_only: true,
            max_result_bytes: 64 * 1024,
            ..opts.clone()
        })
        .unwrap()
    });
    assert!(fo.truncated && !fo.complete);
    assert!(fo.files.len() < 2000 && !fo.files.is_empty());
    assert!(grew < 8 * MB, "heap grew {} MB", grew / MB);
    assert_eq!(fo.files[..], full.files[..fo.files.len()]);
}

#[test]
fn a_busy_watcher_does_not_queue_read_events() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (t, root) = corpus(50, 1, 10);
    let e = Arc::new(Engine::open(config(t.path(), &root), true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    let opts = SearchOpts {
        pattern: "needle".into(),
        root: root.clone(),
        files_only: true,
        ..Default::default()
    };
    let t0 = Instant::now();
    while !(e.search(&opts).unwrap().fresh && e.status()["units"][0]["watched"] == true) {
        assert!(t0.elapsed() < Duration::from_secs(20), "never fresh");
        std::thread::sleep(Duration::from_millis(20));
    }
    let files: Vec<PathBuf> = e
        .search(&opts)
        .unwrap()
        .files
        .iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(files.len(), 50);
    // The loop is busy (as during a long rebuild) while other processes read
    // the corpus: every open() is an inotify event.
    unumsearch::watch::STALL_LOOP_MS.store(4000, Ordering::Relaxed);
    // Past the current drain window: the loop is now asleep.
    std::thread::sleep(Duration::from_millis(500));
    let ((), grew) = peak_growth(|| {
        for _ in 0..1500 {
            for f in &files {
                drop(std::fs::File::open(f).unwrap());
            }
        }
        // Let the watcher thread take the kernel queue.
        std::thread::sleep(Duration::from_millis(300));
    });
    eprintln!("read flood: heap grew {} KB", grew / 1024);
    // 75,000 opens: queued, they took 14 MB here before the fix.
    assert!(grew < 4 * MB, "heap grew {} KB", grew / 1024);
    // Writes are real changes, but the queue holding them is bounded too: an
    // overflow marks every unit dirty (re-listed and rebuilt) instead.
    std::thread::sleep(Duration::from_millis(4500));
    let ((), grew) = peak_growth(|| {
        for i in 0..800 {
            for f in &files {
                std::fs::write(f, format!("needle {i}\n")).unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    });
    unumsearch::watch::STALL_LOOP_MS.store(0, Ordering::Relaxed);
    eprintln!("write flood: heap grew {} KB", grew / 1024);
    assert!(grew < 12 * MB, "heap grew {} KB", grew / 1024);
    // Changes are still noticed afterwards.
    std::fs::write(root.join("d000/new.txt"), "needle fresh\n").unwrap();
    let t1 = Instant::now();
    loop {
        let r = e.search(&opts).unwrap();
        if r.fresh && r.files.len() == 51 {
            break;
        }
        assert!(t1.elapsed() < Duration::from_secs(20), "change never seen");
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
}
