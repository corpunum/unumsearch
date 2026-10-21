# unumsearch benchmarks

How the numbers in the [README](../README.md) were measured, with the weak spots. Reference: [REFERENCE.md](REFERENCE.md).

## Whole tree and one repository

Measured 2026-10-09 on the author's machine (16-core/32-thread Ryzen AI MAX+ 395, 128 GB
unified memory, NVMe, Linux; a 92 GB local LLM was resident and serving during the runs, so
absolute numbers carry its noise). Corpus: the author's working tree, 247,535 files and 4.0 GB of
text after exclusions in 707 units (about 80 checkouts of one JavaScript monorepo, hundreds of
agent workspaces, a handful of other repositories).

Method ([`bench/run_private.sh`](../bench/run_private.sh), which drives
[`race_bench.py`](../bench/race_bench.py), [`cold_bench.py`](../bench/cold_bench.py) and
[`summarize.py`](../bench/summarize.py)): 20 regexes mined from a week of coding-agent tool calls
([`bench/queries-agent.json`](../bench/queries-agent.json)), case-insensitive, files-only (`-l`).
ripgrep 15.1 runs over the same corpus definition (`unumsearch excludes` as `--ignore-file`,
`--hidden`, the same 1 MB size cap). rg, the daemon (HTTP) and the one-shot CLI are interleaved
query by query with alternating order; each query's median over the runs is taken, p50/p95/worst
are across the 20 medians, and the whole pass is repeated three times (the median repeat is
shown). Warm page cache unless stated. The script builds a private index and serves it from a
private daemon, so it never touches a running one.

| Whole tree, 247k files (all roots) | p50 | p95 | worst query |
| --- | --- | --- | --- |
| ripgrep 15.1 (no index) | 347 ms | 482 ms | 541 ms |
| **unumsearch daemon** (HTTP) | **64 ms** | **163 ms** | **381 ms** |
| unumsearch CLI (new process per query) | 91 ms | 198 ms | 413 ms |

| One repository, 3.7k files | p50 | p95 | worst query |
| --- | --- | --- | --- |
| ripgrep 15.1 (no index) | 9.8 ms | 12 ms | 13 ms |
| **unumsearch daemon** (HTTP) | **2.3 ms** | 6.7 ms | 13 ms |
| unumsearch CLI (new process per query) | 12 ms | 15 ms | 20 ms |

- **Same answers.** Whole tree: identical file sets for 19 of 20 queries; on the 20th
  (`https?://...`) rg also lists `.env.example` files, which unumsearch never indexes by design
  (`.env*` is a secret exclude). One repository: 20 of 20.
- **Cold page cache** (corpus and index evicted with `posix_fadvise` before every call, 5 queries
  x 3): rg 5.2 s vs CLI 1.4 s median.
- **Index and build:** 1.02 GB on disk for 4.0 GB of text; a full build takes 61 to 72 s
  (peak RSS 62 to 72 MB). The index must be built, and the daemon (or an MCP /
  `rpc` process) kept running, before any of the speed shows up.
- **Daemon memory:** during the whole-tree race (rg, the daemon and CLI processes all reading
  the corpus) peak anonymous memory was 80 MB with `MALLOC_ARENA_MAX=2` (as the shipped systemd
  unit sets) and 211 MB without it; resident file pages of the mmapped index (about 0.6 GB here)
  are page cache the kernel can reclaim. v0.1.3 reached 568 MB in the same race and was killed by
  a 512 MB cap; see the [changelog](../CHANGELOG.md).
- **Weak spots.** The one-shot CLI is slower than rg on a small repository (12 vs 9.8 ms). The
  worst query, `(get|set)[A-Z]\w+\(` (80k matching files, about a third of the corpus), is where
  the index helps least: the trigram filter keeps most of the corpus and every candidate is read;
  v0.1.4 brought it from 790-1,240 ms to 380-530 ms (rg: 470-540 ms). No multiline (`-U`)
  patterns.
