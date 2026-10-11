#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Identical-results gate and timing for the verification pass.

Runs every query in bench/verify-queries.json through a baseline binary (the
previous release, e.g. v0.1.7) and a candidate binary, over the same corpus,
and compares the full normalised answer: the files, every match (path, line
number, text), truncation, completeness, coverage and candidate counts. Any
difference exits with status 1.

The corpus is a snapshot of the files the index holds for the given roots
(default ~/main and ~/unumsearch), copied into the work directory and indexed
privately with the baseline binary. A live repo changes under a benchmark; a
snapshot does not. `--live` also runs the queries against the roots
themselves (private index, same binaries), for a second look.

  verify_compare.py --baseline BIN --candidate BIN --workdir DIR [--bench 5]

Timing uses the engine's own `elapsed_ms` from each answer, one private
index, and `--repeats` runs per query after one warm-up run. The report gives
p50 and p95 per query and per query class (regex-heavy vs literal).
"""
import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_ROOTS = ["~/main", "~/unumsearch"]


def run(binary, config, index_dir, args):
    cmd = [binary, "--config", config, "--no-default-config", "--index-dir", index_dir] + args
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        raise SystemExit(f"{cmd}: exit {p.returncode}: {p.stderr[:500]}")
    out = json.loads(p.stdout)
    if not out.get("ok"):
        raise SystemExit(f"{cmd}: not ok: {p.stdout[:500]}")
    return out.get("result", out)


def normalise(res):
    """Everything that defines the answer; timing and memory are excluded."""
    keep = ("files", "matches", "complete", "truncated", "covered", "fresh",
            "backend", "candidates", "uncovered")
    return {k: res.get(k) for k in keep if k in res}


def make_snapshot(baseline, workdir, roots):
    """Copy the files the index holds for `roots` into workdir/snap, index them."""
    snap = os.path.join(workdir, "snap")
    live_index = os.path.join(workdir, "live-index")
    os.makedirs(live_index, exist_ok=True)
    cfg = os.path.join(workdir, "live.toml")
    write_config(cfg, roots, live_index)
    files = []
    for r in roots:
        listing = run(baseline, cfg, live_index, ["files", r, "--max", "1000000"])
        files += listing["files"]
    if os.path.isdir(snap):
        shutil.rmtree(snap)
    snap_roots = []
    for r in roots:
        root = os.path.realpath(os.path.expanduser(r))
        dest_root = os.path.join(snap, os.path.basename(root))
        snap_roots.append(dest_root)
        for f in files:
            if f == root or f.startswith(root + os.sep):
                rel = os.path.relpath(f, root)
                dst = os.path.join(dest_root, rel)
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copy2(f, dst)
    snap_index = os.path.join(workdir, "snap-index")
    shutil.rmtree(snap_index, ignore_errors=True)
    os.makedirs(snap_index)
    snap_cfg = os.path.join(workdir, "snap.toml")
    write_config(snap_cfg, snap_roots, snap_index)
    run(baseline, snap_cfg, snap_index, ["index", "--force"])
    return snap_cfg, snap_index, snap_roots, len(files)


def write_config(path, roots, index_dir):
    with open(path, "w") as f:
        f.write("roots = [" + ", ".join(f'"{os.path.realpath(os.path.expanduser(r))}"' for r in roots) + "]\n")
        f.write("threads = 8\n")
        f.write(f'index_dir = "{index_dir}"\n')


def query_args(q, roots_map, root_key):
    args = ["search"]
    if q.get("literal"):
        args.append("-F")
    if q.get("ci"):
        args.append("-i")
    if q.get("files_only"):
        args.append("-l")
    if "max" in q:
        args += ["-m", str(q["max"])]
    args += ["--", q["pattern"], roots_map[root_key]]
    return args


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = max(0, min(len(xs) - 1, int(round(p / 100.0 * len(xs) + 0.5)) - 1))
    return xs[k]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--baseline", required=True)
    ap.add_argument("--candidate", required=True)
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--roots", nargs="*", default=DEFAULT_ROOTS)
    ap.add_argument("--queries", default=os.path.join(HERE, "verify-queries.json"))
    ap.add_argument("--bench", type=int, default=0, help="timed repeats per query (0 = compare only)")
    ap.add_argument("--live", action="store_true", help="also compare on the live roots")
    ap.add_argument("--report", default="", help="write the timing report (markdown) here")
    a = ap.parse_args()

    os.makedirs(a.workdir, exist_ok=True)
    with open(a.queries) as f:
        queries = json.load(f)
    snap_cfg, snap_index, snap_roots, nfiles = make_snapshot(a.baseline, a.workdir, a.roots)
    print(f"snapshot: {nfiles} indexed files in {len(snap_roots)} roots", flush=True)
    sets = [("snapshot", snap_cfg, snap_index, {"main": snap_roots[0], "unumsearch": snap_roots[-1]})]
    if a.live:
        live_index = os.path.join(a.workdir, "live-index")
        live_cfg = os.path.join(a.workdir, "live.toml")
        live_roots = [os.path.realpath(os.path.expanduser(r)) for r in a.roots]
        sets.append(("live", live_cfg, live_index, {"main": live_roots[0], "unumsearch": live_roots[-1]}))

    diffs = []
    checked = 0
    total_matches = 0
    total_files = 0
    timing = []  # (name, class, base_ms list, cand_ms list)
    for name, cfg, idx, roots_map in sets:
        for q in queries:
            for root_key in q.get("roots", ["main"]):
                args = query_args(q, roots_map, root_key)
                base = run(a.baseline, cfg, idx, args)
                cand = run(a.candidate, cfg, idx, args)
                checked += 1
                nb, nc = normalise(base), normalise(cand)
                total_files += len(base.get("files") or [])
                total_matches += len(base.get("matches") or [])
                if nb != nc:
                    diffs.append((name, q["id"], root_key, args))
                    print(f"DIFF {name} {q['id']} {root_key} {args}", flush=True)
                if a.bench and name == "snapshot":
                    # Interleaved, so both binaries see the same background load.
                    run_base, run_cand = [], []
                    for _ in range(a.bench):
                        run_base.append(run(a.baseline, cfg, idx, args)["elapsed_ms"])
                        run_cand.append(run(a.candidate, cfg, idx, args)["elapsed_ms"])
                    timing.append((q["id"], q.get("class", "literal"), run_base, run_cand, len(base["files"]), len(base["matches"]) if isinstance(base.get("matches"), list) else 0))
    print(f"compared {checked} answers over {len(queries)} queries, {len(diffs)} differences "
          f"({total_files} files and {total_matches} matches in the baseline answers)", flush=True)

    if a.bench:
        lines = ["| query | class | files | matches | base p50 | base p95 | new p50 | new p95 | speedup (p50) |",
                 "|---|---|---|---|---|---|---|---|---|"]
        classes = {}
        for qid, cls, b, c, nfl, nmt in timing:
            classes.setdefault(cls, ([], []))
            classes[cls][0].extend(b)
            classes[cls][1].extend(c)
            bp50, cp50 = pct(b, 50), pct(c, 50)
            lines.append(f"| {qid} | {cls} | {nfl} | {nmt} | {bp50:.2f} | {pct(b, 95):.2f} | {cp50:.2f} | {pct(c, 95):.2f} | {bp50 / cp50 if cp50 else float('nan'):.2f}x |")
        lines += ["", "| class | samples | base p50 | base p95 | new p50 | new p95 | p50 change |", "|---|---|---|---|---|---|---|"]
        for cls, (b, c) in sorted(classes.items()):
            bp50, cp50 = pct(b, 50), pct(c, 50)
            lines.append(f"| {cls} | {len(b)} | {bp50:.2f} | {pct(b, 95):.2f} | {cp50:.2f} | {pct(c, 95):.2f} | {100 * (cp50 - bp50) / bp50:+.1f}% |")
        text = "\n".join(lines) + "\n"
        print(text)
        if a.report:
            with open(a.report, "w") as f:
                f.write(text)
    if diffs:
        sys.exit(1)


if __name__ == "__main__":
    main()
