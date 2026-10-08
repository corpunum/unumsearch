// SPDX-License-Identifier: Apache-2.0
//! Model Context Protocol server over stdio (newline-delimited JSON-RPC).
//! Tools: search, find_files, index_status.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use unumsearch::{api, Engine};

const PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "search",
            "description": "Search file contents under a directory using the local index (falls back to a direct scan for unindexed directories). Literal by default; set regex=true for a regular expression. Returns matching lines with file paths and line numbers, plus index freshness.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Text (or regex when regex=true) to find."},
                    "path": {"type": "string", "description": "Directory to search (absolute). Defaults to the first configured root."},
                    "regex": {"type": "boolean", "description": "Treat pattern as a regular expression (Rust regex syntax)."},
                    "ignore_case": {"type": "boolean"},
                    "glob": {"type": "array", "items": {"type": "string"}, "description": "ripgrep-style globs, e.g. \"*.rs\" or \"!tests/**\"."},
                    "files_only": {"type": "boolean", "description": "Only list matching files."},
                    "max_matches": {"type": "integer", "description": "Default 200."}
                },
                "required": ["pattern"]
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "find_files",
            "description": "List files under a directory by glob and/or regex on the relative path or basename, from the local index.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "glob": {"type": "array", "items": {"type": "string"}},
                    "regex": {"type": "string"},
                    "max_files": {"type": "integer", "description": "Default 500."}
                }
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "index_status",
            "description": "Index coverage and freshness: units, file counts, whether a watcher keeps it fresh.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        }
    ])
}

fn call_tool(engine: &Engine, name: &str, args: &Value) -> Result<Value, String> {
    let mut a = args.clone();
    if !a.is_object() {
        a = json!({});
    }
    if let Some(p) = a.get("path").cloned() {
        a["root"] = p;
    }
    match name {
        "search" => {
            if a.get("regex").and_then(|v| v.as_bool()).unwrap_or(false) {
                a["mode"] = json!("regex");
            } else {
                a["mode"] = json!("literal");
            }
            if a.get("max_matches").is_none() {
                a["max_matches"] = json!(200);
            }
            api::call(engine, "search", &a)
        }
        "find_files" => {
            if a.get("max_files").is_none() {
                a["max_files"] = json!(500);
            }
            api::call(engine, "files", &a)
        }
        "index_status" => {
            let mut s = api::call(engine, "status", &a)?;
            // Per-unit detail can be long; keep the summary.
            if let Some(o) = s.as_object_mut() {
                o.remove("units");
            }
            Ok(s)
        }
        _ => Err(format!("unknown tool '{name}'")),
    }
}

pub fn handle(engine: &Engine, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let v = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL);
            json!({
                "protocolVersion": v,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "unumsearch", "version": env!("CARGO_PKG_VERSION")}
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(engine, name, &args) {
                Ok(v) => {
                    json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v, "isError": false})
                }
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
            )
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn run(engine: &Engine) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(engine, &msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            ),
        };
        if let Some(r) = reply {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
}
