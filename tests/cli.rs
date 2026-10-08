// SPDX-License-Identifier: Apache-2.0
//! The binary's front-ends: CLI JSON, stdio JSON-RPC, and MCP.

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_unumsearch"));
    // Never pick up the developer's own config.
    c.env_remove("UNUMSEARCH_ROOTS");
    c
}

fn setup() -> (tempfile::TempDir, String, String) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn alpha_beta() {}\n").unwrap();
    std::fs::write(root.join("README.md"), "alpha docs\n").unwrap();
    let idx = t.path().join("idx");
    std::fs::write(t.path().join("empty.toml"), "").unwrap();
    (
        t,
        root.to_string_lossy().into_owned(),
        idx.to_string_lossy().into_owned(),
    )
}

fn empty_config(idx: &str) -> String {
    std::path::Path::new(idx)
        .parent()
        .unwrap()
        .join("empty.toml")
        .to_string_lossy()
        .into_owned()
}

fn run_stdio(args: &[&str], root: &str, idx: &str, input: &str) -> Vec<Value> {
    let cfg = empty_config(idx);
    let mut child = bin()
        .args(["--config", cfg.as_str(), "--root", root, "--index-dir", idx])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn cli_index_search_files_status() {
    let (_t, root, idx) = setup();
    let cfg = empty_config(&idx);
    let base = [
        "--config",
        cfg.as_str(),
        "--root",
        root.as_str(),
        "--index-dir",
        idx.as_str(),
    ];
    let out = bin().args(base).arg("index").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let st: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(st["files"], 2);

    let out = bin()
        .args(base)
        .args(["search", "-l", "alpha_b", &root])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["backend"], "index");
    assert_eq!(v["result"]["files"].as_array().unwrap().len(), 1);

    let out = bin()
        .args(base)
        .args(["search", "--text", "-i", "ALPHA", &root])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.lines().count(), 2, "{text}");

    let out = bin()
        .args(base)
        .args(["files", "-g", "*.md", &root])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["result"]["files"][0]
        .as_str()
        .unwrap()
        .ends_with("README.md"));
}

#[test]
fn rpc_and_mcp_over_stdio() {
    let (_t, root, idx) = setup();
    let cfg = empty_config(&idx);
    let out = bin()
        .args([
            "--config",
            cfg.as_str(),
            "--root",
            &root,
            "--index-dir",
            &idx,
            "index",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let req = format!(
        "{}\n{}\n",
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"search","params":{"pattern":"alpha","root":root,"files_only":true}}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"nope"})
    );
    let r = run_stdio(&["rpc"], &root, &idx, &req);
    assert_eq!(
        r[0]["result"]["result"]["files"].as_array().unwrap().len(),
        2
    );
    assert_eq!(r[1]["error"]["code"], -32601);

    let req = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"pattern":"alpha_beta","path":root}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"find_files","arguments":{"path":root,"glob":["*.rs"]}}}),
    ]
    .iter()
    .map(|v| v.to_string() + "\n")
    .collect::<String>();
    let r = run_stdio(&["mcp"], &root, &idx, &req);
    assert_eq!(r.len(), 4, "notifications get no reply");
    assert_eq!(r[0]["result"]["serverInfo"]["name"], "unumsearch");
    let names: Vec<&str> = r[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["search", "find_files", "index_status"]);
    assert_eq!(r[2]["result"]["isError"], false);
    assert_eq!(
        r[2]["result"]["structuredContent"]["result"]["matches"][0]["line"],
        1
    );
    assert_eq!(
        r[3]["result"]["structuredContent"]["result"]["files"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn explicit_root_does_not_merge_default_config_roots() {
    let (t, root, idx) = setup();
    // A default config (as the platform resolves it from HOME/XDG) that
    // names another root.
    let other = t.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("x.txt"), "x\n").unwrap();
    let home = t.path().join("home");
    let xdg = home.join(".config");
    let mac = home.join("Library/Application Support");
    for base in [&xdg, &mac] {
        std::fs::create_dir_all(base.join("unumsearch")).unwrap();
        std::fs::write(
            base.join("unumsearch/config.toml"),
            format!("roots = [{:?}]\n", other.to_string_lossy()),
        )
        .unwrap();
    }
    let status = |extra: &[&str]| -> Value {
        let built = bin()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env_remove("UNUMSEARCH_CONFIG")
            .args(extra)
            .args(["--index-dir", idx.as_str(), "index"])
            .output()
            .unwrap();
        assert!(built.status.success(), "{built:?}");
        let out = bin()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env_remove("UNUMSEARCH_CONFIG")
            .args(extra)
            .args(["--index-dir", idx.as_str(), "status"])
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let units = |v: &Value| -> Vec<String> {
        v["units"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["path"].as_str().unwrap().to_string())
            .collect()
    };
    // Without --root the default config applies.
    let idx_path = std::path::Path::new(&idx);
    let _ = std::fs::remove_dir_all(idx_path);
    let st = status(&[]);
    assert!(units(&st).iter().any(|u| u.ends_with("other")), "{st}");
    // An explicit --root replaces it instead of merging.
    let _ = std::fs::remove_dir_all(idx_path);
    let st = status(&["--root", root.as_str()]);
    let u = units(&st);
    assert!(u.iter().all(|u| !u.ends_with("other")), "{st}");
    // --no-default-config skips it too.
    let _ = std::fs::remove_dir_all(idx_path);
    let st = status(&["--no-default-config"]);
    assert!(units(&st).is_empty(), "{st}");
}

#[test]
fn all_roots_search_and_batch_lookup() {
    let (t, root, idx) = setup();
    let second = t.path().join("second");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("b.rs"), "fn alpha_gamma() {}\n").unwrap();
    let second = second.to_string_lossy().into_owned();
    let cfg = empty_config(&idx);
    let base = [
        "--config",
        cfg.as_str(),
        "--root",
        root.as_str(),
        "--root",
        second.as_str(),
        "--index-dir",
        idx.as_str(),
    ];
    let st = bin().args(base).arg("index").output().unwrap();
    assert!(st.status.success());
    let out = bin()
        .args(base)
        .args(["search", "-F", "alpha_", "--all-roots", "-l"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let files = v["result"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{v}");
    assert_eq!(v["result"]["roots"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["result"]["covered"], true, "{v}");
    // Batch lookup over every root through the JSON-RPC front-end.
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"lookup","params":{"patterns":["alpha_beta","alpha_gamma","no_such_symbol"],"all_roots":true}}"#;
    let replies = run_stdio_with(&base, &["rpc"], &format!("{req}\n"));
    let r = &replies[0]["result"]["result"];
    let found: Vec<bool> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["found"].as_bool().unwrap())
        .collect();
    assert_eq!(found, vec![true, true, false], "{r}");
    assert!(r["results"][1]["files"][0]
        .as_str()
        .unwrap()
        .ends_with("b.rs"));
}

fn run_stdio_with(global: &[&str], args: &[&str], input: &str) -> Vec<Value> {
    let mut child = bin()
        .args(global)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
