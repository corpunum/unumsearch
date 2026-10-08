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
