# Changelog

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
