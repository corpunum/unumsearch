// SPDX-License-Identifier: Apache-2.0
//! v0.1.5: `covered` is false for a root the index leaves out. A search root
//! inside a directory the unit's corpus excludes (ignore files, excludes,
//! secrets, hidden-file rules, the size cap, symlinks) or a split root's
//! non-unit child directories must not be answered as a covered, empty
//! result: the answer says `covered: false` and lists the `uncovered` paths.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use unumsearch::api::{self, Scope};
use unumsearch::engine::{Engine, FilesOpts, SearchOpts, SearchResult};
use unumsearch::Config;

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn cfg(t: &Path, roots: &[&Path]) -> Config {
    Config {
        roots: roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect(),
        index_dir: Some(t.join("index").to_string_lossy().into_owned()),
        max_file_size: 1000,
        threads: 2,
        rescan_secs: 3600,
        watch: false,
        ..Config::default()
    }
}

/// A git checkout with one indexed file and one excluded directory of each
/// kind, every one holding `needle`.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root, ".gitignore", "/vendored/\nbuilds/**\n");
    write(&root, "src/a.rs", "fn needle() {}\n");
    write(&root, "src/empty/.keep", "");
    write(&root, "vendored/kernel/drivers/x.c", "int needle;\n");
    // A nested checkout (its own repository) the outer one ignores.
    std::fs::create_dir_all(root.join("builds/one/.git")).unwrap();
    write(&root, "builds/one/y.c", "int needle;\n");
    write(&root, "datasets/d.jsonl", "{\"needle\":1}\n");
    write(&root, "node_modules/pkg/i.js", "needle()\n");
    write(&root, ".hidden/h.txt", "needle\n");
    write(&root, ".config/app/c.txt", "needle\n");
    write(&root, "big/large.txt", &"needle ".repeat(400));
    (t, root)
}

fn engine(t: &Path, root: &Path, f: impl FnOnce(&mut Config)) -> Engine {
    let mut c = cfg(t, &[root]);
    c.excludes = vec!["datasets/".into()];
    f(&mut c);
    let e = Engine::open(c, true).unwrap();
    e.index_all(false);
    e
}

fn search(e: &Engine, root: &Path, scan: bool) -> SearchResult {
    e.search(&SearchOpts {
        pattern: "needle".into(),
        root: root.to_path_buf(),
        files_only: true,
        scan_fallback: scan,
        ..Default::default()
    })
    .unwrap()
}

fn assert_uncovered(r: &SearchResult, root: &Path, reason: &str) {
    assert!(!r.covered && !r.complete, "{r:?}");
    assert_eq!(r.uncovered.len(), 1, "{r:?}");
    // The answer names the root as the engine normalised it ('/' becomes
    // '\\' on Windows).
    let want = unumsearch::config::normalize(root);
    assert_eq!(r.uncovered[0].path, want.to_string_lossy(), "{r:?}");
    assert_eq!(r.uncovered[0].reason, reason, "{r:?}");
}

/// Every exclusion kind: not covered, never a covered empty answer; with
/// the scan fallback the root is scanned and its files found (rg's answer).
#[test]
fn roots_under_each_exclusion_kind_are_not_covered() {
    let (t, root) = repo();
    let e = engine(t.path(), &root, |_| {});
    let cases = [
        ("vendored/kernel", "gitignore (anchored dir)"),
        ("vendored/kernel/drivers", "gitignore, deeper"),
        ("builds/one", "gitignore (dir/** pattern)"),
        ("datasets", "config exclude"),
        ("node_modules/pkg", "default exclude"),
    ];
    for (sub, kind) in cases {
        let p = root.join(sub);
        let r = search(&e, &p, false);
        assert_uncovered(&r, &p, "excluded");
        assert_eq!(r.backend, "none", "{kind}");
        assert!(r.files.is_empty(), "{kind}");
        let r = search(&e, &p, true);
        assert_uncovered(&r, &p, "excluded");
        assert_eq!(r.backend, "scan", "{kind}");
        assert_eq!(r.files.len(), 1, "{kind}: the scan finds it: {r:?}");
        assert!(r.fresh && !r.complete, "{kind}");
        let f = e
            .files(&FilesOpts {
                root: p.clone(),
                scan_fallback: false,
                ..Default::default()
            })
            .unwrap();
        assert!(!f.covered && !f.complete, "{kind}: {f:?}");
        assert_eq!(f.uncovered[0].reason, "excluded", "{kind}");
    }
}

