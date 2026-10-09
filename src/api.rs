// SPDX-License-Identifier: Apache-2.0
//! One request surface shared by every front-end (HTTP, stdio JSON-RPC, MCP).

use crate::engine::{Engine, FilesOpts, SearchOpts, SearchResult};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

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
        max_result_bytes: engine.cfg.max_result_bytes(),
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
        max_result_bytes: engine.cfg.max_result_bytes(),
    }
}

/// `all_roots=1` (or `root=*`): the query runs over every configured root.
fn wants_all_roots(p: &Value) -> bool {
    b(p, "all_roots", false) || s(p, "root").as_deref() == Some("*")
}

/// Every configured root. A root nested inside another is kept: the outer
/// root's ignore files or excludes may hide files the nested root's own
/// corpus contains. Results are de-duplicated by path when merged.
pub fn all_roots(engine: &Engine) -> Vec<PathBuf> {
    let mut roots = engine.cfg.root_paths();
    roots.sort();
    roots.dedup();
    roots
}

fn backend_rank(b: &str) -> u8 {
    match b {
        "index" => 0,
        "scan" => 1,
        _ => 2,
    }
}

/// Run one search per configured root and merge the answers: files and
/// matches concatenated (deduplicated by path), `covered`/`fresh` only when
/// every root's answer is, `truncated` when any is or the merged caps are hit.
/// The caps and the result budget apply to the merged answer: each root gets
/// what the roots before it left over.
pub fn search_all_roots(engine: &Engine, o: &SearchOpts) -> Result<Value, String> {
    let t0 = Instant::now();
    let mut merged = SearchResult {
        backend: "index",
        covered: true,
        fresh: true,
        files: vec![],
        matches: vec![],
        candidates: 0,
        truncated: false,
        complete: false,
        units: vec![],
        uncovered: vec![],
        elapsed_ms: 0.0,
    };
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut seen_matches: HashSet<(String, u64)> = HashSet::new();
    let mut per_root = Vec::new();
    let mut used = 0usize;
    for root in all_roots(engine) {
        let mut ro = o.clone();
        ro.root = root.clone();
        ro.max_files = o.max_files.saturating_sub(merged.files.len());
        ro.max_matches = o.max_matches.saturating_sub(merged.matches.len());
        ro.max_result_bytes = o.max_result_bytes.saturating_sub(used);
        let r = engine.search(&ro)?;
        used += r.result_bytes();
        per_root.push(json!({
            "root": root.to_string_lossy(),
            "backend": r.backend,
            "covered": r.covered,
            "fresh": r.fresh,
            "files": r.files.len(),
            "truncated": r.truncated,
            "uncovered": r.uncovered,
        }));
        if backend_rank(r.backend) > backend_rank(merged.backend) {
            merged.backend = r.backend;
        }
        merged.covered &= r.covered;
        merged.fresh &= r.fresh;
        merged.truncated |= r.truncated;
        merged.candidates += r.candidates;
        merged.units.extend(r.units);
        for u in r.uncovered {
            if !merged.uncovered.contains(&u) {
                merged.uncovered.push(u);
            }
        }
        for f in r.files {
            if seen_files.insert(f.clone()) {
                merged.files.push(f);
            }
        }
        for m in r.matches {
            if seen_matches.insert((m.path.clone(), m.line)) {
                merged.matches.push(m);
            }
        }
    }
    if merged.files.len() > o.max_files {
        merged.files.truncate(o.max_files);
        merged.truncated = true;
    }
    if merged.matches.len() > o.max_matches {
        merged.matches.truncate(o.max_matches);
        merged.truncated = true;
    }
    merged.complete = merged.covered && merged.fresh && !merged.truncated;
    merged.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut v = serde_json::to_value(&merged).map_err(|e| e.to_string())?;
    v["roots"] = Value::Array(per_root);
    Ok(json!({"ok": true, "result": v}))
}

fn patterns(p: &Value) -> Vec<String> {
    match p.get("patterns") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        // Over HTTP GET: newline-separated (`patterns=a%0Ab`), since a
        // literal may itself contain commas.
        Some(Value::String(x)) => x
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => vec![],
    }
}

