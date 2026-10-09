#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Summarise a run directory (raw-repo.json, raw-tree.json, raw-cold.json, build-N.*) into
summary.json: p50/p95/max over the per-query medians, per repeat, plus the spread across
repeats (min / median / max), cold-cache medians, build time and index size.

  summarize.py RUN_DIR
"""
import json, os, re, statistics, sys

D = sys.argv[1] if len(sys.argv) > 1 else "."

def pct(xs, p):
    xs = sorted(xs); k = (len(xs) - 1) * p; f = int(k); c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)

out = {}
for s in ("repo", "tree"):
    path = os.path.join(D, f"raw-{s}.json")
    if not os.path.exists(path): continue
    d = json.load(open(path))
    reps = []
    for rows in d["repeats"]:
        r = {}
        for k in ("rg_ms", "http_ms", "cli_ms"):
            meds = [statistics.median(x[k]) for x in rows]
            r[k] = {"p50": pct(meds, .5), "p95": pct(meds, .95), "max": max(meds),
                    "worst_single_call": max(max(x[k]) for x in rows), "sum_of_medians": sum(meds)}
        r["equal_http"] = sum(x["equal_http"] for x in rows); r["equal_cli"] = sum(x["equal_cli"] for x in rows)
        r["queries"] = len(rows)
        r["speedup_per_query_median"] = statistics.median(statistics.median(x["rg_ms"]) / statistics.median(x["http_ms"]) for x in rows)
        r["rg_faster_than_http"] = sum(statistics.median(x["rg_ms"]) < statistics.median(x["http_ms"]) for x in rows)
        r["rg_faster_than_cli"] = sum(statistics.median(x["rg_ms"]) < statistics.median(x["cli_ms"]) for x in rows)
        reps.append(r)
    spread = {}
    for k in ("rg_ms", "http_ms", "cli_ms"):
        spread[k] = {m: [round(min(r[k][m] for r in reps), 1), round(statistics.median(r[k][m] for r in reps), 1),
                         round(max(r[k][m] for r in reps), 1)] for m in ("p50", "p95", "max", "worst_single_call", "sum_of_medians")}
    meta = {k: d.get(k) for k in ("rg", "unumsearch", "index_files", "index_bytes", "content_bytes", "units", "started", "finished", "runs")}
    out[s] = {"meta": meta, "per_repeat": reps, "spread_min_median_max": spread,
              "equal_http": [r["equal_http"] for r in reps], "equal_cli": [r["equal_cli"] for r in reps],
              "speedup_per_query_median": [round(r["speedup_per_query_median"], 1) for r in reps],
              "rg_faster_than_http": [r["rg_faster_than_http"] for r in reps],
              "rg_faster_than_cli": [r["rg_faster_than_cli"] for r in reps],
              "daemon_peak_kb": d.get("daemon_peak_kb"), "daemon_end_kb": d.get("daemon_end_kb")}

cold = os.path.join(D, "raw-cold.json")
if os.path.exists(cold):
    c = json.load(open(cold)); rows = [x for rep in c["repeats"] for x in rep]
    out["cold"] = {"corpus_files": c["corpus_files"], "calls": len(rows),
                   "rg_cold_ms": {"median": statistics.median(x["rg_cold_ms"] for x in rows), "min": min(x["rg_cold_ms"] for x in rows), "max": max(x["rg_cold_ms"] for x in rows)},
                   "cli_cold_ms": {"median": statistics.median(x["cli_cold_ms"] for x in rows), "min": min(x["cli_cold_ms"] for x in rows), "max": max(x["cli_cold_ms"] for x in rows)},
                   "rg_warm_after_ms_median": statistics.median(x["rg_warm_after_ms"] for x in rows),
                   "cli_warm_after_ms_median": statistics.median(x["cli_warm_after_ms"] for x in rows),
                   "equal": sum(x["equal"] for x in rows)}
builds = []
for i in (1, 2):
    t = os.path.join(D, f"build-{i}.time")
    if not os.path.exists(t): continue
    txt = open(t).read()
    el = re.search(r"Elapsed \(wall clock\) time.*: (\S+)", txt).group(1)
    parts = [float(p) for p in el.split(":")]; secs = sum(p * 60 ** i for i, p in enumerate(reversed(parts)))
    rss = int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", txt).group(1))
    du = int(open(os.path.join(D, f"build-{i}.du")).read().split()[0])
    builds.append({"wall_s": secs, "max_rss_mb": rss // 1024, "index_bytes": du})
out["builds"] = builds
json.dump(out, open(os.path.join(D, "summary.json"), "w"), indent=1)
print(json.dumps(out, indent=1))