#[test]
fn hidden_directories_are_not_covered_when_hidden_files_are_off() {
    let (t, root) = repo();
    let e = engine(t.path(), &root, |c| c.hidden = false);
    let p = root.join(".hidden");
    assert_uncovered(&search(&e, &p, false), &p, "excluded");
    // With hidden files on (the default) the same directory is indexed.
    let (t2, root2) = repo();
    let e2 = engine(t2.path(), &root2, |_| {});
    let r = search(&e2, &root2.join(".hidden"), false);
    assert!(r.covered && r.complete && r.uncovered.is_empty(), "{r:?}");
    assert_eq!(r.files.len(), 1);
}

#[test]
fn an_over_size_file_root_is_not_covered() {
    let (t, root) = repo();
    let e = engine(t.path(), &root, |_| {});
    let p = root.join("big/large.txt");
    assert_uncovered(&search(&e, &p, false), &p, "excluded");
    // Its directory is covered: the corpus leaves the file out by design.
    let r = search(&e, &root.join("big"), false);
    assert!(r.covered && r.files.is_empty(), "{r:?}");
}

#[cfg(unix)]
#[test]
fn a_root_through_a_symlinked_directory_is_not_covered() {
    let (t, root) = repo();
    let elsewhere = t.path().join("elsewhere");
    write(&elsewhere, "z.txt", "needle\n");
    std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
    let e = engine(t.path(), &root, |_| {});
    let p = root.join("link");
    let r = search(&e, &p, true);
    assert_uncovered(&r, &p, "excluded");
    assert_eq!(r.files.len(), 1, "{r:?}");
}

/// Secrets: reported as not covered, with the reason that tells a caller not
/// to scan; never scanned, and refused outright by every front-end.
#[test]
fn secret_locations_are_not_covered_and_never_scanned() {
    let (t, root) = repo();
    let e = engine(t.path(), &root, |_| {});
    for sub in [".config", ".config/app"] {
        let p = root.join(sub);
        for scan in [false, true] {
            let r = search(&e, &p, scan);
            assert_uncovered(&r, &p, "secret");
            assert_eq!(r.backend, "none");
            assert!(r.files.is_empty());
        }
        let f = e
            .files(&FilesOpts {
                root: p.clone(),
                ..Default::default()
            })
            .unwrap();
        assert!(!f.covered && f.files.is_empty() && f.uncovered[0].reason == "secret");
        for scope in [Scope::Local, Scope::Confined] {
            let v = api::call_scoped(
                &e,
                "search",
                &json!({"pattern": "needle", "root": p.to_string_lossy()}),
                scope,
            );
            assert!(v.is_err(), "{v:?}");
        }
    }
}

/// What was covered before stays covered, with the same answer.
#[test]
fn covered_roots_are_unchanged() {
    let (t, root) = repo();
    let e = engine(t.path(), &root, |_| {});
    for (sub, n) in [("", 2), ("src", 1), ("src/a.rs", 1), ("src/empty", 0)] {
        let p = if sub.is_empty() {
            root.clone()
        } else {
            root.join(sub)
        };
        let r = search(&e, &p, true);
        assert!(r.covered && r.complete, "{sub}: {r:?}");
        assert_eq!(r.backend, "index", "{sub}");
        assert!(r.uncovered.is_empty(), "{sub}");
        assert_eq!(r.files.len(), n, "{sub}: {:?}", r.files);
    }
    // A path that does not exist: nothing to search, the empty answer is exact.
    let r = search(&e, &root.join("src/missing"), true);
    assert!(r.covered && r.files.is_empty() && r.backend == "index");
}

/// The decision is remembered per unit build: after an ignore-file change
/// and a rebuild, a newly included directory is covered (and found).
#[test]
fn coverage_follows_ignore_file_changes() {
    let (t, root) = repo();
    let e = engine(t.path(), &root, |_| {});
    let p = root.join("vendored/kernel");
    assert!(!search(&e, &p, false).covered);
    assert!(!search(&e, &p, false).covered, "cached");
    write(&root, ".gitignore", "builds/**\n");
    e.index_all(false);
    let r = search(&e, &p, false);
    assert!(r.covered && r.files.len() == 1, "{r:?}");
    write(&root, ".gitignore", "/vendored/\nbuilds/**\n");
    e.index_all(false);
    assert!(!search(&e, &p, false).covered);
}

