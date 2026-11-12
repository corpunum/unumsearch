# Changelog

## Unreleased

- **`status` is not `fresh` before the daemon has looked at its roots.** In its first moments a
  daemon has no units yet, and `fresh` over no units was `true`: a client that waited for
  `/status` to say fresh could search before anything was indexed (it got an honest
  `fresh: false` result with no files). Found by a CI run where a test did exactly that.
- **Small units are listed on one thread.** Starting the parallel walker's threads cost about
  2 ms per unit, more than the walk itself for a small unit: on the rig's 2,100 units (2,076 of
  them under 200 files) a full listing pass took 4.2 to 5.0 s, now 1.9 to 2.1 s. A unit whose
  previous listing held fewer than 2,000 files is walked serially; the result is identical
  (`tests/list_equiv.rs` compares both walkers, secrets and excludes included).
  `examples/list_bench.rs` measures a listing pass over your own index.
- **Readers see the writer's state changes within a beat again (regression in 0.1.11).** With the
  heartbeat in its own file, state that changed in memory without a build (a flag cleared by a
  rebuild that found nothing new, a watch that could not be added) reached CLI and MCP readers
  only at the next rescan, up to 5 minutes later; meanwhile a reader could answer `fresh: true`
  for a unit the daemon no longer trusted. The 30 s beat now saves the manifest when, and only
  when, its content changed; a lost watch and a mark-all-dirty are saved at once.
- **Linux: the list of watched directories no longer drifts from the real watches.** After an
  event-queue overflow (a large branch switch) a directory removed and created again during it
  stayed unwatched while its unit said `fresh`: every watch is now dropped and added again by
  the rebuilds an overflow causes, and a unit whose rescan finds a missed change is re-armed.
  After an overflow the split roots are watched again at once and new units are looked for (a
  child directory created during it stayed unknown until the next rescan). A renamed directory
  drops the watches below it whichever unit owns them (nested roots, a split root's child
  units), and those units are rebuilt to add them again. A subdirectory that vanishes between
  the listing and the watch no longer marks its unit unwatched; a unit whose root is gone is
  unwatched and not `fresh`. (`tests/watch_drift.rs`, `overflow_unit_scan.rs`, `root_gone.rs`,
  `nested_roots.rs`, `root_recreate.rs`, `split_root_recreate.rs`.)
- Manifest saves are serialized and a failed save is retried by the next beat; on macOS a daemon
  start no longer saves the manifest once per unit.
- **Test: a unit's root directory that is removed and created again is watched again (Linux).**
  `rm -rf repo && git clone ... repo` was checked, since the directory-level fix above did not
  cover the root itself: the new root is found `fresh: true` and a file created in it afterwards
  is found through its watch, not by a rescan. It already worked; `tests/root_recreate.rs` pins it.
- **Linux: a directory that is removed and created again is watched again.** The watch died with
  the old directory, but it stayed on the list of watched directories, so it was never added
  again: files created in the new directory raised no event and the unit stayed `fresh: true`
  without them until a rescan found them (`rm -rf build && mkdir build`, a branch switch that
  drops and restores a directory). An event that names a watched directory now drops the
  watches at and below it, and the rebuild that follows adds them again. In every release so far.
- **Every command takes `-h`/`--help`**, and an option a command does not take now says
  `(see unumsearch --help)`. `files` accepts `-m N` (and `--max-files N`) for its cap, like
  `search -m`. Agents try these first; before, each was an "invalid option" error.
- **Linux: directories without an indexed file are watched too.** Watches were placed only on
  directories that held an indexed file, so a file created in an empty directory (or one holding
  only ignored files) raised no event: the unit stayed `fresh: true` without it until the next
  rescan, up to 5 minutes. The listing now hands the watcher every directory it entered.
  (`tests/empty_dir_watch.rs`; macOS and Windows watch recursively and were not affected.)
