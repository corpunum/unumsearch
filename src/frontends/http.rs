// SPDX-License-Identifier: Apache-2.0
//! Local HTTP/JSON API: GET /health, /status, /search, /files, /lookup;
//! POST /rpc (JSON-RPC 2.0).
//!
//! Hardening: every request runs with [`api::Scope::Confined`] (roots must lie
//! inside the configured roots; secret locations are refused; limits on
//! pattern size and result counts), `Host` must be a loopback name (DNS
//! rebinding), an optional shared token is required when configured (and is
//! mandatory off loopback), and request lines and bodies are size-capped.

use serde_json::{json, Map, Value};
use std::io::Read;
use std::sync::Arc;
use unumsearch::{api, Engine};

/// RFC 3339 UTC (`2026-11-11T15:00:00Z`) from Unix seconds.
fn rfc3339_utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from a day count (proleptic Gregorian, era = 400 years).
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `GET /health`: which build is running and since when. Constant-time; it does
/// not touch the index, so a monitor can poll it during a rebuild.
fn health(started_at: &str) -> Value {
    json!({
        "ok": true,
        "name": "unumsearch",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": env!("UNUM_GIT_SHA"),
        "started_at": started_at,
    })
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or(())
                {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            x => out.push(x),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_params(url: &str) -> Value {
    let mut m = Map::new();
    if let Some((_, q)) = url.split_once('?') {
        for kv in q.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let k = pct_decode(k);
            let v = pct_decode(v);
            // Repeated keys (glob=a&glob=b) accumulate comma-separated.
            match m.get_mut(&k) {
                Some(Value::String(prev)) => {
                    prev.push(',');
                    prev.push_str(&v);
                }
                _ => {
                    m.insert(k, Value::String(v));
                }
            }
        }
    }
    Value::Object(m)
}

fn respond(req: tiny_http::Request, code: u16, body: &Value) {
    let data = serde_json::to_vec(body).unwrap_or_default();
    let ctype =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    // One request per connection: with keep-alive, Nagle + delayed ACK on the
    // client added ~40 ms to every reused-connection request on loopback.
    let close = tiny_http::Header::from_bytes(&b"Connection"[..], &b"close"[..]).unwrap();
    let _ = req.respond(
        tiny_http::Response::from_data(data)
            .with_status_code(code)
            .with_header(ctype)
            .with_header(close),
    );
}

const MAX_BODY: u64 = 1 << 20;
const MAX_URL: usize = 64 * 1024;

fn header<'a>(req: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

/// `Host` names this server answers to: loopback names, plus the host part of
/// the configured listen address.
fn host_ok(host: &str, listen: &str) -> bool {
    let h = host.trim();
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map_or(h, |(n, _)| n)
    };
    let lname = listen
        .rsplit_once(':')
        .map_or(listen, |(n, _)| n)
        .trim_matches(|c| c == '[' || c == ']');
    name.eq_ignore_ascii_case("localhost")
        || name == lname
        || name
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn token_ok(req: &tiny_http::Request, token: &str) -> bool {
    let given = header(req, "X-Unumsearch-Token")
        .map(str::to_string)
        .or_else(|| {
            header(req, "Authorization")
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_default();
    // Constant-time comparison.
    let (a, b) = (given.as_bytes(), token.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..a.len().min(b.len()) {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

pub fn serve(engine: Arc<Engine>) {
    let addr = engine.cfg.listen.clone();
    if !engine.cfg.listen_is_loopback() && engine.cfg.auth_token.is_none() {
        eprintln!(
            "unumsearch: refusing to listen on {addr}: not a loopback address and no auth_token / UNUMSEARCH_TOKEN is set"
        );
        std::process::exit(2);
    }
    let server = match tiny_http::Server::http(&addr) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("unumsearch: cannot listen on {addr}: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("unumsearch: listening on http://{addr}");
    let started_at = rfc3339_utc((unumsearch::engine::now_ms() / 1000) as i64);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let server = server.clone();
        let engine = engine.clone();
        let started_at = started_at.clone();
        handles.push(std::thread::spawn(move || loop {
            let Ok(mut req) = server.recv() else { break };
            let url = req.url().to_string();
            if url.len() > MAX_URL {
                respond(req, 414, &json!({"ok": false, "error": "URL too long"}));
                continue;
            }
            if !header(&req, "Host").is_none_or(|h| host_ok(h, &engine.cfg.listen)) {
                respond(req, 403, &json!({"ok": false, "error": "bad Host header"}));
                continue;
            }
            if let Some(t) = &engine.cfg.auth_token {
                if !token_ok(&req, t) {
                    respond(req, 401, &json!({"ok": false, "error": "unauthorized"}));
                    continue;
                }
            }
            let path = url.split('?').next().unwrap_or("").to_string();
            if path == "/rpc" {
                let mut body = String::new();
                let _ = req.as_reader().take(MAX_BODY + 1).read_to_string(&mut body);
                if body.len() as u64 > MAX_BODY {
                    respond(
                        req,
                        413,
                        &json!({"ok": false, "error": "request body too large"}),
                    );
                    continue;
                }
                // Held until the reply is written (see `Engine::query_slot`).
                let _slot = engine.query_slot();
                let reply = super::rpc::handle_line(&engine, &body, api::Scope::Confined)
                    .unwrap_or(Value::Null);
                respond(req, 200, &reply);
                continue;
            }
            let method = path.trim_start_matches('/');
            if method == "health" {
                respond(req, 200, &health(&started_at));
                continue;
            }
            if !matches!(method, "status" | "search" | "files" | "lookup") {
                respond(req, 404, &json!({"ok": false, "error": "not found"}));
                continue;
            }
            let _slot = (method != "status").then(|| engine.query_slot());
            let mut span = usage::span("unumsearch", "route", super::known_method(method));
            match api::call_scoped(&engine, method, &query_params(&url), api::Scope::Confined) {
                Ok(v) => respond(req, 200, &v),
                Err(e) => {
                    span.fail();
                    respond(req, 400, &json!({"ok": false, "error": e}))
                }
            }
            drop(span);
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_query() {
        let v = query_params("/search?pattern=a%2Bb+c&glob=*.rs&glob=!x");
        assert_eq!(v["pattern"], "a+b c");
        assert_eq!(v["glob"], "*.rs,!x");
    }
    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_399), "2000-02-28T23:59:59Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_794_411_540), "2026-11-11T15:39:00Z");
    }
    #[test]
    fn health_has_the_shared_fields() {
        let h = health("2026-11-11T15:39:00Z");
        assert_eq!(h["ok"], true);
        assert_eq!(h["name"], "unumsearch");
        assert_eq!(h["version"], env!("CARGO_PKG_VERSION"));
        assert!(!h["git_sha"].as_str().unwrap().is_empty());
        assert_eq!(h["started_at"], "2026-11-11T15:39:00Z");
    }
}
