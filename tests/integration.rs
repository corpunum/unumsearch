// SPDX-License-Identifier: Apache-2.0
//! End-to-end behaviour on a synthetic corpus: corpus rules (gitignore,
//! hidden, binary, size, secrets), query semantics, freshness, and - when a
//! `rg` binary is available - file-set equivalence with ripgrep.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, FilesOpts, SearchOpts};
use unumsearch::Config;

fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root, ".gitignore", b"ignored/\n*.log\n");
    write(&root, "src/a.rs", b"fn hello_world() {}\n// foo.bar here\n");
    write(&root, "src/b.txt", b"Hello World\nsecond line fooXbar\n");
    write(
        &root,
        "src/nested/deep/c.md",
        b"# Title\nhello_world in docs\n",
    );
    write(&root, "ignored/x.rs", b"hello_world\n");
    write(&root, "debug.log", b"hello_world\n");
    write(&root, "node_modules/pkg/index.js", b"hello_world\n");
    write(&root, ".hidden/h.md", b"hello_world hidden\n");
    write(&root, ".env", b"HELLO_WORLD=secret\n");
    write(&root, "keys/id_rsa", b"hello_world private\n");
    write(&root, "config/secrets.json", b"{\"hello_world\": 1}\n");
    write(&root, "data.bin.dat", b"hello_world\x00\x01\x02");
    write(&root, "unicode.txt", "Grüße ΑΒΓ hello_world\n".as_bytes());
    let mut big = b"hello_world\n".to_vec();
    big.resize(300_000, b'x');
    write(&root, "big.txt", &big);
    (t, root)
}

fn config(t: &Path, root: &Path) -> Config {
    Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.join("index").to_string_lossy().into_owned()),
        max_file_size: 100_000,
        threads: 2,
        debounce_ms: 100,
        rescan_secs: 3600,
        ..Config::default()
    }
}

fn rels(root: &Path, files: &[String]) -> Vec<String> {
    let mut v: Vec<String> = files
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
    v
}

fn search(
    e: &Engine,
    root: &Path,
    pat: &str,
    regex: bool,
    ci: bool,
    globs: &[&str],
) -> Vec<String> {
    let r = e
        .search(&SearchOpts {
            pattern: pat.into(),
            root: root.to_path_buf(),
            regex,
            case_insensitive: ci,
            globs: globs.iter().map(|s| s.to_string()).collect(),
            files_only: true,
            ..Default::default()
        })
        .unwrap();
    rels(root, &r.files)
}