- **Rescans back off for watched units that stay clean.** The periodic rescan listed every unit
  every `rescan_secs`: on the rig 2,100 units and 393k files, 5 to 18 s of listing (one 270k-file
  unit 0.75 to 2.3 s warm, 7 s cold) every 5 minutes; the fingerprint is about 0.1 s of it. A
  watched unit with nothing pending whose listing matched its index is now listed every 2nd, then
  every 4th rescan (`rescan_backoff_max`, default 4; 1 restores the old behavior). A rescan that
  finds a change the watcher missed puts every unit back to every rescan. Not backed off: units
  without a working watch, split roots' own units (their listing finds new hidden directories),
  setups without a watcher, and Windows, where each rescan re-arms the watches (#7). `fresh` is
  unchanged: on Linux and macOS a watched unit never depended on a recent listing.

## v0.1.11 (2026-11-12) - Quieter Writer

- **`--json` is accepted** (a no-op) by `search`, `files`, `index` and before the command. Output
  was always JSON, but agents add the flag out of habit and got "invalid option '--json'": a
  local-model OpenUnum worker following the skill spent its whole budget on that. `--json` after
  `--text` switches back to JSON. The skill now says that no flag is needed.
- **The writer's heartbeat no longer rewrites `manifest.json`.** The daemon beat every 30 s by
  rewriting the whole manifest (800 KB on the rig's index), and each CLI or MCP reader then parsed
  it again. The beat now goes to a separate `heartbeat` file of a few bytes in the index directory;
  readers take the later of it and the manifest's `heartbeat_ms`. The manifest is still saved at
  every rescan, so readers from 0.1.10 and older still see the writer alive.

## v0.1.10 (2026-11-11) - Health and Windows Watches

- **`GET /health`** on the HTTP API: `ok`, `name`, `version`, `git_sha` (the commit the binary was
  built from, embedded by `build.rs`; `UNUM_GIT_SHA` overrides, `unknown` outside a checkout) and
  `started_at`. It does not touch the index, so monitors can poll it during a rebuild.
- **Windows: a silently dropped watch no longer keeps a unit `fresh` (#7).** notify 8's
  ReadDirectoryChangesW backend unwatches a directory on a buffer overflow
  (`ERROR_NOTIFY_ENUM_DIR`) without an event or an error, and a zero-byte completion is parsed as
  nothing; a large burst (a branch switch, an unpacked archive) could then leave a unit answering
  `fresh: true` while later changes were missed. On Windows every rescan now drops and re-adds each
  unit's watches before listing it again, and a watched unit counts as fresh only if it was listed
  within twice `rescan_secs`. A lost burst is found within one rescan interval. Query latency is
  unchanged; other platforms are unchanged. (notify 9 reports the overflow as a rescan, which
  unumsearch already handles; it is still a release candidate and needs Rust 1.91.)

## v0.1.9 (2026-11-10) - Hot Directories

A directory in which an indexed file is rewritten every second no longer keeps the daemon busy.
Found on a rig where a 42 MB state file and several journals lived inside an indexed workspace:
the unit was rebuilt back to back for days, at a quarter of a core and several MB/s of index
writes. No API or index-format change.

- **Minimum rebuild interval per unit.** A unit is rebuilt at most once per 20 times its last
  build time (`rebuild_backoff_factor`), never more often than `debounce_ms`. While the unit stays
  hot the interval doubles per rebuild, up to 5 minutes (`rebuild_backoff_max_ms`). Searches stay
  exact and `fresh`: changed files are verified directly, as before. A change the overlay cannot
  represent waits only twice the build time. `rebuild_backoff_factor = 0` restores the old
  behaviour. Measured on a 2,311-file unit with one 112 KB file rewritten every second for 60 s
  (`tests/hot_unit.rs` covers the logic): index writes 537 MB -> 62 MB, daemon CPU 2.7 s -> 0.6 s,
  every one of the 60 searches exact and fresh.
- **`max_memory_mb` counts the daemon's own heap.** A rebuild gets the budget less what the
  process already holds (never less than a quarter of it), instead of the whole budget on top.
- **Merges no longer hold the output shard in memory.** The parallel merge of v0.1.8 kept each
  worker's postings (together up to a whole 64 MB shard) in memory until the shard was written.
  A worker now moves its postings to a temporary file past 4 MiB. Output bytes are identical.
- **Smaller watcher tables.** "Is this path indexed?" is answered from 64-bit path hashes
  (8 bytes per document) instead of a set of path strings per unit, and that cache is bounded by
  an eighth of the budget.

- **Local usage counters.** Each CLI subcommand, daemon/RPC query and MCP tool call appends one line (timestamp, name, ok, ms; no arguments, paths or results) to `~/.local/share/unum-usage/unumsearch.jsonl` via the Apache-2.0 [`unum-usage`](https://github.com/corpunum/unum-usage) crate. Local file only, no network. Set `UNUM_USAGE=off` to disable.

## v0.1.8 (2026-10-29) - Faster Search and Fuzzy Filename Matching

This release combines faster regex verification, faster shard merging, and fuzzy filename search. The README now leads with a story, with reference material and benchmarks split into dedicated docs.

Regex queries spend less CPU verifying candidate files. Answers are unchanged: the same files,
lines, line numbers and order as v0.1.7 (checked with `bench/verify_compare.py`, 70 queries, over
the indexed files of `~/main` and `~/unumsearch`).

- **Parallel shard merge and fuzzy filenames.** Shard merging is parallelized, and filename searches support fuzzy matching.
- **One search per file.** A candidate is searched once for the whole buffer, and each line
  where a match starts is then tested on its own, as before. Matches that span a newline cannot
  hide later lines.
- **Literal prefilter.** A file that lacks a literal every match must contain is skipped before
  the regex runs.
- **Binary check.** The first 8 KB are read and checked for NUL before the rest of a file is
  read. The rule itself is unchanged: a NUL anywhere in the file still makes it binary.
- Patterns with `$` on files that contain `\r`, and the `\A`/`\z` assertions, keep the v0.1.7
  per-line path.

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
