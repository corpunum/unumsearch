#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Compare unumsearch with ripgrep on your own corpus.

Queries file: JSON list of [root, pattern, glob-or-null]; patterns are regexes,
matched case-insensitively (as many agent tools do). For each query:
  * ripgrep `-l` over the same corpus definition (`unumsearch excludes` is
    passed as --ignore-file, plus --hidden and the same size cap),
  * unumsearch through the daemon's HTTP API, and through the CLI,
median of N runs each, and whether the matching file sets are identical.

Usage: bench.py queries.json [--runs 5] [--url http://127.0.0.1:7781]
                             [--rg rg] [--bin unumsearch] [--max-filesize 1M]
"""
import argparse, json, statistics, subprocess, tempfile, time, urllib.parse, urllib.request

ap = argparse.ArgumentParser()
ap.add_argument("queries")
ap.add_argument("--runs", type=int, default=5)
ap.add_argument("--url", default="http://127.0.0.1:7781")
ap.add_argument("--rg", default="rg")
ap.add_argument("--bin", default="unumsearch")
ap.add_argument("--max-filesize", default="1M")
ap.add_argument("--skip-rg", action="store_true", help="do not run ripgrep (timing of unumsearch only; no file-set comparison)")
a = ap.parse_args()

ignore = tempfile.NamedTemporaryFile("w", suffix=".ignore", delete=False)
ignore.write(subprocess.run([a.bin, "excludes"], capture_output=True, text=True, check=True).stdout)
ignore.close()

def med(fn):
    ts, out = [], None
    for _ in range(a.runs):
        t = time.perf_counter(); out = fn(); ts.append((time.perf_counter() - t) * 1000)
    return statistics.median(ts), out

def rg(root, q, g):
    cmd = [a.rg, "-l", "-i", "--hidden", "--max-filesize", a.max_filesize, "--ignore-file", ignore.name]
    if g: cmd += ["-g", g]
    return sorted(subprocess.run(cmd + ["-e", q, root], capture_output=True, text=True).stdout.split())

def http(root, q, g):
    p = {"root": root, "pattern": q, "mode": "regex", "ci": "1", "files_only": "1", "max_files": "1000000"}
    if g: p["glob"] = g
    with urllib.request.urlopen(f"{a.url}/search?{urllib.parse.urlencode(p)}") as r:
        return sorted(json.load(r)["result"]["files"])

def cli(root, q, g):
    cmd = [a.bin, "search", "-l", "-i", "--max-files", "1000000"] + (["-g", g] if g else []) + [q, root]
    return sorted(json.loads(subprocess.run(cmd, capture_output=True, text=True).stdout)["result"]["files"])

def pct(xs, p):
    xs = sorted(xs); k = (len(xs) - 1) * p; f = int(k); c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)

rows = []
for root, q, g in json.load(open(a.queries)):
    r_ms, r_files = (0.0, None) if a.skip_rg else med(lambda: rg(root, q, g))
    h_ms, h_files = med(lambda: http(root, q, g))
    c_ms, _ = med(lambda: cli(root, q, g))
    rows.append({"pattern": q, "rg_ms": r_ms, "http_ms": h_ms, "cli_ms": c_ms, "files": len(h_files) if a.skip_rg else len(r_files), "equal": True if a.skip_rg else r_files == h_files})
    print(json.dumps(rows[-1]))
for k in ("rg_ms", "http_ms", "cli_ms"):
    xs = [r[k] for r in rows]
    print(f"{k}: p50 {pct(xs, .5):.1f}  p95 {pct(xs, .95):.1f}  max {max(xs):.1f}")
print(f"identical file sets: {sum(r['equal'] for r in rows)}/{len(rows)}")
