#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Latency and correctness of a unumsearch daemon while a git checkout rewrites the tree.

What it does (Python standard library only, Linux or macOS):
  1. checks out --from in --repo (the work tree must be clean: use a scratch clone, the script
     moves HEAD and puts it back at the end),
  2. builds a PRIVATE index of that one repository and serves it from a PRIVATE daemon on
     --port (its own config, index dir and port; a running daemon is never touched),
  3. checks that the daemon and rg return the same files for every query on the idle tree
     (rg gets `unumsearch excludes` as --ignore-file, --hidden and the same size cap, so a
     mismatch here means the two corpora differ, not that the index is stale),
  4. measures steady-state latency,
  5. switches to --to and back to --from while one client sends the queries back to back
     (from --lead seconds before the checkout until --tail seconds after the rebuilt index is
     live), recording every answer's latency and its fresh/complete flags,
  6. compares every answer that started after the checkout finished and was marked
     fresh+complete with rg on the new tree. Answers marked stale are allowed to differ;
     that is the point of the flag.

Per switch: files changed, checkout time, the rebuild window (checkout start until the rebuilt
index is live and fresh), how long answers were marked stale (up to 2,000 changed files the
daemon verifies the changed files directly and answers stay fresh; past that, or for changes it
cannot list, answers say fresh: false until the rebuild lands), p50/p95/max latency inside the
window, and fresh+complete answers checked against rg. Also: steady p50/p95, initial index time,
daemon peak RSS (VmHWM, Linux).

Output: a JSON summary on stdout (and in --out). Example:

  git clone --depth 1 --branch v6.6 https://github.com/torvalds/linux.git /tmp/linux
  git -C /tmp/linux fetch --depth 1 origin tag v6.1
  python3 bench/branch_switch.py --repo /tmp/linux --from v6.6 --to v6.1 \
      --queries bench/queries-kernel.json --port 7799 --out linux-v6.6-v6.1.json
