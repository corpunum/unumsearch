# Changelog

## v0.1.7 (2026-10-10) - Incremental Rebuilds

A large `git checkout` on a big repository no longer leaves answers stale for 20 to 36 s. No API
or index-format change; steady-state query latency unchanged (A/B below).

- **Incremental rebuilds.** A rebuild lists the unit, compares it with the indexed documents
  (path, size, change time) and reads only new and changed files and files an event reported;
  every other document keeps its postings. The shards that held dropped or changed documents
  are rewritten by `shard::merge`, which decodes and renumbers posting lists without reading
  any file, into shards of about 64 MB; untouched shards are kept as they are (one edit
  rewrites one shard). `index --force` still rebuilds from scratch.
- **Parallel, faster indexing.** Files are read and indexed on up to 8 threads, one per 24 MiB
  of `max_memory_mb` (4 at the default 96), each thread's share of the budget flushing small
  pieces that the merge combines. Trigram extraction deduplicates through a reusable
  open-addressing table instead of sorting every position (2.7x faster extraction), and the
  shard builder hashes trigrams with a multiplicative hasher. Initial index of the Linux tree:
  13-33 s -> 3-6 s.
- **Change time.** The size/mtime fingerprint and the per-document stamp now use the later of
  mtime and ctime (Unix), so a rewrite that restores an old mtime (`cp -p`, `touch -d`, archive
  extraction) is still seen. Files stamped within 2 s of being read ("racily clean", as git calls
  it) are read again by the next rebuild. The first rebuild after upgrading reads files whose
  ctime is later than their mtime once.
- **Watcher.** Once a unit awaits a full re-listing and its pending set is full (2,000 files),
  further events for it only move its quiet period instead of being classified one by one; on
  a 44k-file checkout that classification (a directory listing per new file) delayed the
  rebuild by 4 to 8 s.
- **Fix: a reported change could be dropped.** When the unit's listing was unchanged (a rewrite
  that kept size and mtime: restored mtime, coarse clock, Windows without ctime), the rebuild
  returned early and cleared the pending change, so the old content stayed indexed. A pending
  change now always gets its file read again. (Predates this release; found by the new tests
  on Windows CI.)
- Branch switch on Linux (81.8k files), v0.1.6 -> v0.1.7, interleaved on one machine: v6.6 <->
  v6.1 (44,686 files changed) index caught up 20.2/22.6 s -> 7.5/6.4 s, answers flagged stale
  19.9/22.4 s -> 7.3/6.2 s; v6.6 <-> v6.5 (14,856) 36/34 s -> 7/7 s; v6.6 <-> v6.6-rc7 (151,
  never stale) 35 s -> 2 s. 0 wrong fresh+complete answers in 64,454 checked against rg.
  Steady-state A/B: kernel 223 vs 226 ms total, the author's 247k-file tree 3,727 vs 3,823 ms.
- Tests: a branch-switch differential test against rg (thousands of files rewritten, deleted
  and added, directories appearing and vanishing, ignore-file edits, same-size rewrites with the
  old mtime put back; the incremental index must list exactly what a from-scratch build lists,
  with a bounded shard count), `tests/incremental.rs`, a shard-merge unit test and an
  extractor equivalence test.

## v0.1.6 (2026-10-09) - Fresh During Rebuilds

