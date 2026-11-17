// SPDX-License-Identifier: Apache-2.0
//! `no_gitignore_roots`: an allowlist-style .gitignore hides a tree unless the
//! root opts out; excludes still apply under the opt-out.

use std::path::Path;
use unumsearch::{walk, Config};

fn write(root: &Path, rel: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, "hello").unwrap();
}

fn rels(cfg: &Config, root: &Path) -> Vec<String> {
    let mut v: Vec<String> = walk::list(cfg, root).into_iter().map(|f| f.rel).collect();
    v.sort();
    v
}

#[test]
fn opt_out_root_ignores_allowlist_gitignore_but_keeps_excludes() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("briefs");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".gitignore"), "/*\n!/a.md\n").unwrap();
    write(&root, "a.md");
    write(&root, "sub/b.md");
    write(&root, "sub/state/secret.md");

    let mut cfg = Config {
        excludes: vec!["state/".into()],
        ..Config::default()
    };
    let before = rels(&cfg, &root);
    assert!(before.contains(&"a.md".to_string()));
    assert!(!before.contains(&"sub/b.md".to_string()));

    cfg.no_gitignore_roots = vec![root.to_string_lossy().into_owned()];
    let after = rels(&cfg, &root);
    assert!(after.contains(&"sub/b.md".to_string()), "{after:?}");
    assert!(after.iter().all(|r| !r.contains("state")), "{after:?}");
}