/// Batch existence lookup: which files contain each of many patterns
/// (literal by default), under one root or every root, in one call.
pub fn lookup(engine: &Engine, p: &Value) -> Result<Value, String> {
    let t0 = Instant::now();
    let pats = patterns(p);
    if pats.is_empty() {
        return Err("missing 'patterns' (array, or newline-separated string)".into());
    }
    if pats.len() > 10_000 {
        return Err("too many patterns (max 10000)".into());
    }
    let mut base = search_opts(engine, &json!({"pattern": "x"}))?;
    let mode = s(p, "mode").unwrap_or_else(|| "literal".into());
    base.regex = mode == "regex" || b(p, "regex", false);
    base.case_insensitive = b(p, "ignore_case", b(p, "ci", false));
    base.globs = globs(p);
    base.files_only = true;
    base.max_files = n(p, "max_files", 100);
    base.scan_fallback = b(p, "scan_fallback", true);
    let roots = if wants_all_roots(p) {
        all_roots(engine)
    } else {
        vec![s(p, "root")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_root(engine))]
    };
    let (mut covered, mut fresh) = (true, true);
    let mut uncovered: Vec<Value> = Vec::new();
    let mut results = Vec::with_capacity(pats.len());
    for pat in pats {
        let mut files: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let (mut pc, mut pf, mut pt) = (true, true, false);
        for root in &roots {
            let mut o = base.clone();
            o.pattern = pat.clone();
            o.root = root.clone();
            let r = engine.search(&o)?;
            pc &= r.covered;
            pf &= r.fresh;
            pt |= r.truncated;
            for u in &r.uncovered {
                let u = json!(u);
                if !uncovered.contains(&u) {
                    uncovered.push(u);
                }
            }
            for f in r.files {
                if seen.insert(f.clone()) {
                    files.push(f);
                }
            }
        }
        if files.len() > base.max_files {
            files.truncate(base.max_files);
            pt = true;
        }
        covered &= pc;
        fresh &= pf;
        results.push(json!({
            "pattern": pat,
            "found": !files.is_empty(),
            "files": files,
            "covered": pc,
            "fresh": pf,
            "truncated": pt,
            "complete": pc && pf && !pt,
        }));
    }
    Ok(json!({"ok": true, "result": {
        "results": results,
        "roots": roots.iter().map(|r| r.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "covered": covered,
        "uncovered": uncovered,
        "fresh": fresh,
        "elapsed_ms": t0.elapsed().as_secs_f64() * 1000.0,
    }}))
}

/// Who is asking. `Local` callers (CLI, stdio JSON-RPC, MCP) run as the user
/// and may search any directory; `Confined` callers (the HTTP API) may only
/// search inside the configured roots and get request limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Local,
    Confined,
}

/// Limits applied to confined (HTTP) requests.
pub const MAX_PATTERN_BYTES: usize = 16 * 1024;
pub const MAX_RESULT_FILES: usize = 1_000_000;
pub const MAX_RESULT_MATCHES: usize = 200_000;
const MAX_GLOBS: usize = 64;

/// Authorise the request's root and clamp its limits.
fn guard(engine: &Engine, method: &str, params: &Value, scope: Scope) -> Result<Value, String> {
    let mut p = params.clone();
    if !matches!(method, "search" | "files" | "lookup" | "reindex") {
        return Ok(p);
    }
    if let Some(r) = s(&p, "root") {
        if r != "*" {
            engine.authorize_root(Path::new(&r), scope == Scope::Confined)?;
        }
    }
    if scope == Scope::Confined {
        if s(&p, "pattern")
            .or_else(|| s(&p, "q"))
            .is_some_and(|x| x.len() > MAX_PATTERN_BYTES)
            || s(&p, "regex").is_some_and(|x| x.len() > MAX_PATTERN_BYTES)
        {
            return Err("pattern too long".into());
        }
        if globs(&p).len() > MAX_GLOBS {
            return Err("too many globs".into());
        }
        for (k, cap) in [
            ("max_files", MAX_RESULT_FILES),
            ("max_matches", MAX_RESULT_MATCHES),
        ] {
            if params.get(k).is_some() {
                p[k] = Value::from(n(params, k, cap).min(cap) as u64);
            }
        }
    }
    Ok(p)
}

/// [`call_scoped`] with [`Scope::Local`].
pub fn call(engine: &Engine, method: &str, params: &Value) -> Result<Value, String> {
    call_scoped(engine, method, params, Scope::Local)
}

/// Dispatch a method by name. Errors are returned as strings; transports
/// wrap them in their own error envelope.
pub fn call_scoped(
    engine: &Engine,
    method: &str,
    params: &Value,
    scope: Scope,
) -> Result<Value, String> {
    let params = &guard(engine, method, params, scope)?;
    match method {
        "search" => {
            let o = search_opts(engine, params)?;
            if wants_all_roots(params) {
                return search_all_roots(engine, &o);
            }
            let r = engine.search(&o)?;
            Ok(json!({"ok": true, "result": r}))
        }
        "files" => {
            let o = files_opts(engine, params);
            let r = engine.files(&o)?;
            Ok(json!({"ok": true, "result": r}))
        }
        "lookup" => lookup(engine, params),
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
