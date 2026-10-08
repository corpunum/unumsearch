// SPDX-License-Identifier: Apache-2.0
//! unumsearch CLI: index, search, files, status, watch, serve (HTTP), rpc
//! (stdio JSON-RPC 2.0) and mcp (stdio Model Context Protocol server).

mod frontends;

use lexopt::prelude::*;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use unumsearch::config::{expand_tilde, Config};
use unumsearch::engine::{Engine, FilesOpts, SearchOpts};

const USAGE: &str = "unumsearch - always-fresh indexed file & code search

USAGE:
  unumsearch [GLOBAL] <command> [ARGS]

COMMANDS:
  index [--force]                 build or update the index once
  search PATTERN [PATH]           content search (JSON output)
      -F, --literal               literal string (default: regex, like ripgrep)
      -i, --ignore-case           case-insensitive
      -g, --glob GLOB             include/exclude glob (repeatable, '!' negates)
      -l, --files-with-matches    only list files
      -m, --max-matches N         cap on returned matches (default 1000)
      --candidates                return unverified index candidates
      --no-scan                   do not scan roots the index does not cover
      --text                      ripgrep-style text output instead of JSON
  files [PATH]                    list indexed files
      -g, --glob GLOB             glob filter (repeatable)
      --regex RE                  regex on relative path or basename
      --max N                     cap (default 5000)
  status                          index status (JSON)
  excludes                        effective exclude patterns (for rg --ignore-file)
  watch                           keep the index fresh (foreground)
  serve                           watch + HTTP JSON API on --listen
  rpc [--watch]                   JSON-RPC 2.0 over stdio
  mcp [--watch]                   MCP server over stdio

GLOBAL:
  --config FILE                   TOML config (default: $UNUMSEARCH_CONFIG or
                                  <platform config dir>/unumsearch/config.toml)
  --root DIR                      add an index root (repeatable)
  --split-root DIR                add a root whose children are separate units
  --index-dir DIR                 index location
  --listen ADDR                   HTTP address for serve (default 127.0.0.1:7781)
  --exclude PATTERN               extra gitignore-style exclude (repeatable)
  --max-memory-mb N               build memory budget
";

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("unumsearch: {msg}");
    std::process::exit(2)
}

/// Write a line to stdout; a closed pipe (`| head`) ends the process quietly.
fn out(line: &str) {
    use std::io::Write;
    let mut o = std::io::stdout().lock();
    if writeln!(o, "{line}").is_err() {
        std::process::exit(0);
    }
}

fn print(v: &serde_json::Value) {
    out(&serde_json::to_string(v).unwrap_or_default());
}

