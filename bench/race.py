#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Live terminal race: the same queries through ripgrep, unumsearch and (optionally) ugrep.

Every query really runs; nothing is replayed or animated. Engines run one at a time, never in
parallel, interleaved per query (rg q1, unumsearch q1, ugrep q1, rg q2, ...). While an engine is
running, its timer and bar grow; they stop when its process (or HTTP request) returns. Redraws
are throttled to about 30 per second. Python standard library only.

What is timed, per query and engine:
  rg         wall time of the `rg -l` process, over the same corpus rules as the index
             (`unumsearch excludes` as --ignore-file, --hidden, --max-filesize 1M)
  unumsearch wall time of one HTTP request to a running daemon (index already built), or with
             --unumsearch-cli the wall time of a `unumsearch search -l` process
  ugrep      wall time of the `ugrep -rl` process with the same exclude file, .gitignore, hidden
             files, binary files and .git skipped (ugrep has no size cap, so files over 1 MB can differ)

    python3 bench/race.py --root .                       # this repository, built-in queries
    python3 bench/race.py --root ~/src --queries q.txt   # one regex per line
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, textwrap, threading, time, urllib.parse, urllib.request

DEFAULT_QUERIES = [
    r"TODO|FIXME", r"fn\s+main\s*\(", r"async function \w+", r"process\.env\.[A-Z_]+",
    r"class \w+(Error|Exception)", r"throw new Error\(", r"createServer", r"unsafe\s*\{",
    r"def \w+\(self", r"SELECT .* FROM", r"console\.(log|error)\(", r"license",
]

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("--root", default=".", help="directory to search (must be inside the daemon's roots)")
ap.add_argument("--queries", help="file with one regex per line (default: a built-in agent-style list)")
ap.add_argument("--url", default="http://127.0.0.1:7781", help="unumsearch daemon")
ap.add_argument("--unumsearch-bin", default="unumsearch")
ap.add_argument("--unumsearch-cli", action="store_true", help="time `unumsearch search` processes instead of the daemon")
ap.add_argument("--rg", default="rg")
ap.add_argument("--ugrep", default="ugrep", help="ugrep binary; skipped if not found (--no-ugrep to skip)")
ap.add_argument("--no-ugrep", action="store_true")
ap.add_argument("-i", "--ignore-case", action="store_true")
ap.add_argument("--label", default="", help="text for the header, e.g. the machine")
a = ap.parse_args()

ROOT = os.path.abspath(os.path.expanduser(a.root))
QUERIES = DEFAULT_QUERIES
if a.queries:
    QUERIES = [l.rstrip("\n") for l in open(a.queries) if l.strip() and not l.startswith("#")]

def version(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True).stdout.splitlines()[0].strip()
    except (OSError, IndexError):
        return None

UNUM = version([a.unumsearch_bin, "--version"])
RG = version([a.rg, "--version"])
if not UNUM or not RG:
    sys.exit("need both `rg` and `unumsearch` on PATH (or --rg / --unumsearch-bin)")
UG = None if a.no_ugrep or not shutil.which(a.ugrep) else version([a.ugrep, "--version"])
ign = tempfile.NamedTemporaryFile("w", suffix=".ignore", delete=False)
ign.write(subprocess.run([a.unumsearch_bin, "excludes"], capture_output=True, text=True, check=True).stdout); ign.close()
CI = ["-i"] if a.ignore_case else []

def files_of(out, base=None):
    fs = set()
    for line in out.splitlines():
        if line:
            p = line if os.path.isabs(line) else os.path.join(base or ROOT, line)
            fs.add(os.path.normpath(p))
    return fs

