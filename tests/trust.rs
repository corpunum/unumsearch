// SPDX-License-Identifier: Apache-2.0
//! v0.1.3 "Trustworthy Search": root authorisation and secret locations,
//! the hardened HTTP API, shard generations, `files()` limits, overlapping
//! roots and cross-process freshness.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unumsearch::api::{self, Scope};
use unumsearch::engine::{Change, Engine, FilesOpts, SearchOpts};
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
        max_file_size: 100_000,
        threads: 2,
        debounce_ms: 100,
        rescan_secs: 3600,
        watch: false,
        ..Config::default()
    }
}

fn repo() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root, "src/a.rs", "fn visible_token() {}\n");
    write(&root, "src/b.rs", "fn other() {}\n");
    (t, root)
}

// ---------------------------------------------------------------- finding 1

#[test]
fn confined_roots_must_be_inside_configured_roots() {
    let (t, root) = repo();
    let outside = t.path().join("outside");
    write(&outside, "x.txt", "visible_token outside\n");
    let e = Engine::open(cfg(t.path(), &[&root]), true).unwrap();
    e.index_all(false);
    let s = |root: &Path| {
        api::call_scoped(
            &e,
            "search",
            &json!({"pattern": "visible_token", "root": root.to_string_lossy()}),
            Scope::Confined,
        )
    };
    // Inside: fine. Outside, `..` traversal, nonexistent: denied.
    assert!(s(&root).is_ok());
    assert!(s(&root.join("src")).is_ok());
    assert!(s(&outside).is_err());
    assert!(s(&root.join("../outside")).is_err());
    assert!(s(&root.join("src/../../outside")).is_err());
    assert!(s(&root.join("missing")).is_err());
    assert!(s(t.path()).is_err());
    assert!(s(Path::new("/")).is_err());
    // Local callers (CLI, stdio, MCP) may still scan other directories.
    let local = api::call(
        &e,
        "search",
        &json!({"pattern": "visible_token", "root": outside.to_string_lossy()}),
    )
    .unwrap();
    assert_eq!(local["result"]["backend"], "scan");
    assert_eq!(local["result"]["files"].as_array().unwrap().len(), 1);
    // files() and lookup() are confined too.
    for method in ["files", "lookup"] {
        let r = api::call_scoped(
            &e,
            method,
            &json!({"root": outside.to_string_lossy(), "patterns": ["x"]}),
            Scope::Confined,
        );
        assert!(r.is_err(), "{method}");
    }
}

#[cfg(unix)]
#[test]
fn confined_symlink_escape_is_denied() {
    let (t, root) = repo();
    let outside = t.path().join("outside");
    write(&outside, "x.txt", "visible_token outside\n");
    std::os::unix::fs::symlink(&outside, root.join("src/escape")).unwrap();
    let e = Engine::open(cfg(t.path(), &[&root]), true).unwrap();
    e.index_all(false);
    let ask = |r: PathBuf| {
        api::call_scoped(
            &e,
            "search",
            &json!({"pattern": "visible_token", "root": r.to_string_lossy()}),
            Scope::Confined,
        )
    };
    assert!(ask(root.join("src/escape")).is_err());
    // `link/..` resolves to the outside directory's parent, not to `src`.
    assert!(ask(root.join("src/escape/..")).is_err());
    assert!(ask(root.join("src")).is_ok());
}

