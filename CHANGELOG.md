# Changelog

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