def run_proc(cmd):
    return files_of(subprocess.run(cmd, capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout)

def rg(q):
    return run_proc([a.rg, "-l", "--hidden", "--ignore-file", ign.name, "--max-filesize", "1M"] + CI + ["-e", q, ROOT])

def ugrep(q):
    return run_proc([a.ugrep, "-rl", "-I", "--hidden", "--ignore-files", "--exclude-dir=.git", "--exclude-from=" + ign.name, "--no-messages"] + CI + ["-e", q, ROOT])

def unum_http(q):
    p = {"root": ROOT, "pattern": q, "mode": "regex", "files_only": "1", "max_files": "1000000"}
    if a.ignore_case: p["ci"] = "1"
    with urllib.request.urlopen(f"{a.url}/search?{urllib.parse.urlencode(p)}", timeout=600) as r:
        res = json.load(r)
    if not res.get("ok"):
        raise RuntimeError(res.get("error"))
    return {os.path.normpath(f) for f in res["result"]["files"]}

def unum_cli(q):
    out = subprocess.run([a.unumsearch_bin, "search", "-l", "--max-files", "1000000"] + CI + [q, ROOT],
                         capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout
    return {os.path.normpath(f) for f in json.loads(out)["result"]["files"]}

if not a.unumsearch_cli:
    try:
        st = json.load(urllib.request.urlopen(f"{a.url}/status", timeout=5))
    except OSError as e:
        sys.exit(f"no unumsearch daemon at {a.url} ({e}); start `unumsearch serve`, or pass --unumsearch-cli")
    INDEX_FILES = st.get("files")
    try:
        unum_http(QUERIES[0])
    except Exception as e:
        msg = getattr(e, "read", lambda: b"")().decode(errors="replace") or str(e)
        sys.exit(f"the daemon at {a.url} cannot search {ROOT}: {msg.strip()[:300]}\n"
                 "its configured roots must contain --root (e.g. `unumsearch --root DIR serve`), or pass --unumsearch-cli")
else:
    INDEX_FILES = None

ENGINES = [{"name": "rg", "ver": RG.split(" (")[0], "fn": rg, "how": "rg -l --hidden --ignore-file=<excludes> --max-filesize 1M", "col": "\x1b[38;5;252m"},
           {"name": "unumsearch", "ver": UNUM, "fn": unum_cli if a.unumsearch_cli else unum_http,
            "how": "unumsearch search -l (CLI process, index)" if a.unumsearch_cli else "GET /search files_only=1 (daemon, index)", "col": "\x1b[38;5;78m"}]
if UG:
    ENGINES.append({"name": "ugrep", "ver": UG.split(" x86")[0].split(" aarch")[0], "fn": ugrep, "how": "ugrep -rl -I --hidden --ignore-files --exclude-from=<excludes>", "col": "\x1b[38;5;75m"})
for e in ENGINES:
    e.update(total=0.0, cur=0.0, running=False, done=0, files=0, match=0, err=0)

# ---------------------------------------------------------------- drawing
B, DIM, RST, YEL = "\x1b[1m", "\x1b[38;5;245m", "\x1b[0m", "\x1b[38;5;221m"
COLS = min(shutil.get_terminal_size((100, 30)).columns, 140)
NARROW = COLS < 80  # e.g. a phone-shaped terminal: stacked rows, short labels
BARW = max(10, COLS - 14) if NARROW else max(20, COLS - 46)
qi_now, q_now = 0, ""

def fmt(ms):
    return f"{ms/1000:6.2f} s" if ms >= 1000 else f"{ms:6.0f} ms" if ms >= 10 else f"{ms:6.1f} ms"

def bar(frac, width):
    eighths = " ▏▎▍▌▋▊▉█"
    n = max(0.0, min(1.0, frac)) * width
    full = int(n)
    return "█" * full + (eighths[int((n - full) * 8)] if full < width else "") + " " * (width - full - 1 if full < width else 0)

def draw_wide(final=False):
    scale = max([e["total"] + (e["cur"] if e["running"] else 0) for e in ENGINES] + [1e-9])
    out = ["\x1b[H"]
    out.append(f"{B}unumsearch race{RST}  {DIM}{a.label}{RST}\x1b[K")
    out.append(f"{DIM}root {ROOT}" + (f" · {INDEX_FILES:,} files indexed" if INDEX_FILES else "") + f" · {len(QUERIES)} queries · files-only (-l){' -i' if CI else ''}{RST}\x1b[K")
    out.append(f"{DIM}timed: wall time per query, one engine at a time, warm page cache, index already built{RST}\x1b[K")
    out.append("\x1b[K")
    qline = f"query {qi_now}/{len(QUERIES)}  {q_now}" if not final else "all queries done"
    out.append(f"{YEL}{qline[:COLS-2]}{RST}\x1b[K")
    out.append("\x1b[K")
    for e in ENGINES:
        tot = e["total"] + (e["cur"] if e["running"] else 0)
        out.append(f"{e['col']}{B}{e['name']:<11}{RST}{DIM}{e['ver']} · {e['how']}{RST}"[:COLS + 40] + "\x1b[K")
        cur = f"{fmt(e['cur'])} {'◀ running' if e['running'] else '         '}"
        out.append(f"  {e['col']}{bar(tot / scale, BARW)}{RST} {B}{fmt(tot)}{RST}\x1b[K")
        stat = f"q {e['done']}/{len(QUERIES)}  this query {cur}  files {e['files']:,}"
        if e["name"] != "rg":
            stat += f"  same files as rg {e['match']}/{e['done']}"
        if e["err"]:
            stat += f"  errors {e['err']}"
        out.append(f"  {DIM}{stat}{RST}\x1b[K")
        out.append("\x1b[K")
    if final:
        rg_t = ENGINES[0]["total"]
        out.append(f"{B}result{RST}\x1b[K")
        for e in ENGINES:
            mult = "" if e["name"] == "rg" else (f"   {rg_t / e['total']:5.1f}x vs rg" if e["total"] > 0 else "")
            same = "" if e["name"] == "rg" else f"   same files as rg: {e['match']}/{len(QUERIES)} queries"
            if e["err"]: same += f"   errors: {e['err']}"
            out.append(f"  {e['col']}{B}{e['name']:<11}{RST} {B}{fmt(e['total'])}{RST} total{mult}{same}\x1b[K")
        out.append("\x1b[K")
    out.append(f"{DIM}live run, real timings · python3 bench/race.py · github.com/corpunum/unumsearch{RST}\x1b[K\x1b[J")
    sys.stdout.write("\n".join(out)); sys.stdout.flush()

def wrap(text, indent=""):
    return [indent + l for l in textwrap.wrap(text, COLS - len(indent) - 1)] or [indent]

def draw_narrow(final=False):
    scale = max([e["total"] + (e["cur"] if e["running"] else 0) for e in ENGINES] + [1e-9])
    home = os.path.expanduser("~")
    root = ROOT.replace(home, "~", 1) if ROOT.startswith(home) else ROOT
    lines = [f"{B}unumsearch race{RST}"]
    lines += [f"{DIM}{l}{RST}" for l in wrap(a.label)] if a.label else []
    lines += [f"{DIM}{l}{RST}" for l in wrap(f"root {root}")]
    lines += [f"{DIM}{l}{RST}" for l in wrap((f"{INDEX_FILES:,} files indexed · " if INDEX_FILES else "") + f"{len(QUERIES)} queries · -l{' -i' if CI else ''}")]
    lines += [f"{DIM}{l}{RST}" for l in wrap("timed: wall time per query, one engine at a time, warm cache, index already built")]
    lines.append("")
    if final:
        lines.append(f"{YEL}all queries done{RST}")
    else:
        lines.append(f"{YEL}query {qi_now}/{len(QUERIES)}{RST}")
        lines.append(f"{YEL}{q_now[:COLS - 1]}{RST}")
    lines.append("")
    for e in ENGINES:
        tot = e["total"] + (e["cur"] if e["running"] else 0)
        lines.append(f"{e['col']}{B}{e['name']}{RST} {DIM}{e['ver'].split()[-1]}{RST}")
        lines += [f"{DIM}{l}{RST}" for l in wrap(e["how"], "  ")]
        lines.append(f"  {e['col']}{bar(tot / scale, BARW)}{RST} {B}{fmt(tot).strip():>9}{RST}")
        run = " ◀" if e["running"] else ""
        lines.append(f"  {DIM}q {e['done']}/{len(QUERIES)} · this query {fmt(e['cur']).strip()}{run}{RST}")
        same = f" · same as rg {e['match']}/{e['done']}" if e["name"] != "rg" else ""
        lines.append(f"  {DIM}files {e['files']:,}{same}{' · errors ' + str(e['err']) if e['err'] else ''}{RST}")
        lines.append("")
    if final:
        rg_t = ENGINES[0]["total"]
        lines.append(f"{B}result{RST}")
        for e in ENGINES:
            mult = "" if e["name"] == "rg" or e["total"] <= 0 else f"  {rg_t / e['total']:.1f}x vs rg"
            lines.append(f"  {e['col']}{B}{e['name']:<11}{RST}{B}{fmt(e['total']).strip():>9}{RST}{mult}")
            if e["name"] != "rg":
                lines.append(f"  {DIM}same files as rg: {e['match']}/{len(QUERIES)}{' · errors ' + str(e['err']) if e['err'] else ''}{RST}")
        lines.append("")
    lines += [f"{DIM}live run, real timings{RST}", f"{DIM}python3 bench/race.py{RST}", f"{DIM}github.com/corpunum/unumsearch{RST}"]
    sys.stdout.write("\x1b[H" + "\n".join(l + "\x1b[K" for l in lines) + "\x1b[J"); sys.stdout.flush()

def draw(final=False):
    (draw_narrow if NARROW else draw_wide)(final)

# ---------------------------------------------------------------- race
sys.stdout.write("\x1b[?25l\x1b[2J"); sys.stdout.flush()
ref = []
try:
    for qi, q in enumerate(QUERIES, 1):
        qi_now, q_now = qi, q
        for e in ENGINES:
            box = {}
            def work(e=e, box=box):
                try:
                    box["files"] = e["fn"](q)
                except Exception as ex:  # report, keep racing
                    box["err"] = str(ex)
            t0 = time.perf_counter(); th = threading.Thread(target=work, daemon=True)
            e["running"], e["cur"] = True, 0.0
            th.start()
            while th.is_alive():
                th.join(1 / 30)  # throttle redraws; the wait itself is the real query
                e["cur"] = (time.perf_counter() - t0) * 1000
                draw()
            e["cur"] = (time.perf_counter() - t0) * 1000
            e["running"] = False; e["total"] += e["cur"]; e["done"] += 1
            fs = box.get("files", set())
            if "err" in box: e["err"] += 1
            e["files"] += len(fs)
            if e["name"] == "rg": ref = fs
            elif fs == ref and "err" not in box: e["match"] += 1
            draw()
    draw(final=True)
finally:
    sys.stdout.write("\x1b[?25h\n"); sys.stdout.flush()
    os.unlink(ign.name)
