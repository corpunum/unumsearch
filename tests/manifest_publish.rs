// SPDX-License-Identifier: Apache-2.0
//! State that changes in memory reaches readers within one heartbeat, and an
//! unchanged manifest is never rewritten.

use std::path::Path;
use unumsearch::engine::{Change, Engine};
use unumsearch::Config;

fn cfg(t: &Path, root: &Path) -> Config {
    Config {
        roots: vec![root.to_string_lossy().into_owned()],
        index_dir: Some(t.join("index").to_string_lossy().into_owned()),
        max_file_size: 100_000,
        threads: 2,
        debounce_ms: 100,
        rescan_secs: 30,
        watch: false,
        ..Config::default()
    }
}

#[test]
fn heartbeat_publishes_changed_state_only() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    let w = Engine::open(cfg(t.path(), &root), true).unwrap();
    w.index_all(false);
    let manifest = t.path().join("index").join("manifest.json");
    let r = Engine::open(cfg(t.path(), &root), false).unwrap();
    r.refresh();
    assert_eq!(r.status()["units_dirty"], 0);

    // 1. Nothing changed: a beat leaves the manifest alone (the first beat
    //    may save once, to learn the baseline).
    w.heartbeat();
    let base = std::fs::read(&manifest).unwrap();
    w.heartbeat();
    assert_eq!(std::fs::read(&manifest).unwrap(), base);

    // 2. A change in memory that nothing saves: the beat publishes it.
    let unit = root.to_string_lossy().into_owned();
    w.mark_dirty(&unit, Change::Unknown);
    assert_eq!(std::fs::read(&manifest).unwrap(), base, "mark_dirty saved");
    std::thread::sleep(std::time::Duration::from_millis(5));
    w.heartbeat();
    let after = std::fs::read(&manifest).unwrap();
    assert_ne!(after, base, "beat did not publish the change");
    r.refresh();
    assert_eq!(r.status()["units_dirty"], 1);

    // 3. Nothing further changed: identical bytes again.
    w.heartbeat();
    assert_eq!(std::fs::read(&manifest).unwrap(), after);
}