#[test]
fn secret_locations_are_refused_wherever_the_walk_starts() {
    let (t, root) = repo();
    write(&root, ".ssh/notes.txt", "visible_token in ssh\n");
    write(&root, ".config/app/c.txt", "visible_token in config\n");
    write(&root, ".aws/credentials.txt", "visible_token aws\n");
    let e = Engine::open(cfg(t.path(), &[&root]), true).unwrap();
    e.index_all(false);
    for sub in [".ssh", ".config", ".config/app", ".aws"] {
        let p = root.join(sub);
        for scope in [Scope::Local, Scope::Confined] {
            for method in ["search", "files"] {
                let r = api::call_scoped(
                    &e,
                    method,
                    &json!({"pattern": "visible_token", "root": p.to_string_lossy()}),
                    scope,
                );
                assert!(r.is_err(), "{method} {sub} {scope:?}: {r:?}");
            }
        }
    }
    // The same through the library API: no scan inside a secret directory,
    // even though it was never indexed.
    let loose = t.path().join("loose/.ssh");
    write(&loose, "n.txt", "visible_token loose\n");
    let r = e
        .search(&SearchOpts {
            pattern: "visible_token".into(),
            root: loose.clone(),
            ..Default::default()
        })
        .unwrap();
    assert!(r.files.is_empty() && r.backend == "none", "{:?}", r.files);
    let r = e
        .files(&FilesOpts {
            root: loose.clone(),
            ..Default::default()
        })
        .unwrap();
    assert!(r.files.is_empty());
    // An ordinary search of the whole repo never returns them either.
    let r = e
        .search(&SearchOpts {
            pattern: "visible_token".into(),
            root: root.clone(),
            files_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.files.len(), 1);
}

#[test]
fn a_configured_root_below_a_secret_looking_directory_still_works() {
    // Exclusions apply below the configured root: someone who deliberately
    // indexes ~/.config/myapp keeps that working.
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join(".config/myapp");
    write(&root, "a.txt", "visible_token\n");
    let e = Engine::open(cfg(t.path(), &[&root]), true).unwrap();
    e.index_all(false);
    let v = api::call_scoped(
        &e,
        "search",
        &json!({"pattern": "visible_token", "root": root.to_string_lossy()}),
        Scope::Confined,
    )
    .unwrap();
    assert_eq!(v["result"]["files"].as_array().unwrap().len(), 1);
}

// --- the HTTP daemon, end to end

struct Daemon {
    child: Child,
    port: u16,
    _t: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_unumsearch"));
    for k in [
        "UNUMSEARCH_ROOTS",
        "UNUMSEARCH_SPLIT_ROOTS",
        "UNUMSEARCH_INDEX_DIR",
        "UNUMSEARCH_LISTEN",
        "UNUMSEARCH_TOKEN",
        "UNUMSEARCH_CONFIG",
    ] {
        c.env_remove(k);
    }
    c
}

fn start(extra_cfg: &str) -> Daemon {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    write(&root, "src/a.rs", "fn visible_token() {}\n");
    let outside = t.path().join("outside");
    write(&outside, "x.txt", "visible_token outside\n");
    let port = free_port();
    let idx = t.path().join("idx");
    let conf = t.path().join("c.toml");
    std::fs::write(
        &conf,
        format!(
            "roots = [{:?}]\nindex_dir = {:?}\nlisten = \"127.0.0.1:{port}\"\ndebounce_ms = 60000\nmax_wait_ms = 60000\n{extra_cfg}",
            root.to_string_lossy(),
            idx.to_string_lossy()
        ),
    )
    .unwrap();
    let child = bin()
        .args(["--config", &conf.to_string_lossy(), "serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let d = Daemon {
        child,
        port,
        _t: t,
        root,
        outside,
    };
    let t0 = Instant::now();
    loop {
        let hdr: Vec<(&str, String)> = if extra_cfg.contains("auth_token") {
            vec![("X-Unumsearch-Token", "s3cret-token".to_string())]
        } else {
            vec![]
        };
        if let Ok((200, b)) = http(d.port, "GET /status HTTP/1.1", &hdr, "") {
            if b["fresh"] == true {
                break;
            }
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "daemon never came up"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    d
}

fn http(
    port: u16,
    line: &str,
    headers: &[(&str, String)],
    body: &str,
) -> Result<(u16, Value), String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut req = format!("{line}\r\nConnection: close\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req += &format!("Host: 127.0.0.1:{port}\r\n");
    }
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    req += &format!("Content-Length: {}\r\n\r\n", body.len());
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("no status")?;
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    Ok((status, serde_json::from_str(body).unwrap_or(Value::Null)))
}

fn enc(p: &Path) -> String {
    p.to_string_lossy()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[test]
fn http_denies_roots_outside_and_secret_locations() {
    let d = start("");
    let get = |root: &Path| {
        http(
            d.port,
            &format!(
                "GET /search?pattern=visible_token&root={} HTTP/1.1",
                enc(root)
            ),
            &[],
            "",
        )
        .unwrap()
    };
    let (code, v) = get(&d.root);
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["result"]["files"].as_array().unwrap().len(), 1);
    for bad in [
        d.outside.clone(),
        d.root.join("../outside"),
        d.root.join("src/../../outside"),
        PathBuf::from("/etc"),
        PathBuf::from("/"),
    ] {
        let (code, v) = get(&bad);
        assert_eq!(code, 400, "{bad:?}: {v}");
        assert_eq!(v["ok"], false);
    }
    // JSON-RPC over HTTP is confined the same way.
    let body = json!({"jsonrpc":"2.0","id":1,"method":"search","params":{"pattern":"visible_token","root":d.outside.to_string_lossy()}}).to_string();
    let (code, v) = http(d.port, "POST /rpc HTTP/1.1", &[], &body).unwrap();
    assert_eq!(code, 200);
    assert!(v["error"].is_object(), "{v}");
}

#[test]
fn http_rejects_foreign_host_oversized_bodies_and_bad_tokens() {
    let d = start("");
    // DNS rebinding: a Host that is not a loopback name.
    let (code, _) = http(
        d.port,
        "GET /status HTTP/1.1",
        &[("Host", "evil.example.com".into())],
        "",
    )
    .unwrap();
    assert_eq!(code, 403);
    let (code, _) = http(
        d.port,
        "GET /status HTTP/1.1",
        &[("Host", format!("localhost:{}", d.port))],
        "",
    )
    .unwrap();
    assert_eq!(code, 200);
    // Bodies above 1 MiB.
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\",\"params\":{{\"pad\":\"{}\"}}}}",
        "x".repeat(1_100_000)
    );
    let (code, _) = http(d.port, "POST /rpc HTTP/1.1", &[], &big).unwrap();
    assert_eq!(code, 413);
    // Absurd limits are clamped, not honoured.
    let (code, v) = http(
        d.port,
        &format!(
            "GET /search?pattern=visible_token&root={}&max_matches=999999999999 HTTP/1.1",
            enc(&d.root)
        ),
        &[],
        "",
    )
    .unwrap();
    assert_eq!(code, 200, "{v}");
    // A pattern over the cap is refused.
    let (code, _) = http(
        d.port,
        &format!(
            "GET /search?pattern={}&root={} HTTP/1.1",
            "a".repeat(20_000),
            enc(&d.root)
        ),
        &[],
        "",
    )
    .unwrap();
    assert_eq!(code, 400);
}

#[test]
fn http_token_is_enforced_when_configured() {
    let d = start("auth_token = \"s3cret-token\"\n");
    let (code, _) = http(d.port, "GET /status HTTP/1.1", &[], "").unwrap();
    assert_eq!(code, 401);
    let (code, _) = http(
        d.port,
        "GET /status HTTP/1.1",
        &[("X-Unumsearch-Token", "wrong".into())],
        "",
    )
    .unwrap();
    assert_eq!(code, 401);
    let (code, _) = http(
        d.port,
        "GET /status HTTP/1.1",
        &[("X-Unumsearch-Token", "s3cret-token".into())],
        "",
    )
    .unwrap();
    assert_eq!(code, 200);
    let (code, _) = http(
        d.port,
        "GET /status HTTP/1.1",
        &[("Authorization", "Bearer s3cret-token".into())],
        "",
    )
    .unwrap();
    assert_eq!(code, 200);
}

#[test]
fn non_loopback_listen_requires_a_token() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    write(&root, "a.txt", "x\n");
    let conf = t.path().join("c.toml");
    let port = free_port();
    std::fs::write(
        &conf,
        format!(
            "roots = [{:?}]\nindex_dir = {:?}\nlisten = \"0.0.0.0:{port}\"\n",
            root.to_string_lossy(),
            t.path().join("idx").to_string_lossy()
        ),
    )
    .unwrap();
    let out = bin()
        .args(["--config", &conf.to_string_lossy(), "serve"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing"));
}

// ---------------------------------------------------------------- finding 3

#[test]
fn reader_process_never_sees_stale_fresh_after_the_writer_noticed() {
    let d = start("");
    // The writer is watching with a 60 s debounce: no rebuild in this test.
    write(&d.root, "src/new_file.rs", "fn brand_new_marker() {}\n");
    let t0 = Instant::now();
    loop {
        let (_, v) = http(
            d.port,
            "GET /search?pattern=brand_new_marker&files_only=1 HTTP/1.1"
                .replace("search?", &format!("search?root={}&", enc(&d.root)))
                .as_str(),
            &[],
            "",
        )
        .unwrap();
        if v["result"]["files"]
            .as_array()
            .is_some_and(|f| !f.is_empty())
        {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "writer never noticed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // From here on, a separate reader process must either find the file or
    // say that its answer is not fresh.
    let idx = d._t.path().join("idx");
    let conf = d._t.path().join("c.toml");
    let out = bin()
        .args([
            "--config",
            &conf.to_string_lossy(),
            "--index-dir",
            &idx.to_string_lossy(),
        ])
        .args(["search", "-F", "-l", "--no-scan", "brand_new_marker"])
        .arg(&d.root)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let found = v["result"]["files"]
        .as_array()
        .is_some_and(|f| !f.is_empty());
    assert!(
        found || v["result"]["fresh"] == false,
        "stale manifest claimed fresh and missed the change: {v}"
    );
}

// ---------------------------------------------------------------- finding 4

#[test]
fn rapid_forced_rebuilds_keep_every_referenced_shard() {
    let (t, root) = repo();
    let e = Engine::open(cfg(t.path(), &[&root]), true).unwrap();
    e.index_all(false);
    let unit = root.to_string_lossy().into_owned();
    for i in 0..40 {
        e.build_unit(&unit, true).expect("rebuild");
        let m: Value =
            serde_json::from_slice(&std::fs::read(t.path().join("index/manifest.json")).unwrap())
                .unwrap();
        for s in m["units"][&unit]["shards"].as_array().unwrap() {
            let p = t.path().join("index").join(s.as_str().unwrap());
            assert!(
                p.exists(),
                "round {i}: manifest references missing shard {p:?}"
            );
        }
        let r = e
            .search(&SearchOpts {
                pattern: "visible_token".into(),
                root: root.clone(),
                files_only: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(r.files.len(), 1, "round {i}");
    }
    // No stray shard files accumulate.
    let n = std::fs::read_dir(t.path().join("index"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "shard"))
        .count();
    assert_eq!(n, 1);
}

// ---------------------------------------------------------------- finding 5

#[test]
fn files_limit_is_applied_after_deduplication() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..6 {
        write(&root, &format!("f{i}.txt"), "x\n");
    }
    let e = Engine::open(cfg(t.path(), &[&root]), true).unwrap();
    e.index_all(false);
    let unit = root.to_string_lossy().into_owned();
    // Three indexed files changed since the build: listed as pending too.
    for i in 0..3 {
        e.mark_dirty(&unit, Change::File(format!("f{i}.txt")));
    }
    let r = e
        .files(&FilesOpts {
            root: root.clone(),
            max_files: 6,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.files.len(), 6, "{:?}", r.files);
    assert!(!r.truncated);
    let r = e
        .files(&FilesOpts {
            root: root.clone(),
            max_files: 5,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.files.len(), 5);
    assert!(r.truncated);
}

// ---------------------------------------------------------------- finding 6

#[test]
fn nested_configured_root_is_kept_when_the_outer_one_ignores_it() {
    let t = tempfile::tempdir().unwrap();
    let outer = t.path().join("outer");
    std::fs::create_dir_all(outer.join(".git")).unwrap();
    write(&outer, ".gitignore", "inner/\n");
    write(&outer, "o.txt", "shared_marker outer\n");
    let inner = outer.join("inner");
    write(&inner, "i.txt", "shared_marker inner\n");
    let e = Arc::new(Engine::open(cfg(t.path(), &[&outer, &inner]), true).unwrap());
    e.index_all(false);
    assert_eq!(api::all_roots(&e).len(), 2);
    let v = api::call(
        &e,
        "search",
        &json!({"pattern": "shared_marker", "all_roots": true, "files_only": true}),
    )
    .unwrap();
    let files = v["result"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    // And a file both roots see is reported once.
    write(&outer, "common.txt", "dup_marker\n");
    e.index_all(false);
    let v = api::call(
        &e,
        "search",
        &json!({"pattern": "dup_marker", "all_roots": true, "files_only": true}),
    )
    .unwrap();
    assert_eq!(v["result"]["files"].as_array().unwrap().len(), 1);
}
