// SPDX-License-Identifier: Apache-2.0
//! Recovery cost of an event-queue overflow on a tree like the live rig's.
//!
//! Loads the default config read-only, drops the roots named in the
//! `SKIP_ROOTS` env var (comma separated, default `/data2`), indexes the rest
//! into a fresh temp index dir, then forces `OVERFLOW_RUNS` (default 3)
//! overflows and samples every 100 ms until the index is fresh again.
//! Reports time, watch count, CPU and RSS. Linux only for the measurements.
//!
//!   cargo run --release --example overflow_bench

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::Engine;
use unumsearch::Config;

/// inotify watches held by this process (Linux; 0 elsewhere).
fn watches() -> usize {
    #[cfg(target_os = "linux")]
    {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir("/proc/self/fdinfo") {
            for e in rd.flatten() {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    n += t.lines().filter(|l| l.starts_with("inotify wd")).count();
                }
            }
        }
        n
    }
    #[cfg(not(target_os = "linux"))]
    0
}

/// CPU seconds (user + system) of this process (Linux; 0 elsewhere).
fn cpu_secs() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/stat") {
            if let Some(rest) = s.rsplit(')').next() {
                let f: Vec<&str> = rest.split_whitespace().collect();
                // after ")": state is f[0]; utime and stime are fields 14, 15
                if f.len() > 13 {
                    let t: f64 =
                        f[11].parse::<f64>().unwrap_or(0.0) + f[12].parse::<f64>().unwrap_or(0.0);
                    return t / 100.0; // USER_HZ
                }
            }
        }
    }
    0.0
}

fn rss_peak_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for l in s.lines() {
                if let Some(v) = l.strip_prefix("VmHWM:") {
                    let kb: f64 = v
                        .trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse()
                        .unwrap_or(0.0);
                    return kb / 1024.0;
                }
            }
        }
    }
    0.0
}

fn num(v: &serde_json::Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

fn main() {
    let skip: Vec<String> = std::env::var("SKIP_ROOTS")
        .unwrap_or_else(|_| "/data2".into())
        .split(',')
        .map(str::to_string)
        .collect();
    let runs: usize = std::env::var("OVERFLOW_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut cfg = Config::load(None).expect("config");
    cfg.roots.retain(|r| !skip.contains(r));
    cfg.split_roots.retain(|r| !skip.contains(r));
    let base = std::env::var("BENCH_TMP").unwrap_or_else(|_| "/tmp/claude-1000".into());
    let dir = std::path::PathBuf::from(base).join(format!("overflow-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    cfg.index_dir = Some(dir.to_string_lossy().into_owned());
    cfg.rescan_secs = 3600;
    println!("roots: {:?}", cfg.roots);

    let engine = Arc::new(Engine::open(cfg, true).expect("open"));
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (engine.clone(), stop.clone());
    let cpu0 = cpu_secs();
    let t0 = Instant::now();
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    let mut peak = 0usize;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        peak = peak.max(watches());
        let st = engine.status();
        if st["fresh"] == true && num(&st, "units_dirty") == 0 && watches() > 0 {
            break;
        }
        if t0.elapsed() > Duration::from_secs(1800) {
            println!("initial index timed out");
            break;
        }
    }
    let st = engine.status();
    let init_w = watches();
    println!(
        "INITIAL: fresh in {:.1}s cpu {:.1}s units {} files {} watches {} (peak {}) rss_peak {:.0}MB",
        t0.elapsed().as_secs_f64(),
        cpu_secs() - cpu0,
        num(&st, "units_total"),
        num(&st, "files"),
        init_w,
        peak,
        rss_peak_mb()
    );
    std::thread::sleep(Duration::from_secs(3));

    for run in 1..=runs {
        let cpu0 = cpu_secs();
        let t0 = Instant::now();
        unumsearch::watch::FORCE_OVERFLOW.store(true, Ordering::Relaxed);
        let (mut minw, mut maxw) = (usize::MAX, 0usize);
        let (mut bad, mut samples, mut fresh_at) = (0, 0, None);
        let mut max_dirty = 0;
        // The flag is consumed within ~200 ms; require it consumed first.
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let w = watches();
            minw = minw.min(w);
            maxw = maxw.max(w);
            let st = engine.status();
            let dirty = num(&st, "units_dirty");
            max_dirty = max_dirty.max(dirty);
            let fresh = st["fresh"] == true;
            samples += 1;
            if fresh && (dirty > 0 || w < init_w) {
                bad += 1;
                println!(
                    "  bad sample at {:?}: fresh=true dirty={dirty} watches={w} consumed={}",
                    t0.elapsed(),
                    !unumsearch::watch::FORCE_OVERFLOW.load(Ordering::Relaxed)
                );
            }
            let consumed = !unumsearch::watch::FORCE_OVERFLOW.load(Ordering::Relaxed);
            if consumed && fresh && dirty == 0 && w >= init_w {
                fresh_at = Some(t0.elapsed());
                break;
            }
            if t0.elapsed() > Duration::from_secs(900) {
                break;
            }
        }
        println!(
            "OVERFLOW {run}: fresh again in {:?}s cpu {:.1}s watches min {} max {} (initial {}) max_dirty {} samples {} bad(fresh while dirty/low watches) {} rss_peak {:.0}MB rescans {}",
            fresh_at.map(|d| d.as_secs_f64()),
            cpu_secs() - cpu0,
            minw,
            maxw,
            init_w,
            max_dirty,
            samples,
            bad,
            rss_peak_mb(),
            unumsearch::watch::RESCANS.load(Ordering::Relaxed)
        );
        std::thread::sleep(Duration::from_secs(3));
    }
    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    drop(engine);
    let _ = std::fs::remove_dir_all(&dir);
    println!("temp index dir removed: {}", !dir.exists());
}
