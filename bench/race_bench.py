#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Interleaved race: ripgrep vs the unumsearch daemon (HTTP) vs the one-shot CLI.

Same corpus for every engine: the roots and excludes of the unumsearch config
(`unumsearch excludes` becomes rg's --ignore-file), rg runs with -l -i --hidden
and the same 1 MB size cap. Every individual timing is kept; inside each run the
order rg / http / cli alternates, and the whole pass is repeated.

  race_bench.py --set repo|tree --runs N --repeat K --out FILE.json
                [--config FILE] [--index-dir DIR] [--url URL] [--rg rg] [--bin unumsearch]
                [--repo DIR] [--queries queries-agent.json] [--pid DAEMON_PID]

  repo: the queries rooted at one repository (--repo; default: the first configured root)
  tree: the same patterns over every configured root (all_roots=1, --all-roots)
  --pid: sample the daemon's resident memory (Linux /proc) and record its peak.

Read-only against the daemon; never restarts it. Do not point the tree set at a
daemon with a tight memory cap you cannot afford to lose.
"""
import argparse, json, os, platform, statistics, subprocess, threading, time, urllib.parse, urllib.request
from common import cli_prefix, default_config, ignore_file, proc_status, roots

HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("--set", choices=["repo", "tree"], required=True)
ap.add_argument("--runs", type=int, default=3)
ap.add_argument("--repeat", type=int, default=3)
ap.add_argument("--out", required=True)
ap.add_argument("--config", default=default_config())
ap.add_argument("--index-dir", help="private index for the CLI (default: the config's)")
ap.add_argument("--url", default="http://127.0.0.1:7781")
ap.add_argument("--rg", default="rg")
ap.add_argument("--bin", default="unumsearch")
ap.add_argument("--repo")
ap.add_argument("--queries", default=os.path.join(HERE, "queries-agent.json"))
ap.add_argument("--pid", type=int)
a = ap.parse_args()
G = cli_prefix(a.bin, a.config, a.index_dir)
ROOTS = roots(a.config)
REPO = os.path.normpath(os.path.expanduser(a.repo)) if a.repo else ROOTS[0]
IGN = ignore_file(G, os.path.join(os.path.dirname(os.path.abspath(a.out)), "excludes.ignore"))
status = json.load(urllib.request.urlopen(f"{a.url}/status"))

peak = {}
def sample():
    while a.pid and not done:
        for k, v in proc_status(a.pid).items():
            peak[k] = max(peak.get(k, 0), v)
        time.sleep(0.005)
done = False
threading.Thread(target=sample, daemon=True).start()

def rg(q, g):
    cmd = [a.rg, "-l", "-i", "--hidden", "--max-filesize", "1M", "--ignore-file", IGN] + (["-g", g] if g else [])
    return sorted(subprocess.run(cmd + ["-e", q] + ([REPO] if a.set == "repo" else ROOTS), capture_output=True, text=True).stdout.split("\n")[:-1]), {}

def http(q, g):
    p = {"pattern": q, "mode": "regex", "ci": "1", "files_only": "1", "max_files": "1000000"}
    if a.set == "repo": p["root"] = REPO
    else: p["all_roots"] = "1"
    if g: p["glob"] = g
    with urllib.request.urlopen(f"{a.url}/search?{urllib.parse.urlencode(p)}") as r:
        res = json.load(r)["result"]
    return sorted(res["files"]), {k: res.get(k) for k in ("fresh", "complete", "truncated", "candidates")}

def cli(q, g):
    cmd = G + ["search", "-l", "-i", "--max-files", "1000000"] + (["-g", g] if g else [])
    cmd += ["--all-roots", q] if a.set == "tree" else [q, REPO]
    res = json.loads(subprocess.run(cmd, capture_output=True, text=True).stdout)["result"]
    return sorted(res["files"]), {"fresh": res.get("fresh")}

def timed(fn, *x):
    t = time.perf_counter(); out = fn(*x); return (time.perf_counter() - t) * 1000, out

queries = json.load(open(a.queries))
doc = {"set": a.set, "runs": a.runs, "repeat": a.repeat,
       "rg": subprocess.run([a.rg, "--version"], capture_output=True, text=True).stdout.splitlines()[0],
       "unumsearch": subprocess.run([a.bin, "--version"], capture_output=True, text=True).stdout.strip(),
       "host": platform.platform(), "cpus": os.cpu_count(), "url": a.url,
       "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
       "index_files": status["files"], "index_bytes": status["index_bytes"], "content_bytes": status["content_bytes"],
       "units": len(status["units"]), "roots": len(ROOTS) if a.set == "tree" else 1, "repeats": []}
for rep in range(a.repeat):
    rows = []
    for _, q, g in queries:
        row = {"pattern": q, "glob": g, "rg_ms": [], "http_ms": [], "cli_ms": []}
        for run in range(a.runs):
            order = [("rg", rg), ("http", http), ("cli", cli)]
            if (run + rep) % 2: order.reverse()  # alternate who goes first
            for name, fn in order:
                ms, (files, meta) = timed(fn, q, g)
                row[name + "_ms"].append(round(ms, 3))
                row[name + "_files"] = files
                if meta: row[name + "_meta"] = meta
        r = {"pattern": q, "glob": g, "rg_ms": row["rg_ms"], "http_ms": row["http_ms"], "cli_ms": row["cli_ms"],
             "files": len(row["rg_files"]), "equal_http": row["rg_files"] == row["http_files"],
             "equal_cli": row["rg_files"] == row["cli_files"], "http_meta": row.get("http_meta"),
             # Paths are reported relative to the roots, so a shared result file names no home directory.
             "only_rg": [os.path.relpath(p, os.path.dirname(REPO)) for p in sorted(set(row["rg_files"]) - set(row["http_files"]))[:20]],
             "only_unum": [os.path.relpath(p, os.path.dirname(REPO)) for p in sorted(set(row["http_files"]) - set(row["rg_files"]))[:20]]}
        rows.append(r)
        print(json.dumps({k: r[k] for k in ("pattern", "rg_ms", "http_ms", "cli_ms", "files", "equal_http", "equal_cli")}), flush=True)
    doc["repeats"].append(rows)
    json.dump(doc, open(a.out, "w"), indent=1)
done = True
doc["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
if a.pid:
    doc["daemon_peak_kb"] = peak
    doc["daemon_end_kb"] = proc_status(a.pid)
json.dump(doc, open(a.out, "w"), indent=1)