- The one-repository row is from the v0.1.3 run of the same morning (the v0.1.4 run, taken while
  the machine was busier, measured every engine about 1.7x slower). In interleaved A/B runs on the
  same index, v0.1.4 was faster than v0.1.3 on every one of the 20 queries in that repository
  (p50 2.0 to 1.6 ms, p95 7.6 to 3.4 ms) and over the whole tree (p50 126 to 83 ms, p95 1,258 to
  210 ms, worst 1,341 to 775 ms in a run with the machine heavily loaded).
- Binary size: about 3.5 MB (x86_64, glibc), 3.6 MB static musl x86_64, 2.9 MB static musl
  aarch64.

[`bench/bench.py`](../bench/bench.py) is a simpler single-pass version for a quick check on your own
tree.

## Branch switch: searching while `git checkout` rewrites the tree

What does an agent get if it keeps searching while a checkout replaces thousands of files?
[`bench/branch_switch.py`](../bench/branch_switch.py) indexes one scratch clone with a private daemon
(its own config, index and port), checks that daemon and rg return the same files on the idle
tree, then switches `--from` -> `--to` -> `--from` while one client sends the queries back to back
over HTTP. Every answer's latency and `fresh`/`complete` flags are recorded, and every answer that
started after the checkout finished and claimed `fresh` + `complete` is compared with rg on the
new tree. Measured 2026-10-10, v0.1.6 against v0.1.7 run back to back on the same clones, same
machine as above (a local LLM serving throughout), warm cache, files-only regex queries, two
directions per pair (shown as `a / b`).

| Repository, switch | Version | Files changed | Checkout | Index caught up | Answers flagged stale | p50 / p95 / max during the switch | Steady p50 / p95 | Fresh+complete answers vs rg |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| OpenUnum (JS, 1.9k files), two commits | 0.1.6 | 1,529 | 0.1 / 0.2 s | 0.9 / 1.4 s | 0.8 / 1.4 s | 2 / 2 / 5 ms; 2 / 3 / 12 ms | 0.9 / 1 ms | 27,258 checked, **0 wrong** |
| | **0.1.7** | 1,529 | 0.1 / 0.3 s | 0.8 / 1.1 s | 0.7 / 1.1 s | 2 / 2 / 5 ms; 2 / 3 / 7 ms | 0.9 / 1 ms | 28,011 checked, **0 wrong** |
| Linux (81.8k files), v6.6 <-> v6.6-rc7 | 0.1.6 | 151 | 0.3 / 0.1 s | 35.1 / 35.4 s | **none** | 20 / 56 / 107 ms; 20 / 56 / 103 ms | 19 / 53 ms | 3,482 checked, **0 wrong** |
| | **0.1.7** | 151 | 0.2 / 0.2 s | **2.0 / 2.1 s** | **none** | 20 / 63 / 86 ms; 21 / 60 / 74 ms | 19 / 52 ms | 888 checked, **0 wrong** |
| Linux, v6.6 <-> v6.5 | 0.1.6 | 14,856 | 2.9 / 1.4 s | 36.5 / 33.7 s | 36.3 / 34.5 s | 24 / 62 / 143 ms; 24 / 63 / 115 ms | 19 / 56 ms | 1,065 checked, **0 wrong** |
| | **0.1.7** | 14,856 | 2.9 / 3.2 s | **6.7 / 7.0 s** | **6.6 / 6.5 s** | 26 / 66 / 106 ms; 27 / 71 / 126 ms | 21 / 56 ms | 760 checked, **0 wrong** |
| Linux, v6.6 <-> v6.1 | 0.1.6 | 44,686 | 3.0 / 2.6 s | 20.2 / 22.6 s | 19.9 / 22.4 s | 12 / 33 / 69 ms; 12 / 35 / 55 ms | 9.0 / 23 ms | 1,507 checked, **0 wrong** |
| | **0.1.7** | 44,686 | 3.0 / 2.9 s | **7.5 / 6.4 s** | **7.3 / 6.2 s** | 13 / 39 / 74 ms; 13 / 42 / 75 ms | 10 / 30 ms | 1,483 checked, **0 wrong** |

- **Queries.** OpenUnum: the first 12 of [`queries-agent.json`](../bench/queries-agent.json). Linux:
  12 kernel regexes ([`queries-kernel.json`](../bench/queries-kernel.json)) that match 686 to
  17,169 files each; every answer returns all matching paths, so most of those milliseconds are
  result size, not lookup.
