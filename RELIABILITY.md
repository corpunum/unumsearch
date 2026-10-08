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
5. **Secrets stay out.** `.ssh/`, `.env*`, keys, tokens and similar are never indexed, never
   served from the overlay, never scanned, and a request cannot start a walk inside them.
6. **The HTTP API is confined.** Roots must be inside configured roots (symlinks and `..`
   resolved), `Host` must be loopback, bodies and result counts are capped, a token is required
   off loopback.
7. **Corruption is survivable.** A truncated, scrambled or missing shard or manifest never
   panics a reader; the unit reports not-ready and the writer rebuilds it.

## Tests

| Area | Where |
| --- | --- |
| Corpus rules, query semantics, freshness, ripgrep equivalence on a fixture | `tests/integration.rs` |
| Randomised differential test against `rg` (static corpora; Unicode and Greek, ignore files, hidden, binary, oversize, secrets), and a watcher phase with creates, edits, deletes, file and directory renames, ignore-file edits, ignored-file churn, symlinks and size-cap crossings: every `fresh` answer must equal `rg`, and the index must always converge | `tests/differential.rs` |
| Roots, secrets, HTTP hardening, shard generations, `files()` limits, overlapping roots, cross-process freshness | `tests/trust.rs` |
| Simulated disk failure during a rebuild | `tests/trust_failpoint.rs` |
| CLI, stdio JSON-RPC, MCP | `tests/cli.rs` |

CI runs all of it on Linux x86_64 and arm64, macOS and Windows. Reproduce or deepen the
differential test locally:

```bash
UNUMSEARCH_DIFF_CASES=5000 UNUMSEARCH_DIFF_MUTATIONS=100 cargo test --release --test differential -- --nocapture
UNUMSEARCH_DIFF_SEED=12345 cargo test --test differential static_corpora   # replay one failing seed
```

## Benchmark method

[`bench/bench.py`](bench/bench.py) runs a list of real queries (`[root, pattern, glob]`) against
ripgrep over the same corpus definition, the daemon's HTTP API and the CLI, median of N runs each
with a warm page cache, and checks that the file sets are identical. Report the daemon p50/p95 and
the CLI p50/p95. Changes that touch the query path are compared before/after on the same machine
and index with `--skip-rg` (timing only), and are not merged if they slow the daemon or CLI.

The daemon is the fast path. A CLI call that starts a new process per query pays process start
and index open and is slower than `rg` at the median on small trees; the index's advantage is the
tail and large trees.