#[test]
fn corpus_rules_and_query_semantics() {
    let (t, root) = fixture();
    // One-shot indexing, no watcher: freshness rests on the recent build.
    let cfg = Config {
        watch: false,
        ..config(t.path(), &root)
    };
    let e = Engine::open(cfg, true).unwrap();
    assert!(e.is_writer());
    e.index_all(false);

    // gitignore, node_modules, secrets, binary and oversized files are out;
    // hidden files are in.
    assert_eq!(
        search(&e, &root, "hello_world", false, false, &[]),
        vec![
            ".hidden/h.md",
            "src/a.rs",
            "src/nested/deep/c.md",
            "unicode.txt"
        ]
    );
    // Literal by default in the library: '.' is not a wildcard.
    assert_eq!(
        search(&e, &root, "foo.bar", false, false, &[]),
        vec!["src/a.rs"]
    );
    assert_eq!(
        search(&e, &root, "foo.bar", true, false, &[]),
        vec!["src/a.rs", "src/b.txt"]
    );
    // Case handling, including non-ASCII.
    assert_eq!(
        search(&e, &root, "hello world", false, true, &[]),
        vec!["src/b.txt"]
    );
    assert!(search(&e, &root, "hello world", false, false, &[]).is_empty());
    assert_eq!(
        search(&e, &root, "αβγ", false, true, &[]),
        vec!["unicode.txt"]
    );
    assert!(search(&e, &root, "αβγ", false, false, &[]).is_empty());
    // Globs and sub-directory roots.
    assert_eq!(
        search(&e, &root, "hello", false, true, &["*.rs"]),
        vec!["src/a.rs"]
    );
    assert_eq!(search(&e, &root, "hello", false, true, &["!*.rs"]).len(), 4);
    let sub = root.join("src/nested");
    assert_eq!(
        search(&e, &sub, "hello_world", false, false, &[]),
        vec!["deep/c.md"]
    );
    // Regex features that defeat trigrams still work (verified scan of all docs).
    assert_eq!(
        search(&e, &root, r"^#\s+\w+$", true, false, &[]),
        vec!["src/nested/deep/c.md"]
    );

    // Filename search.
    let f = e
        .files(&FilesOpts {
            root: root.clone(),
            globs: vec!["*.md".into()],
            regex: None,
            max_files: 100,
            scan_fallback: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        rels(&root, &f.files),
        vec![".hidden/h.md", "src/nested/deep/c.md"]
    );
    let f = e
        .files(&FilesOpts {
            root: root.clone(),
            globs: vec![],
            regex: Some("^uni".into()),
            max_files: 100,
            scan_fallback: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rels(&root, &f.files), vec!["unicode.txt"]);

    // Matches carry line numbers and text.
    let r = e
        .search(&SearchOpts {
            pattern: "second".into(),
            root: root.clone(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.matches.len(), 1);
    assert_eq!(r.matches[0].line, 2);
    assert_eq!(r.backend, "index");
    assert!(r.fresh);
}

#[test]
fn uncovered_root_falls_back_to_scan() {
    let (t, root) = fixture();
    let other = t.path().join("other");
    write(&other, "z.txt", b"needle\n");
    let e = Engine::open(config(t.path(), &root), true).unwrap();
    e.index_all(false);
    let r = e
        .search(&SearchOpts {
            pattern: "needle".into(),
            root: other.clone(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.backend, "scan");
    assert_eq!(r.files.len(), 1);
    let r = e
        .search(&SearchOpts {
            pattern: "needle".into(),
            root: other,
            scan_fallback: false,
            ..Default::default()
        })
        .unwrap();
    assert!(!r.covered && r.files.is_empty());
}

#[test]
fn rebuild_picks_up_changes_and_readers_reload() {
    let (t, root) = fixture();
    let cfg = config(t.path(), &root);
    let w = Engine::open(cfg.clone(), true).unwrap();
    w.index_all(false);
    let r = Engine::open(cfg, true).unwrap();
    assert!(!r.is_writer(), "second opener must be a reader");
    assert!(search(&r, &root, "brand_new_token", false, false, &[]).is_empty());
    write(&root, "src/new.rs", b"brand_new_token\n");
    std::fs::remove_file(root.join("src/a.rs")).unwrap();
    w.index_all(false);
    // Manifest mtime granularity can be coarse on some filesystems.
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        search(&r, &root, "brand_new_token", false, false, &[]),
        vec!["src/new.rs"]
    );
    assert!(!search(&r, &root, "fn hello", false, false, &[]).contains(&"src/a.rs".to_string()));
}

#[test]
fn watcher_keeps_index_fresh() {
    let (t, root) = fixture();
    let e = Arc::new(Engine::open(config(t.path(), &root), true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    // Wait for the initial index.
    let t0 = Instant::now();
    while !e
        .search(&SearchOpts {
            pattern: "hello_world".into(),
            root: root.clone(),
            ..Default::default()
        })
        .unwrap()
        .fresh
    {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "initial index never became fresh"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    write(&root, "src/nested/deep/c.md", b"freshly_written_marker\n");
    let t1 = Instant::now();
    loop {
        let r = e
            .search(&SearchOpts {
                pattern: "freshly_written_marker".into(),
                root: root.clone(),
                ..Default::default()
            })
            .unwrap();
        if r.fresh && r.files.len() == 1 {
            break;
        }
        assert!(
            t1.elapsed() < Duration::from_secs(20),
            "change never indexed"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    eprintln!("freshness lag: {:?}", t1.elapsed());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
}

/// File sets must equal ripgrep's on the same corpus definition.
#[test]
fn matches_ripgrep_when_available() {
    let rg = std::env::var("UNUMSEARCH_TEST_RG").unwrap_or_else(|_| "rg".into());
    if std::process::Command::new(&rg)
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("rg not found; skipping equivalence test");
        return;
    }
    let (t, root) = fixture();
    let cfg = config(t.path(), &root);
    let ignore_file = t.path().join("excludes.ignore");
    std::fs::write(&ignore_file, cfg.exclude_lines().join("\n")).unwrap();
    let e = Engine::open(cfg.clone(), true).unwrap();
    e.index_all(false);
    let cases: &[(&str, bool, bool)] = &[
        ("hello_world", false, false),
        ("hello", false, true),
        ("foo.bar", true, false),
        ("foo.bar", false, false),
        (r"h(ello|idden)", true, false),
        ("second line", false, false),
        ("αβγ", false, true),
    ];
    for (pat, regex, ci) in cases {
        let mut cmd = std::process::Command::new(&rg);
        cmd.current_dir(&root)
            .args([
                "-l",
                "--hidden",
                "--max-filesize",
                &cfg.max_file_size.to_string(),
                "--ignore-file",
            ])
            .arg(&ignore_file);
        if !regex {
            cmd.arg("-F");
        }
        if *ci {
            cmd.arg("-i");
        }
        let out = cmd.arg("-e").arg(pat).arg(".").output().unwrap();
        let mut want: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.replace('\\', "/").trim_start_matches("./").to_string())
            .collect();
        want.sort();
        assert_eq!(
            search(&e, &root, pat, *regex, *ci, &[]),
            want,
            "pattern {pat:?} regex={regex} ci={ci}"
        );
    }
}

#[test]
fn pending_changes_are_searchable_before_rebuild() {
    let (t, root) = fixture();
    let mut cfg = config(t.path(), &root);
    // A long debounce: the rebuild will not happen during this test.
    cfg.debounce_ms = 60_000;
    cfg.max_wait_ms = 60_000;
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    let t0 = Instant::now();
    while !e
        .search(&SearchOpts {
            pattern: "hello_world".into(),
            root: root.clone(),
            ..Default::default()
        })
        .unwrap()
        .fresh
    {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(50));
    }
    // Watches must be in place before the edits.
    while e.status()["units"][0]["watched"] != true {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    // Edit an indexed file and create a new one in an indexed directory.
    write(&root, "src/a.rs", b"overlay_token_one\n");
    write(&root, "src/added.rs", b"overlay_token_two\n");
    let t1 = Instant::now();
    loop {
        let r = e
            .search(&SearchOpts {
                pattern: "overlay_token".into(),
                root: root.clone(),
                files_only: true,
                ..Default::default()
            })
            .unwrap();
        if r.files.len() == 2 {
            // Windows also reports the parent directory as modified, which
            // conservatively marks the unit not fresh (the files are still
            // found through the overlay); elsewhere the answer stays fresh.
            if !cfg!(windows) {
                assert!(r.fresh, "known-file changes keep the answer exact");
            }
            // The rebuild is 60 s away, so the answer came from the overlay.
            // (Windows: the watcher can deliver an overflow that forces an
            // earlier rescan; the answer is still exact, which is the point.)
            if !cfg!(windows) {
                assert!(
                    r.units.iter().any(|u| u.dirty),
                    "the rebuild has not happened yet"
                );
            }
            break;
        }
        assert!(
            t1.elapsed() < Duration::from_secs(10),
            "overlay never saw the changes"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let f = e
        .files(&FilesOpts {
            root: root.clone(),
            globs: vec!["added.rs".into()],
            regex: None,
            max_files: 10,
            scan_fallback: false,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(f.files.len(), 1);
    // Secrets created after the build must never be served from the overlay
    // either (at the unit root and below it).
    write(&root, ".env.local", b"overlay_secret_token=1\n");
    write(&root, "server.pem", b"overlay_secret_token\n");
    write(&root, "src/id_rsa", b"overlay_secret_token\n");
    write(&root, "src/visible.rs", b"overlay_secret_token\n");
    let t2 = Instant::now();
    loop {
        let r = e
            .search(&SearchOpts {
                pattern: "overlay_secret_token".into(),
                root: root.clone(),
                files_only: true,
                ..Default::default()
            })
            .unwrap();
        if !r.files.is_empty() {
            assert_eq!(
                rels(&root, &r.files),
                vec!["src/visible.rs"],
                "secrets leaked"
            );
            break;
        }
        assert!(
            t2.elapsed() < Duration::from_secs(10),
            "overlay never saw the new file"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let all = e
        .files(&FilesOpts {
            root: root.clone(),
            globs: vec![],
            regex: None,
            max_files: 1000,
            scan_fallback: false,
            ..Default::default()
        })
        .unwrap();
    let names = rels(&root, &all.files);
    for secret in [".env.local", "server.pem", "src/id_rsa"] {
        assert!(
            !names.iter().any(|n| n == secret),
            "{secret} listed: {names:?}"
        );
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
}

#[test]
fn ignored_file_churn_does_not_dirty_the_unit() {
    let (t, root) = fixture();
    let e = Arc::new(Engine::open(config(t.path(), &root), true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    let t0 = Instant::now();
    while !e
        .search(&SearchOpts {
            pattern: "x".into(),
            root: root.clone(),
            ..Default::default()
        })
        .unwrap()
        .fresh
    {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(50));
    }
    for i in 0..20 {
        write(&root, "debug.log", format!("line {i}\n").as_bytes()); // gitignored (*.log)
        write(&root, "src/state.tmp.db", b"x"); // excluded (*.db)
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(400));
    let st = e.status();
    assert_eq!(st["units_dirty"], 0, "{st}");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
}

#[test]
fn loose_files_directly_in_a_split_root_are_indexed() {
    let t = tempfile::tempdir().unwrap();
    let split = t.path().join("workspaces");
    write(&split, "_port-registry.json", b"{\"loose_marker\": 1}\n");
    write(&split, "alpha/a.txt", b"loose_marker in a unit\n");
    write(&split, "beta/deep/b.txt", b"loose_marker deeper\n");
    write(&split, ".hidden/h.txt", b"loose_marker hidden\n");
    let cfg = Config {
        split_roots: vec![split.to_string_lossy().into_owned()],
        ..config(t.path(), &split)
    };
    let e = Arc::new(Engine::open(cfg, true).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, s2) = (e.clone(), stop.clone());
    let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
    let search = |root: &Path| {
        e.search(&SearchOpts {
            pattern: "loose_marker".into(),
            root: root.to_path_buf(),
            files_only: true,
            scan_fallback: false,
            ..Default::default()
        })
        .unwrap()
    };
    let t0 = Instant::now();
    while !(search(&split).fresh && e.status()["units"].as_array().unwrap().len() == 3) {
        assert!(t0.elapsed() < Duration::from_secs(20), "never fresh");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The loose file and both child units (a hidden child directory is not
    // a unit, as before).
    let r = search(&split);
    assert!(r.covered && r.complete);
    assert_eq!(
        rels(&split, &r.files),
        vec!["_port-registry.json", "alpha/a.txt", "beta/deep/b.txt"]
    );
    let f = e
        .files(&FilesOpts {
            root: split.clone(),
            ..Default::default()
        })
        .unwrap();
    assert!(rels(&split, &f.files).contains(&"_port-registry.json".to_string()));
    // A child unit still answers alone, without the loose files.
    assert_eq!(
        rels(&split, &search(&split.join("alpha")).files),
        vec!["alpha/a.txt"]
    );
    // Inside a non-unit child the index makes no claim (scan fallback territory).
    assert!(!search(&split.join(".hidden")).covered);
    // A new loose file is picked up by the watcher.
    write(&split, "notes.md", b"loose_marker new\n");
    let t1 = Instant::now();
    loop {
        let r = search(&split);
        if r.fresh && rels(&split, &r.files).contains(&"notes.md".to_string()) {
            break;
        }
        assert!(
            t1.elapsed() < Duration::from_secs(20),
            "new loose file never seen"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap();
}