- **"Index caught up"** is checkout start until the rebuilt index is live. Since v0.1.7 a rebuild
  reads only the files whose size or change time moved and merges everything else from the old
  shards (see [How it works](REFERENCE.md#how-it-works)); before, a unit was rebuilt as a whole, so on the
  kernel any change cost a re-read of 81k files (20 to 36 s here). The remaining window is
  mostly the checkout itself (3 s for the big switches) plus 3 to 4 s of rebuild. Initial index
  of the kernel: 13 to 33 s with v0.1.6, 3 to 6 s with v0.1.7 (reading on 4 threads, sort-free
  trigram extraction).
- **"Flagged stale"**: up to 2,000 changed files, the daemon verifies the changed files directly
  and answers stay `fresh` and exact (the 151-file kernel switch: no stale answers at all, 0
  wrong). Past that, or when directories appear or vanish, answers say `fresh: false` /
  `complete: false` until the rebuild lands, and a client should fall back to its own scan. Those
  answers are not counted as wrong; they say so. The 2,000-file limit stays: each pending file is
  read by every query, which on the kernel already costs about 5 ms of p50 at 1,990 pending
  files (10.5 -> 15.7 ms), so tens of thousands would cost every query far more than the
  rebuild now takes.
- **Correctness.** Before each run all 12 queries returned exactly rg's files (rg with
  `unumsearch excludes` as `--ignore-file`, `--hidden`, 1 MB cap). After each checkout every
  answer marked fresh + complete was compared with rg: 0 of 64,454 differed (both versions).
  Answers that started while `git checkout` was still writing are not checked (the tree had no
  single state). Fewer answers were checked with v0.1.7 on the kernel because the window it
  queries through is shorter.
- **Memory.** Daemon peak RSS (VmHWM, with `MALLOC_ARENA_MAX=2`): about 40 MB on OpenUnum both
  versions; on the kernel 231 to 233 MB (v0.1.6) and 200 to 227 MB (v0.1.7), which includes
  resident pages of the mmapped index (259 / 252 MB). Peak anonymous memory while building the
  kernel index: 59 MB (v0.1.6), 72 MB (v0.1.7, four reading threads within the 96 MB budget).
- **Noise.** Runs were interleaved (v0.1.6 then v0.1.7, or the reverse) per pair; the rc7 and
  v6.5 pairs landed on a busier stretch (steady p50 19 to 21 ms against 9 to 10 ms) for both
  versions. Shallow clones (`--depth 1` per tag); one client; one run per pair per version.
  Steady-state query latency, A/B with two private daemons answering alternately: kernel
  (12 queries x 11 runs) 223 ms total for v0.1.7 against 226 ms for v0.1.6; the author's tree
  (20 agent queries x 5 runs, twice, order swapped) 3,727 ms against 3,823 ms; same files in
  every answer.

```bash
git clone --depth 1 --branch v6.6 https://github.com/torvalds/linux.git /tmp/linux
git -C /tmp/linux fetch --depth 1 origin tag v6.1
python3 bench/branch_switch.py --repo /tmp/linux --from v6.6 --to v6.1 \
    --queries bench/queries-kernel.json --port 7799
```

## See it yourself

```bash
# a throwaway daemon for this directory (or use your running service if its roots include it)
unumsearch --root . --index-dir /tmp/race-index --listen 127.0.0.1:7790 serve &
python3 bench/race.py --root . --url http://127.0.0.1:7790   # --queries FILE for your own regexes
```

[`bench/race.py`](../bench/race.py) (Python standard library only) runs the same queries through rg
(with the index's exclude rules), the unumsearch daemon and, if installed, ugrep, one engine at a
time and interleaved per query, with a live timer and bar per engine and a final card: totals,
speed-up and whether each engine returned the same files as rg. Nothing is replayed; the bars grow
only while a real process or request runs. `--unumsearch-cli` times one-shot CLI processes
instead of the daemon. A 12-query run over about 180 agent worktrees on the machine above (warm
cache): rg 4.0 s, unumsearch 0.22 s, ugrep 25 s; unumsearch returned the same files as rg for 12
of 12 queries, ugrep for 11 (it has no file-size cap).
