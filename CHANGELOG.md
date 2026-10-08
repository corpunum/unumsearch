# Changelog

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