/// A split root covers its child units; a child directory that is not a
/// unit (a hidden name) is listed as uncovered.
#[test]
fn split_root_lists_non_unit_children() {
    let t = tempfile::tempdir().unwrap();
    let split = t.path().join("checkouts");
    write(&split, "one/a.txt", "needle\n");
    write(&split, ".scratch/b.txt", "needle\n");
    write(&split, ".ssh/k.txt", "needle\n");
    let mut c = cfg(t.path(), &[&split]);
    c.split_roots = vec![split.to_string_lossy().into_owned()];
    let e = Engine::open(c, true).unwrap();
    e.index_all(false);
    let r = search(&e, &split, true);
    assert!(!r.covered && !r.complete, "{r:?}");
    assert_eq!(r.backend, "index");
    assert_eq!(r.files.len(), 1);
    // `.ssh` is excluded by design: part of no corpus, so not "uncovered".
    assert_eq!(r.uncovered.len(), 1, "{r:?}");
    assert_eq!(
        r.uncovered[0].path,
        split.join(".scratch").to_string_lossy()
    );
    assert_eq!(r.uncovered[0].reason, "not_indexed");
    // A child unit by itself is fully covered.
    let r = search(&e, &split.join("one"), true);
    assert!(r.covered && r.uncovered.is_empty());
    // all_roots carries it through.
    let v = api::call(
        &e,
        "search",
        &json!({"pattern": "needle", "all_roots": true}),
    )
    .unwrap();
    assert_eq!(v["result"]["covered"], false);
    assert_eq!(v["result"]["uncovered"][0]["reason"], "not_indexed");
}

/// The same through the shared API (RPC and MCP use it; HTTP adds the
/// confinement): search, files, lookup, and all_roots.
#[test]
fn api_front_ends_report_uncovered() {
    let (t, root) = repo();
    let other = t.path().join("other");
    write(&other, "o.txt", "needle\n");
    let mut c = cfg(t.path(), &[&root, &other]);
    c.excludes = vec!["datasets/".into()];
    let e = Engine::open(c, true).unwrap();
    e.index_all(false);
    let p = root.join("datasets").to_string_lossy().into_owned();
    for scope in [Scope::Local, Scope::Confined] {
        let v = api::call_scoped(
            &e,
            "search",
            &json!({"pattern": "needle", "root": p, "scan_fallback": false}),
            scope,
        )
        .unwrap();
        let r = &v["result"];
        assert_eq!(r["covered"], false);
        assert_eq!(r["complete"], false);
        assert_eq!(r["uncovered"], json!([{"path": p, "reason": "excluded"}]));
        let v = api::call_scoped(&e, "files", &json!({"root": p}), scope).unwrap();
        assert_eq!(v["result"]["covered"], false);
        assert_eq!(v["result"]["backend"], "scan");
        let v = api::call_scoped(
            &e,
            "lookup",
            &json!({"patterns": ["needle"], "root": p}),
            scope,
        )
        .unwrap();
        assert_eq!(v["result"]["covered"], false);
        assert_eq!(v["result"]["results"][0]["complete"], false);
        assert_eq!(v["result"]["uncovered"][0]["reason"], "excluded");
    }
    // Covered answers carry no `uncovered` key at all.
    let v = api::call(
        &e,
        "search",
        &json!({"pattern": "needle", "all_roots": true}),
    )
    .unwrap();
    let r: &Value = &v["result"];
    assert_eq!(r["covered"], true, "{r}");
    assert!(r.get("uncovered").is_none(), "{r}");
    assert_eq!(r["files"].as_array().unwrap().len(), 3, "{r}");
}

#[test]
fn cli_reports_uncovered() {
    let (t, root) = repo();
    let idx = t.path().join("idx");
    std::fs::write(t.path().join("c.toml"), "excludes = [\"datasets/\"]\n").unwrap();
    let run = |args: &[&str]| -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_unumsearch"))
            .env_remove("UNUMSEARCH_ROOTS")
            .arg("--config")
            .arg(t.path().join("c.toml"))
            .arg("--root")
            .arg(&root)
            .arg("--index-dir")
            .arg(&idx)
            .args(args)
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    run(&["index"]);
    let p = root.join("datasets");
    let ps = p.to_string_lossy();
    let v = run(&["search", "-l", "--no-scan", "needle", &ps]);
    assert_eq!(v["result"]["covered"], false, "{v}");
    assert_eq!(v["result"]["uncovered"][0]["reason"], "excluded", "{v}");
    let v = run(&["search", "-l", "needle", &ps]);
    assert_eq!(v["result"]["backend"], "scan", "{v}");
    assert_eq!(v["result"]["files"].as_array().unwrap().len(), 1, "{v}");
    let v = run(&[
        "search",
        "-l",
        "needle",
        &root.join("src").to_string_lossy(),
    ]);
    assert_eq!(v["result"]["covered"], true, "{v}");
    assert!(v["result"].get("uncovered").is_none(), "{v}");
}
