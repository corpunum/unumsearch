#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Cold page cache: rg vs the one-shot unumsearch CLI, over every configured root.

Before every timed call the corpus files (`rg --files` over the same corpus
definition) and the index files are evicted from the page cache with
posix_fadvise(DONTNEED) (no root needed). Use a PRIVATE index (--index-dir) so a
running daemon's index is not disturbed. A/B order alternates per query and repeat.

  cold_bench.py --index-dir DIR --out FILE.json [--repeat 3] [--config FILE] [--rg rg] [--bin unumsearch]
"""
import argparse, json, os, subprocess, time
from common import cli_prefix, default_config, ignore_file, roots

HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("--index-dir", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--repeat", type=int, default=3)
ap.add_argument("--config", default=default_config())
ap.add_argument("--rg", default="rg")
ap.add_argument("--bin", default="unumsearch")
ap.add_argument("--queries", default=os.path.join(HERE, "queries-agent.json"))
a = ap.parse_args()
G = cli_prefix(a.bin, a.config, a.index_dir)
ROOTS = roots(a.config)
IGN = ignore_file(G, os.path.join(os.path.dirname(os.path.abspath(a.out)), "excludes.ignore"))
COLD = ("createServer", "fn\\s+main\\s*\\(", "class \\w+(Error|Exception)", "<title>", "process\\.env\\.[A-Z_]+")
QUERIES = [q for q in json.load(open(a.queries)) if q[1] in COLD]

corpus = subprocess.run([a.rg, "--files", "--hidden", "--max-filesize", "1M", "--ignore-file", IGN] + ROOTS,
                        capture_output=True, text=True).stdout.split("\n")
idx = [os.path.join(d, f) for d, _, fs in os.walk(a.index_dir) for f in fs]

def evict():
    t = time.perf_counter()
    for p in corpus + idx:
        if not p: continue
        try:
            fd = os.open(p, os.O_RDONLY)
            try: os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally: os.close(fd)
        except OSError: pass
    return time.perf_counter() - t

def rg(q, g):
    cmd = [a.rg, "-l", "-i", "--hidden", "--max-filesize", "1M", "--ignore-file", IGN] + (["-g", g] if g else [])
    return sorted(subprocess.run(cmd + ["-e", q] + ROOTS, capture_output=True, text=True).stdout.split("\n")[:-1])

def cli(q, g):
    cmd = G + ["search", "-l", "-i", "--max-files", "1000000"] + (["-g", g] if g else []) + ["--all-roots", q]
    return sorted(json.loads(subprocess.run(cmd, capture_output=True, text=True).stdout)["result"]["files"])

out = {"corpus_files": len([c for c in corpus if c]), "index_files": len(idx), "repeats": []}
for rep in range(a.repeat):
    rows = []
    for i, (_, q, g) in enumerate(QUERIES):
        order = [("rg", rg), ("cli", cli)]
        if (i + rep) % 2: order.reverse()
        row = {"pattern": q, "glob": g}
        for name, fn in order:
            row[name + "_evict_s"] = round(evict(), 2)
            t = time.perf_counter(); files = fn(q, g); row[name + "_cold_ms"] = round((time.perf_counter() - t) * 1000, 1)
            t = time.perf_counter(); fn(q, g); row[name + "_warm_after_ms"] = round((time.perf_counter() - t) * 1000, 1)
            row[name + "_files"] = len(files); row[name + "_set"] = files
        row["equal"] = row.pop("rg_set") == row.pop("cli_set")
        rows.append(row); print(json.dumps(row), flush=True)
    out["repeats"].append(rows)
    json.dump(out, open(a.out, "w"), indent=1)