fn main() {
    let mut p = lexopt::Parser::from_env();
    let mut config_path: Option<PathBuf> = None;
    let mut roots: Vec<String> = vec![];
    let mut splits: Vec<String> = vec![];
    let mut excludes: Vec<String> = vec![];
    let mut index_dir: Option<String> = None;
    let mut listen: Option<String> = None;
    let mut max_mem: Option<u64> = None;
    let mut cmd: Option<String> = None;
    let mut rest: Vec<std::ffi::OsString> = vec![];

    // Global options come before the command.
    while let Some(arg) = p.next().unwrap_or_else(|e| die(e)) {
        match arg {
            Long("config") => config_path = Some(p.value().unwrap_or_else(|e| die(e)).into()),
            Long("root") => roots.push(
                p.value()
                    .unwrap_or_else(|e| die(e))
                    .string()
                    .unwrap_or_else(|e| die(e)),
            ),
            Long("split-root") => {
                let v = p
                    .value()
                    .unwrap_or_else(|e| die(e))
                    .string()
                    .unwrap_or_else(|e| die(e));
                roots.push(v.clone());
                splits.push(v);
            }
            Long("exclude") => excludes.push(
                p.value()
                    .unwrap_or_else(|e| die(e))
                    .string()
                    .unwrap_or_else(|e| die(e)),
            ),
            Long("index-dir") => {
                index_dir = Some(
                    p.value()
                        .unwrap_or_else(|e| die(e))
                        .string()
                        .unwrap_or_else(|e| die(e)),
                )
            }
            Long("listen") => {
                listen = Some(
                    p.value()
                        .unwrap_or_else(|e| die(e))
                        .string()
                        .unwrap_or_else(|e| die(e)),
                )
            }
            Long("max-memory-mb") => {
                max_mem = Some(
                    p.value()
                        .unwrap_or_else(|e| die(e))
                        .parse()
                        .unwrap_or_else(|e| die(e)),
                )
            }
            Short('h') | Long("help") => {
                print!("{USAGE}");
                return;
            }
            Short('V') | Long("version") => {
                println!("unumsearch {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Value(v) => {
                cmd = Some(v.string().unwrap_or_else(|e| die(e)));
                rest = p.raw_args().map(|r| r.collect()).unwrap_or_default();
                break;
            }
            other => die(other.unexpected()),
        }
    }
    let Some(cmd) = cmd else {
        print!("{USAGE}");
        std::process::exit(2);
    };

    let mut cfg = Config::load(config_path.as_deref()).unwrap_or_else(|e| die(e));
    if !roots.is_empty() {
        cfg.roots.extend(roots);
    }
    cfg.split_roots.extend(splits);
    cfg.excludes.extend(excludes);
    if let Some(d) = index_dir {
        cfg.index_dir = Some(d);
    }
    if let Some(l) = listen {
        cfg.listen = l;
    }
    if let Some(m) = max_mem {
        cfg.max_memory_mb = m;
    }

    let mut sp = lexopt::Parser::from_args(rest);
    match cmd.as_str() {
        "index" => {
            let mut force = false;
            while let Some(a) = sp.next().unwrap_or_else(|e| die(e)) {
                match a {
                    Long("force") => force = true,
                    o => die(o.unexpected()),
                }
            }
            let e = Engine::open(cfg, true).unwrap_or_else(|e| die(e));
            if !e.is_writer() {
                die("another process is writing this index (is `unumsearch serve` running?)");
            }
            let t = std::time::Instant::now();
            e.index_all(force);
            let mut st = e.status();
            st["elapsed_ms"] = json!(t.elapsed().as_millis() as u64);
            print(&st);
        }
        "search" => {
            let mut o = SearchOpts {
                regex: true,
                ..Default::default()
            };
            let mut pos: Vec<String> = vec![];
            let mut text = false;
            while let Some(a) = sp.next().unwrap_or_else(|e| die(e)) {
                match a {
                    Short('F') | Long("literal") => o.regex = false,
                    Short('i') | Long("ignore-case") => o.case_insensitive = true,
                    Short('s') | Long("case-sensitive") => o.case_insensitive = false,
                    Short('g') | Long("glob") => o.globs.push(
                        sp.value()
                            .unwrap_or_else(|e| die(e))
                            .string()
                            .unwrap_or_else(|e| die(e)),
                    ),
                    Short('l') | Long("files-with-matches") => o.files_only = true,
                    Short('m') | Long("max-matches") => {
                        o.max_matches = sp
                            .value()
                            .unwrap_or_else(|e| die(e))
                            .parse()
                            .unwrap_or_else(|e| die(e))
                    }
                    Long("max-files") => {
                        o.max_files = sp
                            .value()
                            .unwrap_or_else(|e| die(e))
                            .parse()
                            .unwrap_or_else(|e| die(e))
                    }
                    Long("candidates") => o.candidates_only = true,
                    Long("no-scan") => o.scan_fallback = false,
                    Long("text") => text = true,
                    Short('e') | Long("regexp") => pos.insert(
                        0,
                        sp.value()
                            .unwrap_or_else(|e| die(e))
                            .string()
                            .unwrap_or_else(|e| die(e)),
                    ),
                    Value(v) => pos.push(v.string().unwrap_or_else(|e| die(e))),
                    o => die(o.unexpected()),
                }
            }
            if pos.is_empty() {
                die("search needs a PATTERN");
            }
            o.pattern = pos[0].clone();
            o.root = match pos.get(1) {
                Some(p) => expand_tilde(p),
                None => std::env::current_dir().unwrap_or_default(),
            };
            let e = Engine::open(cfg, false).unwrap_or_else(|e| die(e));
            match e.search(&o) {
                Ok(r) if text => {
                    if o.files_only || o.candidates_only {
                        for f in &r.files {
                            out(f);
                        }
                    } else {
                        for m in &r.matches {
                            out(&format!("{}:{}:{}", m.path, m.line, m.text));
                        }
                    }
                    if r.files.is_empty() {
                        std::process::exit(1);
                    }
                }
                Ok(r) => print(&json!({"ok": true, "result": r})),
                Err(err) => {
                    print(&json!({"ok": false, "error": err}));
                    std::process::exit(2);
                }
            }
        }
        "files" => {
            let mut o = FilesOpts {
                root: PathBuf::new(),
                globs: vec![],
                regex: None,
                max_files: 5000,
                scan_fallback: true,
            };
            let mut path: Option<String> = None;
            while let Some(a) = sp.next().unwrap_or_else(|e| die(e)) {
                match a {
                    Short('g') | Long("glob") => o.globs.push(
                        sp.value()
                            .unwrap_or_else(|e| die(e))
                            .string()
                            .unwrap_or_else(|e| die(e)),
                    ),
                    Long("regex") => {
                        o.regex = Some(
                            sp.value()
                                .unwrap_or_else(|e| die(e))
                                .string()
                                .unwrap_or_else(|e| die(e)),
                        )
                    }
                    Long("max") => {
                        o.max_files = sp
                            .value()
                            .unwrap_or_else(|e| die(e))
                            .parse()
                            .unwrap_or_else(|e| die(e))
                    }
                    Long("no-scan") => o.scan_fallback = false,
                    Value(v) => path = Some(v.string().unwrap_or_else(|e| die(e))),
                    o => die(o.unexpected()),
                }
            }
            o.root = path
                .map(|p| expand_tilde(&p))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let e = Engine::open(cfg, false).unwrap_or_else(|e| die(e));
            match e.files(&o) {
                Ok(r) => print(&json!({"ok": true, "result": r})),
                Err(err) => {
                    print(&json!({"ok": false, "error": err}));
                    std::process::exit(2);
                }
            }
        }
        "excludes" => {
            // Effective exclude patterns, in gitignore syntax: pass to
            // `rg --ignore-file` to search exactly the indexed corpus.
            for l in cfg.exclude_lines() {
                out(&l);
            }
        }
        "status" => {
            let e = Engine::open(cfg, false).unwrap_or_else(|e| die(e));
            print(&e.status());
        }
        "watch" | "serve" => {
            let e = Arc::new(Engine::open(cfg, true).unwrap_or_else(|e| die(e)));
            if !e.is_writer() {
                die("another process is already writing this index");
            }
            let stop = Arc::new(AtomicBool::new(false));
            if cmd == "serve" {
                let e2 = e.clone();
                let s2 = stop.clone();
                std::thread::spawn(move || unumsearch::watch::run(e2, s2));
                frontends::http::serve(e);
            } else {
                unumsearch::watch::run(e, stop);
            }
        }
        "rpc" | "mcp" => {
            let mut watch = false;
            while let Some(a) = sp.next().unwrap_or_else(|e| die(e)) {
                match a {
                    Long("watch") => watch = true,
                    o => die(o.unexpected()),
                }
            }
            let e = Arc::new(Engine::open(cfg, watch).unwrap_or_else(|e| die(e)));
            if watch && e.is_writer() {
                let e2 = e.clone();
                std::thread::spawn(move || {
                    unumsearch::watch::run(e2, Arc::new(AtomicBool::new(false)))
                });
            }
            if cmd == "rpc" {
                frontends::rpc::run(&e);
            } else {
                frontends::mcp::run(&e);
            }
        }
        other => die(format!("unknown command '{other}' (see --help)")),
    }
}