Fixes a freshness bug first seen on macOS CI ([#6](https://github.com/corpunum/unumsearch/issues/6)).
No API change; query latency unchanged.

- **Fix: an answer could say `fresh: true` while missing a recently changed file, during a
  rebuild.** A rebuild cleared the unit's dirty flag and its pending overlay (the changed files
  that queries verify directly) when it *started*, but installed its new shards only when it
  *finished*. A query in between was served from the old index without the overlay and still
  claimed to be fresh. On Linux the window is a few milliseconds; on macOS each shard write ends in
  `F_FULLFSYNC`, which on CI runners stretched it enough for the watcher differential test
  (seed 1024301) to get `fresh: true` without `a/b/new4.txt`. Pending state now stays in force
  until the new shards are live, and is resolved in the same locked step that swaps them in;
  changes that arrive during the rebuild stay pending after it, as before. This affected the
  daemon on every platform, not only the test.
- **Fix (macOS): adding or removing a watch could drop other units' events.** notify's FSEvents
  backend restarts its single stream on every watch change, from "now", so events for every path
  were lost while it restarted (for example when a new checkout appeared under a split root).
  A watch change on macOS now marks every unit for re-listing (writer-side work only; a listing
  whose fingerprint is unchanged rebuilds nothing).
- Tests: `tests/build_window.rs` slows rebuilds down on purpose (a test hook) and fails without
  the fix on any platform; `seed_1024301_with_slow_rebuilds_stays_exact` replays the macOS CI
  failure on any platform, deterministically.
- Known gap on Windows, tracked in [#7](https://github.com/corpunum/unumsearch/issues/7): notify's
  `ReadDirectoryChangesW` backend does not report buffer overflows to us.

## v0.1.5 (2026-10-09) - Honest Coverage

Fixes a correctness gap found by replaying 1,267 real agent searches. API additive.

- **Fix: a root inside an excluded directory was reported as covered.** Searching a directory
  that the indexed corpus leaves out (a gitignored kernel tree, an excluded `datasets/`, a
  hidden directory with hidden files off, a symlinked directory, an over-size file) returned no
  files with `covered: true, fresh: true`, while `rg` in that directory found matches (19 of the
  1,267 replayed searches). Such a root is now `covered: false` (so `complete: false`) and, with
  the scan fallback on (the default), is scanned: all 19 now return exactly `rg`'s files. The
  decision costs nothing when the root is a whole unit, a binary search when the index has files
  under the root, and one walk along the path (remembered per unit build) otherwise.
- **New `uncovered` field** on search and files results (and in `lookup` and `all_roots`
  answers): the paths the index does not cover, each with a `reason`: `excluded`,
  `not_indexed` (scan them) or `secret` (never scanned; do not fall back to scanning it).
  Omitted when the answer is covered.
- **Split roots** report their non-unit child directories (hidden names, which the corpus
  walk would include) as `uncovered`, instead of claiming the whole root is covered. Files
  returned are unchanged.
- Covered answers are unchanged: in the replay the other 1,248 searches returned the same files;
  12 of them, also rooted in excluded directories where `rg` found nothing either, now say
  `covered: false` too.
- Docs: Hermes Agent integration; README and RELIABILITY state what `covered` guarantees.

## v0.1.4 (2026-10-09) - Bounded Memory

Fixes the daemon out-of-memory found while benchmarking for launch, makes large queries faster,
and closes a coverage gap. API additive.

- **Fix: the daemon could run out of memory when other processes read the corpus.** The
  watcher's inotify mask (from the `notify` crate) includes `IN_OPEN`, so every file opened in a
  watched directory (by ripgrep, by `unumsearch` CLI processes, by the daemon's own
  verification) became an event in an unbounded queue. While the watch loop was busy (start-up
  scan, a rebuild) the queue grew without limit: on 2026-10-09 a whole-tree `rg`/CLI/daemon
  benchmark pushed the production daemon past its systemd `MemoryMax=512M` (504 MB anonymous
  memory; it was killed four times in six minutes). Read-only events are now dropped before they
  are queued, and the queue is bounded (16,384 events); an overflow marks every unit dirty, as a
  kernel queue overflow already did. Reproduced with a private daemon under the same benchmark:
  peak anonymous memory 568 MB before, about 100-120 MB after.
- **Bounded results.** A query's returned paths and lines are limited by `max_result_mb`
  (default 48, env `UNUMSEARCH_MAX_RESULT_MB`) as well as `max_files` / `max_matches`. Past a
  limit, verification stops early and the answer is cut in path order (the same cut however the
  threads ran) and reported as `truncated: true`. `all_roots` applies the limits to the merged
  answer. In files-only mode no matched line text is kept at all.
- **New `complete` field** on search and files results (and per pattern in `lookup`):
  `covered && fresh && !truncated`. The existing contract is unchanged: if `complete` is false the
  answer may be missing files.
- **Back-pressure.** The HTTP daemon runs at most `max_concurrent_queries` (default 2) searches at
  once and holds the slot until the reply is written; further requests wait instead of each
  holding a full answer in memory.
- **Faster verification.** One pool of threads per query claims candidates in blocks (it used to
  start new threads for every 256 candidates); candidates are compact (unit + path borrowed from
  the mmapped document table) and sorted without building absolute paths; read buffers are
  reused; files-only checks jump straight to a matching line instead of testing every line; very large
  candidate sets (16k or more) may use up to 16 threads when `threads = 0`. Interleaved A/B
  against v0.1.3 on the same private index (20 agent regexes over 246k files): p50 104 to 65 ms,
  p95 434 to 174 ms, worst query (`(get|set)[A-Z]\w+\(`, 80k matching files) 1,243 to 533 ms;
  file sets identical.
- **Fix: files directly inside a split root were not indexed** (for example
  `workspaces/_port-registry.json` with `split_roots = ["workspaces"]`). A split root is now also
  a unit of its own holding just its loose files; queries over the split root include them.
- Files that grew past `max_file_size` after indexing are skipped during verification, as a walk
  would skip them (they were read whole before).
- Benchmark scripts used for the README numbers are in `bench/` (`run_private.sh`,
  `race_bench.py`, `cold_bench.py`, `summarize.py`, `queries-agent.json`); they build a private
  index and daemon and never touch a running one.
- New tests: `tests/bounded.rs` (a counting allocator checks that a query matching 40,000 lines
  stays within its result budget and that a stalled watcher does not queue read events or more
  than a bounded number of write events), split-root loose files with a watcher.

## v0.1.3 (2026-10-09) - Trustworthy Search

Correctness and security release following an external review. API additive.

- **Security: the HTTP API is confined.** Roots in HTTP and JSON-RPC-over-HTTP requests must lie
  inside a configured root once symlinks and `..` are resolved (a root outside is refused, so the
  daemon can no longer scan arbitrary readable directories). Secret locations (`.ssh`, `.gnupg`,
  `.aws`, `.config`, ...) are refused on the absolute path for every front-end and for scan
  fallback, so starting a walk *inside* an excluded directory no longer bypasses the exclusions.
  `serve` refuses to bind a non-loopback address unless `auth_token` / `UNUMSEARCH_TOKEN` is set
  (then `Authorization: Bearer` or `X-Unumsearch-Token` is required); `Host` must be a loopback
  name (DNS rebinding); bodies are capped at 1 MiB, URLs at 64 KiB; pattern sizes and result
  counts are clamped. The changed-file overlay never follows symlinks and enforces the size cap.
- **Fix: a failed rebuild could report fresh.** `build_unit` is now transactional: on a shard
  write error the old index and every pending change are kept, the unit is marked not fresh and
  the rebuild is retried with back-off.
- **Fix: stale cross-process freshness.** The watcher publishes dirty/pending state within about
  5 ms of an event instead of once per second, and readers notice manifest rewrites by size and
  inode, not only mtime.
- **Fix: editing an indexed `.gitignore` / `.ignore` was treated as a plain file change** and left
  the answer fresh although the corpus had changed. Found by the new differential test.
- **Fix: shard name collisions.** Generations are strictly increasing; an old shard is only
  removed after the manifest replacing it is on disk and never if the new manifest references it.
- **Fix: `files` truncated before de-duplicating**; pending files no longer use up `max_files` twice.
- **Fix: `all_roots` dropped a nested configured root** even when the outer root's ignore rules
  excluded it; both are kept and results de-duplicated by path.
- **Faster start:** the document table is read in place from the mmapped shard instead of being
  copied into heap strings (CLI cold start 40 ms to 18-20 ms on the 700-unit rig corpus; reader
  RSS 56 MB to about 6 MB).
- New tests: randomised differential test against `rg` (thousands of comparisons, including Greek
  and other Unicode, ignore files, hidden/binary/oversize/secret files; a watcher phase with
  renames, ignore-file edits, ignored churn, size-cap crossings and symlinks), HTTP hardening,
  failure injection, shard generations, cross-process freshness. See `RELIABILITY.md`.
- Library: `FilesOpts` implements `Default`; `Shard::docs` is now `Shard::docs()` / `doc(id)` /
  `ndocs()`; `api::call_scoped` and `api::Scope`.
- README: the daemon is the fast path; a new CLI process per query is slower than `rg` at the
  median on small trees.

## v0.1.2 (2026-10-08)

- **Search every root in one call:** `search` takes `all_roots=1` (or `root=*`; CLI
  `--all-roots`; MCP `all_roots`) and merges the per-root answers, with a per-root `roots`
  summary. Additive.
- **Batch lookup:** new `lookup` method (`GET /lookup`, JSON-RPC `lookup`) answers which files
  contain each of many patterns, under one root or all roots, in one call. Additive; requested by
  the OpenLunum reference locator, where one-by-one requests cost hundreds of ms per record and
  parallel requests contended.
- **Explicit roots no longer merge the default config:** `--root`/`--split-root` without
  `--config`/`$UNUMSEARCH_CONFIG` ignore the platform default config file (previously its roots
  were silently merged in, so a private index could grow to cover every default root). New
  `--no-default-config` skips that file in every case. With `--config FILE`, `--root` still adds
  to the file's roots.

## v0.1.1 (2026-10-08)

- **Security fix:** files created or changed after a unit's last build were
  matched against the exclude rules by a listing that let excluded entries
  through, so a newly written secret file (`.env.local`, `*.pem`, `id_rsa`, ...)
  could be returned by `search`/`files` from the overlay until the unit's next
  rebuild (typically under a second, longer under continuous writes). Excludes
  are now re-checked for every entry; regression test added. Upgrade recommended.
- Fix: when several files were written within one event-drain cycle, a file
  created after its directory was first listed in that cycle could be ignored
  until the next rescan. The directory is now listed again before an event is
  dropped.
- Release workflow: prebuilt binaries for Linux (x86_64, aarch64; static musl),
  macOS (x86_64, arm64) and Windows (x86_64), `SHA256SUMS`, build-provenance
  attestations, and checksum-verifying `install.sh` / `install.ps1`.
- Docs: integrations for OpenUnum, Claude Code, Codex CLI, Gemini CLI, editors,
  Goose, OpenCode, OpenClaw, Pi, Aider, generic MCP, Agent Skills, HTTP API and
  shell/CI; compatibility matrix; FAQ.

## v0.1.0 (2026-10-08)

First release: trigram index with next-byte masks, always-fresh watcher with
overlay, CLI, HTTP/JSON-RPC daemon, MCP server and Agent Skill.
