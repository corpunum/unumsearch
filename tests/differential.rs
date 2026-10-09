// SPDX-License-Identifier: Apache-2.0
//! Randomised differential test against ripgrep.
//!
//! Generates small random corpora (nested directories, .gitignore rules,
//! hidden files, binaries, oversized files, secrets, ASCII, Greek and other
//! Unicode text), indexes them, and checks that for many random patterns the
//! file set equals `rg -l` over the same corpus definition. A second phase
//! mutates the tree while the watcher runs (create, edit, delete, rename
//! files and directories, edit ignore files, touch ignored files, create
//! symlinks) and checks that every answer which claims `fresh` is exact and
//! that the index always becomes fresh again.
//!
//! Skipped when `rg` is not installed. Knobs: `UNUMSEARCH_DIFF_CASES`
//! (static corpora, default 150), `UNUMSEARCH_DIFF_MUTATIONS` (watcher cases,
//! default 6), `UNUMSEARCH_DIFF_SEED` (reproduce one failure).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::engine::{Engine, SearchOpts};
use unumsearch::Config;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "gamma",
    "handle_request",
    "HandleRequest",
    "parse_config",
    "TODO",
    "FIXME",
    "foo.bar",
    "foo_bar",
    "fooXbar",
    "hello",
    "Hello",
    "WORLD",
    "world",
    "update",
    "available",
    "updateAvailable",
    "καλημέρα",
    "Καλημέρα",
    "ΚΑΛΗΜΕΡΑ",
    "αβγ",
    "ΑΒΓ",
    "άλφα",
    "Άλφα",
    "ΣΊΣΥΦΟΣ",
    "σίσυφος",
    "straße",
    "STRASSE",
    "naïve",
    "日本語",
    "Ünïcödé",
    "ǅ",
    "x",
    "ab",
    "a1b2",
    "end;",
];
const DIRS: &[&str] = &[
    "src",
    "lib",
    "docs",
    "a/b",
    "a/b/c",
    "build",
    "vendor",
    ".hidden",
    "tests",
    "ignored",
    "node_modules/p",
];
const NAMES: &[&str] = &[
    "main.rs",
    "a.txt",
    "b.md",
    "c.js",
    "notes",
    "x.log",
    "data.json",
    ".env",
    "id_rsa",
    "k.pem",
    "secrets.json",
    "Ünï.txt",
    "καλη.txt",
];
const PATTERNS: &[(&str, bool, bool)] = &[
    ("hello", false, false),
    ("hello", false, true),
    ("handle_request", false, true),
    ("foo.bar", false, false),
    ("foo.bar", true, false),
    (r"foo.bar", true, true),
    (r"(alpha|beta)\w*", true, false),
    (r"TODO|FIXME", true, false),
    (r"upd\w+le", true, true),
    ("καλημέρα", false, false),
    ("καλημέρα", false, true),
    ("ΚΑΛΗΜΕΡΑ", false, true),
    ("αβγ", false, true),
    ("σίσυφος", false, true),
    ("ΣΊΣΥΦΟΣ", false, true),
    (r"[α-ω]+", true, false),
    ("straße", false, true),
    ("日本語", false, false),
    ("ünïcödé", false, true),
    (r"a\d+b\d+", true, false),
    (r"^end;$", true, false),
    ("x", false, false),
    ("zzzz_never", false, false),
    (r"\bworld\b", true, true),
];

fn random_text(r: &mut Rng) -> String {
    let n = 1 + r.below(30);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(r.pick(WORDS));
        s.push(if r.chance(20) { '\n' } else { ' ' });
    }
    if r.chance(10) {
        s.push_str("\r\n");
    }
    s
}

fn write_random_file(root: &Path, rel: &str, r: &mut Rng, max: usize) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let body: Vec<u8> = match r.below(20) {
        0 => b"alpha\0binary hello\n".to_vec(),
        1 => {
            let mut v = random_text(r).into_bytes();
            v.resize(max + 1 + r.below(100), b' ');
            v
        }
        2 => Vec::new(),
        _ => random_text(r).into_bytes(),
    };
    std::fs::write(p, body).unwrap();
}

