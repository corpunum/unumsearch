// SPDX-License-Identifier: Apache-2.0
//! The serial and the parallel listing walkers return identical results.
use std::fs;
use unumsearch::config::Config;
use unumsearch::walk;

fn w(p: &std::path::Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

#[test]
fn serial_and_parallel_listings_are_identical() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    fs::create_dir(r.join(".git")).unwrap();
    w(&r.join(".gitignore"), "*.log\n/build/\n");
    w(&r.join("a.rs"), "fn a() {}");
    w(&r.join("x.log"), "ignored");
    w(&r.join("build/o.rs"), "ignored dir");
    w(&r.join("src/.gitignore"), "gen_*\n!keep.log\n");
    w(&r.join("src/lib.rs"), "lib");
    w(&r.join("src/gen_a.rs"), "ignored nested");
    w(&r.join("src/keep.log"), "negated");
    w(&r.join("src/deep/er/f.txt"), "deep");
    w(&r.join("src/deep/.ignore"), "skip.txt\n");
    w(&r.join("src/deep/skip.txt"), "x");
    w(&r.join(".hidden/h.txt"), "hidden dir");
    w(&r.join(".dotfile"), "hidden");
    w(&r.join("node_modules/m/i.js"), "default-excluded");
    w(&r.join("secret/.env"), "KEY=1");
    w(&r.join("empty/.keep_not"), "x");
    fs::create_dir_all(r.join("lonely/dir")).unwrap();
    w(&r.join("big.bin"), &"x".repeat(4096));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(r.join("a.rs"), r.join("link.rs")).unwrap();
        std::os::unix::fs::symlink(r.join("src"), r.join("linkdir")).unwrap();
    }
    for (hidden, gitignore, max) in [
        (false, true, 1000u64),
        (true, false, 1000),
        (false, true, 100_000),
    ] {
        let cfg = Config {
            hidden,
            gitignore,
            excludes: vec!["/src/deep/er/".into(), "*.tmp".into()],
            max_file_size: max,
            ..Config::default()
        };
        for with_dirs in [false, true] {
            for depth in [None, Some(1)] {
                let (pf, mut pd) = walk::list_full_forced(&cfg, r, depth, with_dirs, false);
                let (sf, mut sd) = walk::list_full_forced(&cfg, r, depth, with_dirs, true);
                let key = |v: &[walk::FileEntry]| {
                    v.iter()
                        .map(|f| (f.rel.clone(), f.size, f.mtime_ns))
                        .collect::<Vec<_>>()
                };
                assert_eq!(key(&pf), key(&sf));
                pd.sort();
                sd.sort();
                assert_eq!(pd, sd);
                assert!(!pf.is_empty());
                assert!(!sf
                    .iter()
                    .any(|f| f.rel == "secret/.env" || f.rel.starts_with("node_modules/")));
                let (hf, _) = walk::list_full_hinted(&cfg, r, depth, with_dirs, Some(3));
                assert_eq!(key(&pf), key(&hf));
            }
        }
    }
    let cfg = Config::default();
    let names: Vec<String> = walk::list(&cfg, r).into_iter().map(|f| f.rel).collect();
    assert!(names.contains(&"a.rs".to_string()));
    assert!(!names.iter().any(|n| n.ends_with(".log") && n == "x.log"));
    assert!(!names.iter().any(|n| n.starts_with("build/")));
    #[cfg(unix)]
    assert!(!names.iter().any(|n| n.starts_with("linkdir")));
}
