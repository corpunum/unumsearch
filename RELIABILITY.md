# Reliability

What unumsearch promises, how that is tested, and how its speed is measured.

## Guarantees

1. **Exact answers.** Every returned match is verified against the file on disk. The index only
   narrows which files are read. For the same corpus definition the file set equals `rg -l`.
2. **Honest freshness.** An answer says `fresh: true` only when the index plus the direct
   verification of known-changed files is exact. Changes the overlay cannot represent
   (directory moves, ignore-file edits, lost events) and failed rebuilds make it `fresh: false`;
   clients should then fall back to their own scan.
3. **Failed rebuilds are transactional.** If writing the new shards fails (disk full, I/O error)
   the old index is kept, every change that was pending is kept, the unit is marked not fresh and
   the rebuild is retried with back-off. Shard files get unique, strictly increasing generation
   numbers and an old shard is only removed after the manifest that replaces it is on disk.
4. **Readers see changes at once.** The writer publishes dirty/pending state to other processes
   (CLI, MCP) within a few milliseconds of noticing a change, not on a timer.
5. **Honest coverage.** `covered: true` means the index holds every file a walk of the requested
   root would search under the same corpus rules, so an empty answer really is empty. A root
   inside a directory the index leaves out (an ignored or excluded directory, a hidden one when
   hidden files are off, a symlinked directory, an over-size file) or a split root with non-unit
   child directories says `covered: false` and lists the `uncovered` paths with a reason:
   `excluded` or `not_indexed` (scan them) or `secret` (never scanned; a client must not fall
   back to scanning it either). Excluded directories *below* a covered root are not part of its
   corpus and do not make it uncovered (as with `rg`, ignore rules apply).
6. **Secrets stay out.** `.ssh/`, `.env*`, keys, tokens and similar are never indexed, never
   served from the overlay, never scanned, and a request cannot start a walk inside them.
7. **The HTTP API is confined.** Roots must be inside configured roots (symlinks and `..`
   resolved), `Host` must be loopback, bodies and result counts are capped, a token is required
   off loopback.
8. **Corruption is survivable.** A truncated, scrambled or missing shard or manifest never
   panics a reader; the unit reports not-ready and the writer rebuilds it.
9. **Bounded memory, honest truncation.** A query's returned paths and lines are capped by
   `max_files`, `max_matches` and `max_result_mb`; past a cap the answer is cut in path order
   (deterministically) and says `truncated: true`, `complete: false`. The daemon runs at most
   `max_concurrent_queries` searches at once. The watcher never queues read-only events and its
   queue is bounded; an overflow marks every unit dirty instead of growing memory.

## Tests

| Area | Where |
| --- | --- |
| Corpus rules, query semantics, freshness, ripgrep equivalence on a fixture | `tests/integration.rs` |
| Randomised differential test against `rg` (static corpora; Unicode and Greek, ignore files, hidden, binary, oversize, secrets), and a watcher phase with creates, edits, deletes, file and directory renames, ignore-file edits, ignored-file churn, symlinks and size-cap crossings: every `fresh` answer must equal `rg`, and the index must always converge | `tests/differential.rs` |
| Roots, secrets, HTTP hardening, shard generations, `files()` limits, overlapping roots, cross-process freshness | `tests/trust.rs` |
| Simulated disk failure during a rebuild | `tests/trust_failpoint.rs` |
| Result budget and deterministic truncation, a busy watcher flooded with reads and writes (counting allocator) | `tests/bounded.rs` |
| CLI, stdio JSON-RPC, MCP | `tests/cli.rs` |
| Coverage of roots under each exclusion kind (ignore files, excludes, secrets, hidden, size, symlinks, split-root non-unit children) across the engine, API and CLI | `tests/coverage.rs` |

CI runs all of it on Linux x86_64 and arm64, macOS and Windows. Reproduce or deepen the
differential test locally:

```bash
UNUMSEARCH_DIFF_CASES=5000 UNUMSEARCH_DIFF_MUTATIONS=100 cargo test --release --test differential -- --nocapture
UNUMSEARCH_DIFF_SEED=12345 cargo test --test differential static_corpora   # replay one failing seed
```

## Benchmark method

[`bench/run_private.sh`](bench/run_private.sh) reproduces the README numbers without touching a
running daemon: it builds a private index of your config (twice, for build time and size),
serves it from a private daemon, races `rg -l` / the daemon / the CLI over one repository and
over every root with interleaved order (recording the daemon's peak memory), measures a cold
page cache, and writes `summary.json`. [`bench/bench.py`](bench/bench.py) is a quicker
single-pass check. Report p50/p95/worst over the per-query medians and the file-set parity.
Changes that touch the query path are compared before/after on the same machine and index,
interleaved, and are not merged if they slow the daemon or CLI.

The daemon is the fast path. A CLI call that starts a new process per query pays process start
and index open and is slower than `rg` at the median on a small repository; the index's
advantage is repeated queries over large trees. Its weakest case is a pattern whose candidates
are a large share of the corpus (every candidate must still be read).
