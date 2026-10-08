// SPDX-License-Identifier: Apache-2.0
//! JSON-RPC 2.0 over stdio, one message per line. Methods: search, files,
//! status, reindex (see `unumsearch::api`).

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use unumsearch::{api, Engine};

pub fn handle_line(engine: &Engine, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            )
        }
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let res = api::call(engine, method, &params);
    let id = id?; // notifications get no reply
    Some(match res {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
        Err(e) => {
            let code = if e.starts_with("unknown method") {
                -32601
            } else {
                -32602
            };
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": e}})
        }
    })
}

pub fn run(engine: &Engine) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(engine, &line) {
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
    }
}
