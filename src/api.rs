// SPDX-License-Identifier: Apache-2.0
//! One request surface shared by every front-end (HTTP, stdio JSON-RPC, MCP).

use crate::engine::{Engine, FilesOpts, SearchOpts};
use serde_json::{json, Value};
use std::path::PathBuf;

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(str::to_string)
}
fn b(v: &Value, k: &str, d: bool) -> bool {
    match v.get(k) {
        Some(Value::Bool(x)) => *x,
        Some(Value::String(x)) => matches!(x.as_str(), "1" | "true" | "yes"),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => d,
    }
}
fn n(v: &Value, k: &str, d: usize) -> usize {
    match v.get(k) {
        Some(Value::Number(x)) => x.as_u64().map(|x| x as usize).unwrap_or(d),
        Some(Value::String(x)) => x.parse().unwrap_or(d),
        _ => d,
    }
}
fn globs(v: &Value) -> Vec<String> {
    match v.get("glob").or_else(|| v.get("globs")) {
        Some(Value::String(x)) => x
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

pub fn default_root(engine: &Engine) -> PathBuf {
    engine
        .cfg
        .root_paths()
        .into_iter()
        .next()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

pub fn search_opts(engine: &Engine, p: &Value) -> Result<SearchOpts, String> {
    let pattern = s(p, "pattern")
        .or_else(|| s(p, "q"))
        .ok_or("missing 'pattern'")?;
    let mode = s(p, "mode").unwrap_or_else(|| "literal".into());
    Ok(SearchOpts {
        pattern,
        root: s(p, "root")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_root(engine)),
        regex: mode == "regex" || b(p, "regex", false),
        case_insensitive: b(p, "ignore_case", b(p, "ci", false)),
        globs: globs(p),
        max_matches: n(p, "max_matches", 1000),
        max_files: n(p, "max_files", 20_000),
        files_only: b(p, "files_only", false),
        candidates_only: b(p, "candidates_only", false),
        scan_fallback: b(p, "scan_fallback", true),
    })
}

pub fn files_opts(engine: &Engine, p: &Value) -> FilesOpts {
    FilesOpts {
        root: s(p, "root")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_root(engine)),
        globs: globs(p),
        regex: s(p, "regex").or_else(|| s(p, "name")),
        max_files: n(p, "max_files", 5000),
        scan_fallback: b(p, "scan_fallback", true),
    }
}

/// Dispatch a method by name. Errors are returned as strings; transports
/// wrap them in their own error envelope.
pub fn call(engine: &Engine, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "search" => {
            let o = search_opts(engine, params)?;
            let r = engine.search(&o)?;
            Ok(json!({"ok": true, "result": r}))
        }
        "files" => {
            let o = files_opts(engine, params);
            let r = engine.files(&o)?;
            Ok(json!({"ok": true, "result": r}))
        }
        "status" => Ok(engine.status()),
        "reindex" => {
            if !engine.is_writer() {
                return Err("this process is not the index writer".into());
            }
            let force = b(params, "force", false);
            match s(params, "unit") {
                Some(u) => {
                    engine.build_unit(&u, force);
                }
                None => {
                    engine.index_all(force);
                }
            }
            Ok(engine.status())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}