fn gitignore_text(r: &mut Rng) -> String {
    let mut s = String::new();
    for pat in [
        "ignored/",
        "*.log",
        "build/",
        "docs/*.md",
        "!docs/keep.md",
        "c.js",
        "/notes",
        "a/b/c/",
    ] {
        if r.chance(35) {
            s.push_str(pat);
            s.push('\n');
        }
    }
    s
}

fn build_corpus(root: &Path, r: &mut Rng, max: usize) {
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".gitignore"), gitignore_text(r)).unwrap();
    for _ in 0..(6 + r.below(30)) {
        let rel = format!("{}/{}", r.pick(DIRS), r.pick(NAMES));
        write_random_file(root, &rel, r, max);
    }
    if r.chance(50) {
        write_random_file(root, "docs/keep.md", r, max);
    }
    if r.chance(30) {
        std::fs::write(root.join("a/.gitignore"), "*.txt\n").ok();
    }
}

struct Env {
    rg: String,
    root: PathBuf,
    ignore_file: PathBuf,
    max: u64,
}

fn rg_set(env: &Env, pat: &str, regex: bool, ci: bool) -> Vec<String> {
    let mut cmd = Command::new(&env.rg);
    cmd.current_dir(&env.root)
        .args([
            "-l",
            "--hidden",
            "--max-filesize",
            &env.max.to_string(),
            "--ignore-file",
        ])
        .arg(&env.ignore_file);
    if !regex {
        cmd.arg("-F");
    }
    if ci {
        cmd.arg("-i");
    }
    let out = cmd.arg("-e").arg(pat).arg(".").output().unwrap();
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.replace('\\', "/").trim_start_matches("./").to_string())
        .collect();
    v.sort();
    v
}

