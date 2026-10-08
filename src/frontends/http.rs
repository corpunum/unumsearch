// SPDX-License-Identifier: Apache-2.0
//! Local HTTP/JSON API: GET /status, /search, /files; POST /rpc (JSON-RPC 2.0).

use serde_json::{json, Map, Value};
use std::sync::Arc;
use unumsearch::{api, Engine};

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

pub fn serve(engine: Arc<Engine>) {
    let addr = engine.cfg.listen.clone();
    let server = match tiny_http::Server::http(&addr) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("unumsearch: cannot listen on {addr}: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("unumsearch: listening on http://{addr}");
    let mut handles = Vec::new();
    for _ in 0..4 {
        let server = server.clone();
        let engine = engine.clone();
        handles.push(std::thread::spawn(move || loop {
            let Ok(mut req) = server.recv() else { break };
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or("").to_string();
            if path == "/rpc" {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let reply = super::rpc::handle_line(&engine, &body).unwrap_or(Value::Null);
                respond(req, 200, &reply);
                continue;
            }
            let method = path.trim_start_matches('/');
            if !matches!(method, "status" | "search" | "files") {
                respond(req, 404, &json!({"ok": false, "error": "not found"}));
                continue;
            }
            match api::call(&engine, method, &query_params(&url)) {
                Ok(v) => respond(req, 200, &v),
                Err(e) => respond(req, 400, &json!({"ok": false, "error": e})),
            }
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
}
