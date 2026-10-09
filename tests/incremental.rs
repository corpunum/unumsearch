// SPDX-License-Identifier: Apache-2.0
//! Incremental rebuilds: only files whose size or change stamp moved (or
//! that an event reported) are read again; everything else keeps its
//! postings. These cases pin down what decides "unchanged".

use std::path::Path;
use unumsearch::engine::{Change, Engine, SearchOpts};
use unumsearch::Config;

fn engine(t: &Path, root: &Path) -> Engine {
    let cfg = Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.join("index").to_string_lossy().into_owned()),
        threads: 2,
        rescan_secs: 3600,
        watch: false,
        ..Config::default()
    };
    Engine::open(cfg, true).unwrap()
}

fn files(e: &Engine, root: &Path, pat: &str) -> (Vec<String>, bool) {
    let r = e
        .search(&SearchOpts {
            pattern: pat.into(),
            root: root.to_path_buf(),
            files_only: true,
            ..Default::default()
        })
        .unwrap();
    let mut v: Vec<String> = r
        .files
        .iter()
        .map(|f| {
            Path::new(f)
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    v.sort();
    (v, r.fresh)
}

fn shard_names(t: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(t.join("index"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".shard"))
        .collect();
    v.sort();
    v
}

/// A rewrite that keeps the size and puts the old mtime back is invisible
/// to a listing on a filesystem without a usable ctime; the watcher's event
/// for it must still get it read again.
#[test]
fn a_reported_change_is_read_again_even_with_size_and_mtime_unchanged() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let p = root.join("src/a.txt");
    std::fs::write(&p, "old_token_x\n").unwrap();
    std::fs::write(root.join("src/b.txt"), "other\n").unwrap();
    let e = engine(t.path(), &root);
    e.set_racy_window_ns(0);
    e.index_all(false);
    assert_eq!(files(&e, &root, "old_token_x").0, vec!["src/a.txt"]);

    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
    std::fs::write(&p, "new_token_y\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let unit = root.to_string_lossy().into_owned();
    e.mark_dirty(&unit, Change::File("src/a.txt".into()));
    e.build_unit(&unit, false).unwrap();
    let (got, fresh) = files(&e, &root, "new_token_y");
    assert!(fresh);
    assert_eq!(got, vec!["src/a.txt"]);
    // The old content is gone from the index, not just filtered by reading.
    let r = e
        .search(&SearchOpts {
            pattern: "old_token_x".into(),
            root: root.clone(),
            files_only: true,
            candidates_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.candidates, 0);
}

/// One edit in a large unit rewrites only the shard that held the file
/// (plus the small new piece); the unit's other shards stay as they are.
#[test]
fn one_edit_keeps_the_shards_that_did_not_change() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    for d in 0..8 {
        for f in 0..40 {
            let dir = root.join(format!("d{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            let body: String = (0..60)
                .map(|i| format!("tok_{d}_{f}_{i} "))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(dir.join(format!("f{f}.txt")), body).unwrap();
        }
    }
    let e = engine(t.path(), &root);
    e.set_racy_window_ns(0);
    // Shards of about 16 KiB; pieces flushed small so there are several.
    e.set_build_sizes(16 << 10, 8 << 10);
    e.index_all(false);
    let unit = root.to_string_lossy().into_owned();
    // The first rebuild may still combine the initial build's small shards;
    // after that the layout is settled.
    for i in 0..2 {
        std::fs::write(root.join("d0/f0.txt"), format!("warm_{i}\n")).unwrap();
        e.mark_dirty(&unit, Change::File("d0/f0.txt".into()));
        e.build_unit(&unit, false).unwrap();
    }
    let before = shard_names(t.path());
    assert!(before.len() >= 4, "{before:?}");

    std::fs::write(root.join("d3/f7.txt"), "edited_token_q\n").unwrap();
    e.mark_dirty(&unit, Change::File("d3/f7.txt".into()));
    e.build_unit(&unit, false).unwrap();
    let after = shard_names(t.path());
    let kept = before.iter().filter(|n| after.contains(n)).count();
    assert!(
        kept + 2 >= before.len(),
        "most shards should be kept: {before:?} -> {after:?}"
    );
    assert_eq!(
        files(&e, &root, "edited_token_q"),
        (vec!["d3/f7.txt".to_string()], true)
    );
    assert_eq!(files(&e, &root, "tok_3_7_5").0, Vec::<String>::new());
    assert_eq!(files(&e, &root, "tok_5_9_59").0, vec!["d5/f9.txt"]);
}