fn engine_answer(e: &Engine, env: &Env, pat: &str, regex: bool, ci: bool) -> (Vec<String>, bool) {
    let r = e
        .search(&SearchOpts {
            pattern: pat.into(),
            root: env.root.clone(),
            regex,
            case_insensitive: ci,
            files_only: true,
            max_files: 1_000_000,
            ..Default::default()
        })
        .unwrap();
    let mut v: Vec<String> = r
        .files
        .iter()
        .map(|f| {
            Path::new(f)
                .strip_prefix(&env.root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    v.sort();
    (v, r.fresh)
}

fn rg_binary() -> Option<String> {
    let rg = std::env::var("UNUMSEARCH_TEST_RG").unwrap_or_else(|_| "rg".into());
    Command::new(&rg).arg("--version").output().ok().map(|_| rg)
}

fn cfg(t: &Path, root: &Path, max: u64, watch: bool) -> Config {
    Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.join("index").to_string_lossy().into_owned()),
        max_file_size: max,
        threads: 2,
        debounce_ms: 50,
        max_wait_ms: 500,
        rescan_secs: 3600,
        watch,
        ..Config::default()
    }
}

fn seed_base() -> u64 {
    std::env::var("UNUMSEARCH_DIFF_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5eed_u64)
}

#[test]
fn static_corpora_match_ripgrep() {
    let Some(rg) = rg_binary() else {
        eprintln!("rg not found; skipping differential test");
        return;
    };
    let cases: u64 = std::env::var("UNUMSEARCH_DIFF_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let single = std::env::var("UNUMSEARCH_DIFF_SEED").is_ok();
    let mut compared = 0usize;
    for case in 0..if single { 1 } else { cases } {
        let seed = if single {
            seed_base()
        } else {
            seed_base() + case
        };
        let mut r = Rng(seed);
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("repo");
        let max = 400u64;
        build_corpus(&root, &mut r, max as usize);
        let c = cfg(t.path(), &root, max, false);
        let ignore_file = t.path().join("excludes.ignore");
        std::fs::write(&ignore_file, c.exclude_lines().join("\n")).unwrap();
        let env = Env {
            rg: rg.clone(),
            root: root.clone(),
            ignore_file,
            max,
        };
        let e = Engine::open(c, true).unwrap();
        e.index_all(false);
        for (pat, regex, ci) in PATTERNS {
            let want = rg_set(&env, pat, *regex, *ci);
            let (got, fresh) = engine_answer(&e, &env, pat, *regex, *ci);
            assert!(fresh, "seed {seed}: fresh index expected");
            assert_eq!(
                got, want,
                "seed {seed} (UNUMSEARCH_DIFF_SEED={seed}) pattern {pat:?} regex={regex} ci={ci}"
            );
            compared += 1;
        }
    }
    eprintln!("differential: {compared} comparisons against rg");
}

/// Inotify delivers events in order, so once a sentinel file written after
/// the mutations is visible to the engine, every earlier event has been
/// processed. Without this barrier a query made microseconds after a write
/// races the delivery of that write's event, which no watcher can avoid.
fn barrier(e: &Engine, env: &Env, n: usize) {
    let token = format!("sentinel_token_{n}_{}", std::process::id());
    std::fs::write(env.root.join(format!(".sentinel{n}.txt")), &token).unwrap();
    let t0 = Instant::now();
    loop {
        let r = e
            .search(&SearchOpts {
                pattern: token.clone(),
                root: env.root.clone(),
                files_only: true,
                ..Default::default()
            })
            .unwrap();
        if !r.files.is_empty() {
            return;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "sentinel never seen"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn mutate(root: &Path, r: &mut Rng, max: usize) {
    let op = r.below(9);
    if std::env::var_os("UNUMSEARCH_DEBUG_EVENTS").is_some() {
        eprintln!("mutate op {op}");
    }
    match op {
        0 => {
            let rel = format!("{}/new{}.txt", r.pick(DIRS), r.below(5));
            write_random_file(root, &rel, r, max);
        }
        1 => {
            let rel = format!("{}/{}", r.pick(DIRS), r.pick(NAMES));
            write_random_file(root, &rel, r, max);
        }
        2 => {
            let rel = format!("{}/{}", r.pick(DIRS), r.pick(NAMES));
            let _ = std::fs::remove_file(root.join(rel));
        }
        3 => {
            let a = format!("{}/{}", r.pick(DIRS), r.pick(NAMES));
            let b = format!("{}/renamed{}.txt", r.pick(DIRS), r.below(4));
            if let Some(p) = root.join(&b).parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::rename(root.join(a), root.join(b));
        }
        4 => {
            let a = r.pick(&["src", "lib", "docs", "tests", "a"]);
            let _ = std::fs::rename(root.join(a), root.join(format!("{a}_moved")));
        }
        5 => {
            std::fs::write(root.join(".gitignore"), gitignore_text(r)).unwrap();
        }
        6 => {
            // churn in an ignored location
            let _ = std::fs::create_dir_all(root.join("build"));
            std::fs::write(root.join("build/out.txt"), random_text(r)).unwrap();
        }
        7 => {
            // a file grows beyond the size cap, or shrinks back
            let rel = format!("{}/{}", r.pick(DIRS), r.pick(NAMES));
            let p = root.join(rel);
            if p.is_file() {
                let mut v = std::fs::read(&p).unwrap_or_default();
                v.resize(max + 50, b'q');
                let _ = std::fs::write(p, v);
            }
        }
        _ => {
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(
                    root.join("src"),
                    root.join(format!("lnk{}", r.below(3))),
                );
            }
            #[cfg(not(unix))]
            write_random_file(root, "src/extra.txt", r, max);
        }
    }
}

#[test]
fn mutating_corpora_with_a_watcher_stay_exact_and_become_fresh() {
    let Some(rg) = rg_binary() else {
        eprintln!("rg not found; skipping differential test");
        return;
    };
    let cases: u64 = std::env::var("UNUMSEARCH_DIFF_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let mut checked_fresh = 0usize;
    for case in 0..cases {
        checked_fresh += mutation_case(&rg, seed_base() + 1_000_000 + case, 0);
    }
    eprintln!("differential(watch): {checked_fresh} fresh answers verified");
}

/// Issue #6: on macOS CI, seed 1024301 got a `fresh:true` answer missing a
/// newly created file. Every rebuild there pays an `F_FULLFSYNC` per shard,
/// which kept a rebuild running while the test queried, and a running
/// rebuild used to hide the pending overlay. Replays that seed with every
/// rebuild stretched by 300 ms, querying throughout, on any platform.
#[test]
fn seed_1024301_with_slow_rebuilds_stays_exact() {
    let Some(rg) = rg_binary() else {
        eprintln!("rg not found; skipping differential test");
        return;
    };
    let n = mutation_case(&rg, 1_024_301, 300);
    eprintln!("differential(watch, slow rebuilds): {n} fresh answers verified");
}

/// One watcher case: mutate a random corpus while the watcher runs, check
/// every fresh answer against rg, then that it converges. Returns how many
/// fresh answers were verified.
fn mutation_case(rg: &str, seed: u64, build_stall_ms: u64) -> usize {
    let mut checked_fresh = 0usize;
    {
        let mut r = Rng(seed);
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("repo");
        let max = 400u64;
        build_corpus(&root, &mut r, max as usize);
        let c = cfg(t.path(), &root, max, true);
        let ignore_file = t.path().join("excludes.ignore");
        std::fs::write(&ignore_file, c.exclude_lines().join("\n")).unwrap();
        let env = Env {
            rg: rg.to_string(),
            root: root.clone(),
            ignore_file,
            max,
        };
        let e = Arc::new(Engine::open(c, true).unwrap());
        e.set_build_stall_ms(build_stall_ms);
        let stop = Arc::new(AtomicBool::new(false));
        let (e2, s2) = (e.clone(), stop.clone());
        let h = std::thread::spawn(move || unumsearch::watch::run(e2, s2));
        let t0 = Instant::now();
        while !engine_answer(&e, &env, "x", false, false).1 {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "seed {seed}: never fresh"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        for round in 0..6 {
            for _ in 0..(1 + r.below(4)) {
                mutate(&root, &mut r, max as usize);
            }
            barrier(&e, &env, round);
            // While the answer claims to be fresh it must be exact.
            let mut check = |pat: &str, regex: bool, ci: bool| {
                let (got, fresh) = engine_answer(&e, &env, pat, regex, ci);
                if fresh {
                    // Re-ask ripgrep after the engine: a change landing in
                    // between would show as a difference in both directions,
                    // so confirm with a second engine answer.
                    let want = rg_set(&env, pat, regex, ci);
                    if got != want {
                        let (again, fresh2) = engine_answer(&e, &env, pat, regex, ci);
                        let want2 = rg_set(&env, pat, regex, ci);
                        assert!(
                            !(fresh2 && again != want2),
                            "seed {seed} round {round} pattern {pat:?} regex={regex} ci={ci}: fresh answer differs from rg\n engine: {again:?}\n rg:     {want2:?}\n status: {}",
                            e.status()["units"]
                        );
                    }
                    checked_fresh += 1;
                }
            };
            if build_stall_ms > 0 {
                // Slow-rebuild mode: keep asking through the next drain cycle
                // (up to 200 ms) and the stalled rebuild that follows it, so
                // queries certainly land inside the rebuild window. Patterns
                // cycle in order: the RNG stream (and so the case) is the
                // same as without the stall.
                let until = Instant::now() + Duration::from_millis(250 + build_stall_ms);
                let mut i = 0;
                while Instant::now() < until {
                    let (pat, regex, ci) = PATTERNS[i % PATTERNS.len()];
                    check(pat, regex, ci);
                    i += 1;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            for _ in 0..4 {
                let (pat, regex, ci) = *r.pick(PATTERNS);
                check(pat, regex, ci);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Quiesce: the index must become fresh and then equal rg for all patterns.
        let t1 = Instant::now();
        loop {
            let ok = PATTERNS.iter().all(|(pat, regex, ci)| {
                let (got, fresh) = engine_answer(&e, &env, pat, *regex, *ci);
                fresh && got == rg_set(&env, pat, *regex, *ci)
            });
            if ok {
                break;
            }
            assert!(
                t1.elapsed() < Duration::from_secs(40),
                "seed {seed}: index never converged to ripgrep's answer"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        stop.store(true, Ordering::Relaxed);
        let _ = h.join();
    }
    checked_fresh
}