"""
import argparse, hashlib, json, os, shutil, signal, subprocess, sys, tempfile, threading, time
import urllib.parse, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))


def sh(*cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def git(repo, *args, check=True):
    r = sh("git", "-C", repo, *args)
    if check and r.returncode:
        sys.exit(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))] if xs else None


def r1(x):
    return None if x is None else round(x, 1)


def digest(paths):
    return hashlib.sha1("\n".join(sorted(paths)).encode()).hexdigest(), len(paths)


def peak_rss_mb(pid):
    try:
        for line in open(f"/proc/{pid}/status"):
            if line.startswith("VmHWM:"):
                return round(int(line.split()[1]) / 1024, 1)
    except OSError:
        pass
    return None


class Bench:
    def __init__(self, a):
        self.a = a
        self.repo = os.path.realpath(a.repo)
        self.base = f"http://127.0.0.1:{a.port}"
        qs = json.load(open(a.queries))
        self.queries = [(q[1], q[2]) for q in qs][: a.max_queries or None]
        self.tmp = tempfile.mkdtemp(prefix="unumsearch-branch-switch-")
        self.cfg = os.path.join(self.tmp, "config.toml")
        with open(self.cfg, "w") as f:
            f.write(f"roots = [{json.dumps(self.repo)}]\n"
                    f"index_dir = {json.dumps(os.path.join(self.tmp, 'index'))}\n"
                    f"excludes = {json.dumps(a.exclude)}\n"
                    f"max_file_size = {a.max_file_size}\n"
                    f"listen = \"127.0.0.1:{a.port}\"\n")
        self.ignore = os.path.join(self.tmp, "rg-ignore")
        self.proc = None

    # --- daemon -------------------------------------------------------------------------------
    def start(self):
        env = dict(os.environ, MALLOC_ARENA_MAX=os.environ.get("MALLOC_ARENA_MAX", "2"))
        self.log = open(os.path.join(self.tmp, "serve.log"), "w")
        t0 = time.time()
        self.proc = subprocess.Popen([self.a.bin, "--config", self.cfg, "--no-default-config", "serve"],
                                     stdout=self.log, stderr=subprocess.STDOUT, env=env)
        while time.time() - t0 < self.a.timeout:
            if self.proc.poll() is not None:
                sys.exit(f"daemon exited, see {self.log.name}")
            try:
                if self.status().get("fresh"):
                    return time.time() - t0
            except OSError:
                pass
            time.sleep(0.2)
        sys.exit("index never became fresh")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)  # our own child, by PID
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def status(self):
        with urllib.request.urlopen(self.base + "/status", timeout=10) as r:
            return json.load(r)

    def ask(self, pat, glob):
        p = {"root": self.repo, "pattern": pat, "mode": "regex", "files_only": "1", "max_files": "10000000"}
        if glob:
            p["glob"] = glob
        t = time.perf_counter()
        with urllib.request.urlopen(self.base + "/search?" + urllib.parse.urlencode(p), timeout=120) as r:
            body = r.read()
        ms = (time.perf_counter() - t) * 1000
        res = json.loads(body)["result"]
        files = [f if isinstance(f, str) else f.get("path") for f in res.get("files", [])]
        return ms, bool(res.get("fresh")), bool(res.get("complete")), digest(files)

    # --- ground truth ---------------------------------------------------------------------------
    def rg(self, pat, glob):
        cmd = [self.a.rg, "--no-config", "-l", "--hidden", "--max-filesize", str(self.a.max_file_size),
               "--ignore-file", self.ignore, "-e", pat]
        if glob:
            cmd += ["-g", glob]
        out = sh(*cmd, "-g", "!.git", self.repo).stdout.split("\n")
        return digest([os.path.abspath(x) for x in out if x])

    def truth(self):
        return [self.rg(p, g) for p, g in self.queries]

    # --- phases ---------------------------------------------------------------------------------
    def parity(self):
        truth = self.truth()
        bad = []
        for i, (p, g) in enumerate(self.queries):
            _, fr, co, d = self.ask(p, g)
            if not (fr and co and d == truth[i]):
                bad.append({"pattern": p, "glob": g, "unumsearch_files": d[1], "rg_files": truth[i][1]})
        return {"queries": len(self.queries), "same_files": len(self.queries) - len(bad), "differ": bad,
                "rg_files_per_query": [t[1] for t in truth]}

    def steady(self):
        lat = [self.ask(p, g)[0] for _ in range(self.a.steady_runs) for p, g in self.queries]
        return {"answers": len(lat), "p50_ms": r1(pct(lat, 50)), "p95_ms": r1(pct(lat, 95)), "max_ms": r1(max(lat))}

    def switch(self, target, label):
        rec, stop, errors = [], threading.Event(), []

        def client():
            i = 0
            while not stop.is_set():
                q = i % len(self.queries)
                t0 = time.time()
                try:
                    ms, fr, co, d = self.ask(*self.queries[q])
                except Exception as e:  # keep going; count it
                    errors.append(repr(e))
                    continue
                rec.append((t0, ms, fr, co, q, d))
                i += 1

        def fresh_after(t_ms):
            st = self.status()
            return st.get("fresh") and all(u.get("indexed_at_ms", 0) >= t_ms for u in st.get("units", []))

        before = git(self.repo, "rev-parse", "HEAD").strip()
        th = threading.Thread(target=client, daemon=True)
        th.start()
        time.sleep(self.a.lead)
        cs = time.time()
        r = sh("git", "-C", self.repo, "checkout", "-q", "--detach", target)
        ce = time.time()
        if r.returncode:
            stop.set()
            sys.exit(f"checkout {target}: {r.stderr}")
        fa = None
        while time.time() - ce < self.a.timeout:
            if fresh_after(int(cs * 1000)):
                fa = time.time()
                break
            time.sleep(0.05)
        time.sleep(self.a.tail)
        stop.set()
        th.join()
        truth = self.truth()
        after = git(self.repo, "rev-parse", "HEAD").strip()
        changed = git(self.repo, "diff", "--no-renames", "--name-only", before, after).count("\n")
        end = fa or ce
        window = [x for x in rec if cs <= x[0] <= end]
        post_co = [x for x in rec if x[0] > ce]
        checked = [x for x in post_co if x[2] and x[3]]
        wrong = [x for x in checked if x[5] != truth[x[4]]]
        steady_after = [x for x in rec if fa and x[0] > fa]
        wl = [x[1] for x in window]
        stale = [x for x in rec if x[0] >= cs and not x[2]]
        return {
            "label": label, "from": before[:12], "to": after[:12], "files_changed": changed,
            "checkout_s": round(ce - cs, 2),
            "rebuild_window_s": round(fa - cs, 2) if fa else None,
            "rebuilt_after_checkout_s": round(fa - ce, 2) if fa else None,
            "window_answers": len(window), "window_marked_stale": sum(1 for x in window if not x[2]),
            "marked_stale_span_s": round(stale[-1][0] + stale[-1][1] / 1000 - stale[0][0], 2) if stale else 0,
            "window_p50_ms": r1(pct(wl, 50)), "window_p95_ms": r1(pct(wl, 95)),
            "window_max_ms": r1(max(wl)) if wl else None,
            "answers_after_checkout": len(post_co),
            "fresh_complete_checked_vs_rg": len(checked), "fresh_complete_wrong": len(wrong),
            "wrong_queries": sorted({self.queries[x[4]][0] for x in wrong}),
            "after_fresh_p50_ms": r1(pct([x[1] for x in steady_after], 50)),
            "client_errors": len(errors),
        }

    def run(self):
        a = self.a
        if git(self.repo, "status", "--porcelain", "--untracked-files=no").strip():
            sys.exit("work tree has changes; run this on a scratch clone")
        orig = git(self.repo, "rev-parse", "HEAD").strip()
        git(self.repo, "checkout", "-q", "--detach", a.from_ref)
        out = {"repo": self.repo, "from": a.from_ref, "to": a.to_ref,
               "tracked_files": git(self.repo, "ls-files").count("\n"),
               "unumsearch": sh(a.bin, "--version").stdout.strip(),
               "rg": sh(a.rg, "--version").stdout.split("\n")[0], "queries": len(self.queries)}
        try:
            out["initial_index_s"] = round(self.start(), 1)
            st = self.status()
            out["indexed_files"], out["index_bytes"] = st.get("files"), st.get("index_bytes")
            with open(self.ignore, "w") as f:
                f.write(sh(a.bin, "--config", self.cfg, "--no-default-config", "excludes").stdout)
            out["parity_before"] = self.parity()
            out["steady"] = self.steady()
            out["switches"] = [self.switch(a.to_ref, f"{a.from_ref} -> {a.to_ref}")]
            out["switches"].append(self.switch(a.from_ref, f"{a.to_ref} -> {a.from_ref}"))
            out["daemon_peak_rss_mb"] = peak_rss_mb(self.proc.pid)
        finally:
            self.stop()
            git(self.repo, "checkout", "-q", orig, check=False)
            if not a.keep:
                shutil.rmtree(self.tmp, ignore_errors=True)
        return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--repo", required=True, help="a scratch git clone (HEAD is moved, then restored)")
    ap.add_argument("--from", dest="from_ref", required=True, help="ref checked out before indexing")
    ap.add_argument("--to", dest="to_ref", required=True, help="ref to switch to (and back from)")
    ap.add_argument("--queries", default=os.path.join(HERE, "queries-agent.json"),
                    help="JSON list of [label, regex, glob-or-null]")
    ap.add_argument("--max-queries", type=int, default=0, help="use only the first N queries")
    ap.add_argument("--port", type=int, default=7799, help="private daemon port")
    ap.add_argument("--bin", default="unumsearch")
    ap.add_argument("--rg", default="rg")
    ap.add_argument("--exclude", action="append", default=[], help="extra exclude (repeatable)")
    ap.add_argument("--max-file-size", type=int, default=1048576)
    ap.add_argument("--steady-runs", type=int, default=3)
    ap.add_argument("--lead", type=float, default=5, help="seconds of queries before the checkout")
    ap.add_argument("--tail", type=float, default=10, help="seconds of queries after fresh again")
    ap.add_argument("--timeout", type=float, default=900)
    ap.add_argument("--keep", action="store_true", help="keep the private index and log")
    ap.add_argument("--out", help="also write the JSON here")
    a = ap.parse_args()
    out = Bench(a).run()
    text = json.dumps(out, indent=1)
    if a.out:
        with open(a.out, "w") as f:
            f.write(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
