// SPDX-License-Identifier: Apache-2.0
//! Times `walk::list` over every unit of the live manifest (read-only).
//! `list_bench [runs]`; set LIST_BENCH_PHASES=1 for the matcher-build share.
use std::path::{Path, PathBuf};
use std::time::Instant;
use unumsearch::config::Config;
use unumsearch::walk;

fn main() {
    let cfg = Config::load_with(None, true).expect("config");
    let manifest = cfg.index_dir().join("manifest.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).expect("manifest")).unwrap();
    let units: Vec<PathBuf> = v["units"]
        .as_object()
        .unwrap()
        .keys()
        .map(PathBuf::from)
        .collect();
    let phases = std::env::var_os("LIST_BENCH_PHASES").is_some();
    let runs: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    // LIST_BENCH_HINT=1: pass each unit's previous file count, as the daemon does.
    let hint = std::env::var_os("LIST_BENCH_HINT").is_some();
    let prev: Vec<usize> = if hint {
        units.iter().map(|u| walk::list(&cfg, u).len()).collect()
    } else {
        Vec::new()
    };
    for _ in 0..runs {
        let mut rows: Vec<(f64, usize, &Path)> = Vec::new();
        let t0 = Instant::now();
        for (i, u) in units.iter().enumerate() {
            let t = Instant::now();
            let n = if hint {
                walk::list_full_hinted(&cfg, u, None, false, Some(prev[i]))
                    .0
                    .len()
            } else {
                walk::list(&cfg, u).len()
            };
            rows.push((t.elapsed().as_secs_f64(), n, u));
        }
        let total = t0.elapsed().as_secs_f64();
        let small: f64 = rows.iter().filter(|r| r.1 < 200).map(|r| r.0).sum();
        let nsmall = rows.iter().filter(|r| r.1 < 200).count();
        let files: usize = rows.iter().map(|r| r.1).sum();
        println!(
            "units={} files={files} total_wall={total:.3}s small(<200 files, {nsmall} units)={small:.3}s",
            units.len()
        );
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for r in rows.iter().take(5) {
            println!("  top {:.3}s files={} {}", r.0, r.1, r.2.display());
        }
        if phases {
            let t = Instant::now();
            for u in &units {
                std::hint::black_box(walk::exclude_matcher(&cfg, u));
            }
            println!(
                "  matcher build, all units: {:.3}s",
                t.elapsed().as_secs_f64()
            );
            // Small units only, per unit: best of 3 for each variant, so
            // load noise on a shared rig cancels.
            let (mut par, mut ser, mut mat) = (0.0, 0.0, 0.0);
            for r in rows.iter().filter(|r| r.1 < 200) {
                let best = |f: &dyn Fn()| {
                    (0..3)
                        .map(|_| {
                            let t = Instant::now();
                            f();
                            t.elapsed().as_secs_f64()
                        })
                        .fold(f64::MAX, f64::min)
                };
                let u = r.2;
                par += best(&|| drop(walk::list_full_forced(&cfg, u, None, false, false)));
                ser += best(&|| drop(walk::list_full_forced(&cfg, u, None, false, true)));
                mat += best(&|| drop(walk::exclude_matcher(&cfg, u)));
            }
            println!("  small units best-of-3: parallel {par:.3}s serial {ser:.3}s matcher-only {mat:.3}s");
        }
    }
}
