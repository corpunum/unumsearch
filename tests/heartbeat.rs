// SPDX-License-Identifier: Apache-2.0
//! The writer's heartbeat lives in its own small file: beating must not
//! rewrite the manifest, and readers must still see the writer alive.

use std::path::Path;
use unumsearch::engine::Engine;
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
fn heartbeat_keeps_the_manifest_and_readers_see_it() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    let w = Engine::open(cfg(t.path(), &root), true).unwrap();
    assert!(w.is_writer());
    // Before the roots were looked at there are no units: that is not fresh
    // (a daemon answers /status from its first moment).
    assert_eq!(w.status()["fresh"], false);
    w.index_all(false);
    assert_eq!(w.status()["units_total"], 1);
    let idx = t.path().join("index");
    let manifest = idx.join("manifest.json");

    // Age the manifest's own beat beyond the liveness window, as if the
    // writer had stopped long ago, and drop any heartbeat file.
    let mut m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    m["heartbeat_ms"] = serde_json::json!(1u64);
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let _ = std::fs::remove_file(idx.join("heartbeat"));
    let r = Engine::open(cfg(t.path(), &root), false).unwrap();
    r.refresh();
    assert_eq!(r.status()["writer_alive"], false);

    let before = std::fs::read(&manifest).unwrap();
    w.heartbeat();
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        before,
        "beat rewrote the manifest"
    );
    assert!(idx.join("heartbeat").exists());
    r.refresh();
    assert_eq!(r.status()["writer_alive"], true);
    assert!(r.status()["heartbeat_ms"].as_u64().unwrap() > 1);

    // A checkpoint also refreshes the manifest's beat, for older readers.
    w.checkpoint();
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert!(m["heartbeat_ms"].as_u64().unwrap() > 1);
}
