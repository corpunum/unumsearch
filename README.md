# unumsearch

Fast, always-fresh, indexed file and code search for local directories, written in Rust.
One engine with several thin front-ends: a **library**, a **CLI** with stable JSON output, a
**daemon** (local HTTP/JSON API and stdio JSON-RPC), an **MCP server**, and an **Agent Skill**.
No GPU, model or network dependencies; nothing in it is specific to any one agent, framework or
operating system.

It was built for AI coding agents that grep large trees all day: dozens of checkouts of the same
monorepo, agent workspaces, a few big repos. A plain `rg` over such a tree reads gigabytes per
query. unumsearch reads a compact trigram index instead and only opens the files that can match,
then verifies them against the real file contents, so answers are exact and usually take a few
milliseconds.

> **Use the daemon for speed.** The fast path is the running daemon (or the MCP server / `rpc`
> process, which keep the index open). Over a 247k-file tree it answered the benchmark's 20
> agent regexes at 64 ms p50 against 347 ms for `rg -l`; inside one 3.7k-file repository, 2.3 ms
> against 9.8 ms. A *new CLI process per query* pays process start and index open each time and
> is **slower than `rg` on a small repository** (12 ms vs 9.8 ms p50). See
> [Benchmarks](#benchmarks) for the weak spots and [RELIABILITY.md](RELIABILITY.md) for what is
> guaranteed and how it is tested.

## Features

- **Content search**: regex (Rust/ripgrep syntax) or literal, case-sensitive or not, with
  ripgrep-style globs (`-g '*.rs' -g '!tests/**'`). Results are verified line by line against the
  files on disk; the index only narrows which files are read.
- **Filename search**: globs and/or a regex on the relative path or basename.
- **ripgrep-compatible corpus**: the file set is defined with ripgrep's own walker (the `ignore`
  crate): `.gitignore`/`.ignore` respected, hidden files included, binary files (NUL byte) and
  files above a size cap skipped, plus configurable gitignore-syntax excludes. Equivalence with
  `rg -l` is part of the test suite.
- **Secrets-safe by default**: `.ssh/`, `.gnupg/`, `.aws/`, `.env*`, `*.key`, `*.pem`,
  `id_rsa*`, `secrets.json`, `.netrc`, `.npmrc`, credentials files and more are never indexed,
  regardless of configuration. Build output, dependency trees, media and archives are excluded
  by default (overridable).
- **Always fresh**: a filesystem watcher (`notify`: inotify, FSEvents, ReadDirectoryChangesW)
  marks changed files immediately; they are searched directly until the background rebuild of
  their unit lands (typically well under a second). A periodic rescan with a size/mtime
  fingerprint catches anything the watcher missed. Every answer says whether it is `fresh`.
- **Bounded resources**: a configurable build-memory budget, mmapped read-only shards (index
  pages are reclaimable page cache, not heap), compact on-disk format (delta/varint postings with
  an 8-bit next-byte mask per posting that removes most false-positive candidates).
- **Units**: each root is indexed as a unit; a *split root* (a folder of many checkouts) makes
  each child its own unit, so a change rebuilds one checkout, not all of them.
- **One writer, many readers**: a lock file elects one writer per index directory; CLI
  invocations and MCP servers open the same index read-only and reload it when it changes.
- **Graceful fallbacks**: a directory the index does not cover is scanned directly with the same
  corpus rules (`"backend": "scan"`), unless you ask for `--no-scan`.

## Install

Prebuilt binaries for Linux (x86_64, aarch64; fully static musl), macOS (x86_64, arm64) and
Windows (x86_64) are on the [releases page](https://github.com/corpunum/unumsearch/releases).
The install scripts pick the right archive, verify it against the release's `SHA256SUMS` and
install the binary (default `~/.local/bin`; `%LOCALAPPDATA%\Programs\unumsearch` on Windows):

```bash
curl -fsSL https://github.com/corpunum/unumsearch/releases/latest/download/install.sh | sh
```

```powershell
irm https://github.com/corpunum/unumsearch/releases/latest/download/install.ps1 | iex
```

`UNUMSEARCH_VERSION=v0.1.5` pins a version, `UNUMSEARCH_INSTALL_DIR` changes the destination.
Every archive and `SHA256SUMS` also carry a GitHub build-provenance attestation:

```bash
gh attestation verify unumsearch-v0.1.5-x86_64-unknown-linux-musl.tar.gz --repo corpunum/unumsearch
```

Each archive contains the binary, this README, the license, `config.example.toml`, the Agent
Skill (`skills/`) and the systemd unit (`packaging/`).

From source (Rust 1.89 or newer):

```bash
cargo install --git https://github.com/corpunum/unumsearch --tag v0.1.5
# or
git clone https://github.com/corpunum/unumsearch && cd unumsearch
cargo build --release            # target/release/unumsearch
```

Fully static Linux binaries (no glibc dependency), e.g. for servers or phones:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
  cargo build --release --target aarch64-unknown-linux-musl
```

## Quick start

```bash
# Index a directory once and search it.
unumsearch --root ~/src/myrepo index
unumsearch search 'fn\s+main' ~/src/myrepo
unumsearch search -F -i 'config.load(' ~/src/myrepo --text

# Keep it fresh and serve the local API (or install the systemd unit below).
unumsearch --root ~/src/myrepo serve
```

For permanent use, put the roots in a config file (see [`config.example.toml`](config.example.toml)):
`--config FILE`, `$UNUMSEARCH_CONFIG`, or the platform default
(`~/.config/unumsearch/config.toml` on Linux, `~/Library/Application Support/unumsearch/config.toml`
on macOS, `%APPDATA%\unumsearch\config.toml` on Windows). Environment variables
(`UNUMSEARCH_ROOTS`, `UNUMSEARCH_SPLIT_ROOTS`, `UNUMSEARCH_INDEX_DIR`, `UNUMSEARCH_LISTEN`,
`UNUMSEARCH_MAX_MEMORY_MB`, `UNUMSEARCH_MAX_FILE_SIZE`) override the file; flags override both.

**Explicit roots replace the default config.** `--root`/`--split-root` without `--config` (or
`$UNUMSEARCH_CONFIG`) do not read the platform default config file, so a private index built
with `--root DIR --index-dir DIR` covers exactly `DIR`. (Before v0.1.3 the default file's roots
were merged in.) `--no-default-config` skips the default file in every case; with an explicit
`--config FILE`, `--root` adds to that file's roots.

### As a service (Linux, systemd user unit)

```bash
install -m 755 target/release/unumsearch ~/.local/bin/
cp packaging/systemd/unumsearch.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now unumsearch
```

The unit runs at background CPU/IO priority with `MemoryMax=512M` (the cap includes page
cache for files read during verification; the process's own heap stays far below it).

## Use it from your agent or harness

unumsearch works with any agent, harness or script: it speaks MCP, a local HTTP/JSON API, stdio
JSON-RPC, the Agent Skills format and plain CLI. Exact, checked configuration for each tool is in
[`docs/integrations/`](docs/integrations/README.md).

- **[OpenUnum](https://github.com/corpunum/openunum)**: first-class. OpenUnum's built-in search
  tools use the unumsearch daemon as a fast-search backend and fall back to their own walk
  automatically when the daemon is absent, stale or does not cover a directory. OpenUnum
  installers ship unumsearch and enable the backend by default.
  [Guide](docs/integrations/openunum.md).
- **Claude Code**: `claude mcp add unumsearch -- unumsearch mcp`, or the skill in
  `~/.claude/skills/`. [Guide](docs/integrations/claude-code.md).
- **OpenAI Codex CLI**: `codex mcp add unumsearch -- unumsearch mcp` (`[mcp_servers.unumsearch]`
  in `~/.codex/config.toml`). [Guide](docs/integrations/codex.md).
- **Gemini CLI**: `gemini mcp add unumsearch unumsearch mcp`. [Guide](docs/integrations/gemini-cli.md).
- **Cursor, VS Code / GitHub Copilot agent mode, Windsurf, Cline, Roo Code, Continue, Zed**:
  stdio MCP server `unumsearch mcp`. [Guide](docs/integrations/editors.md).
- **Goose**, **Hermes Agent**, **OpenCode**: MCP. [Goose](docs/integrations/goose.md),
  [Hermes](docs/integrations/hermes.md), [OpenCode](docs/integrations/opencode.md).
- **OpenClaw**: `openclaw mcp set unumsearch '{"command":"unumsearch","args":["mcp"]}'` or the
  skill. [Guide](docs/integrations/openclaw.md).
- **Pi**: the skill in `~/.pi/agent/skills/` (Pi has no MCP by design). [Guide](docs/integrations/pi.md).
- **Aider**: `/run unumsearch search --text PATTERN DIR` (no MCP client). [Guide](docs/integrations/aider.md).
- **Any MCP client**, **Agent Skills consumers**, **LangChain / LlamaIndex / custom agents**
  (HTTP API), **shell and CI**: [MCP](docs/integrations/generic-mcp.md),
  [skills](docs/integrations/agent-skills.md), [HTTP](docs/integrations/http-api.md),
  [shell/CI](docs/integrations/shell-ci.md).

## Agnostic by design

- **No GPU, no model, no network.** It is a plain trigram index plus a regex engine; it never
  calls a model and never opens an outbound connection. The HTTP API listens on loopback only.
- **Any agent or harness.** Nothing in the engine knows about a particular agent. The same
  `api::call` surface backs the CLI, HTTP, JSON-RPC and MCP front-ends, so every client gets the
  same answers, including the `fresh`/`covered`/`truncated` flags that let it decide when to
  fall back to its own scan.
- **Any OS.** One static binary per platform; the watcher uses each OS's native notification API.
- **Stable interfaces.** CLI flags, JSON fields and HTTP/JSON-RPC parameters only grow; existing
  fields keep their meaning.

## Compatibility

| OS | Arch | Release binary | CLI | Daemon + HTTP | JSON-RPC (stdio) | MCP (stdio) | Watcher | Service |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Linux | x86_64 | static musl | yes | yes | yes | yes | inotify | systemd user unit (`packaging/systemd`) |
| Linux (incl. Android/Termux-style userlands) | aarch64 | static musl | yes | yes | yes | yes | inotify | systemd user unit |
| macOS | arm64 | yes | yes | yes | yes | yes | FSEvents | launchd agent (see FAQ) |
| macOS | x86_64 | yes | yes | yes | yes | yes | FSEvents | launchd agent |
| Windows | x86_64 | yes (static CRT) | yes | yes | yes | yes | ReadDirectoryChangesW | scheduled task (see FAQ) |

The test suite runs in CI on Linux x86_64 and arm64, macOS arm64 and Windows x86_64; the macOS
x86_64 build is cross-compiled from arm64.

## CLI

| Command | Purpose |
| --- | --- |
| `index [--force]` | Build or update the index once (fails if a daemon holds the writer lock). |
| `search PATTERN [PATH]` | Content search. `-F` literal, `-i` ignore case, `-g GLOB` (repeatable), `-l` files only, `-m N` max matches, `--candidates` unverified index candidates, `--no-scan`, `--text` for `path:line:text` output, `--all-roots` every configured root. |
| `files [PATH]` | Filename search. `-g GLOB`, `--regex RE`, `--max N`. |
| `status` | Units, file counts, index size, freshness, RSS. |
| `excludes` | Effective exclude patterns, e.g. for `rg --ignore-file`. |
| `watch` | Keep the index fresh (foreground, no API). |
| `serve` | `watch` plus the HTTP API on `--listen` (default `127.0.0.1:7781`). |
| `rpc [--watch]` | JSON-RPC 2.0 over stdio (one message per line). |
| `mcp [--watch]` | MCP server over stdio. |

Global flags (before the command): `--config`, `--no-default-config`, `--root DIR` (repeatable), `--split-root DIR`,
`--index-dir DIR`, `--listen ADDR`, `--exclude PATTERN`, `--max-memory-mb N`.

All output is JSON: `{"ok": true, "result": {...}}` with `backend` (`index`, `scan`, `none`),
`covered`, `fresh`, `files`, `matches` (`path`, `line`, `text`), `candidates`, `truncated`,
`complete` (`covered && fresh && !truncated`), per-unit `units` status and `elapsed_ms`. Errors: `{"ok": false, "error": "..."}`, exit code 2.

`covered: true` means the index holds every file of the root's corpus: the files a walk of that
root with the same rules (ignore files, excludes, hidden, size cap) would search. When the root
or part of it is outside the index, `covered` is `false` and `uncovered` lists those paths with a
`reason`: `excluded` (inside a directory the indexed corpus leaves out, e.g. a gitignored kernel
tree or an excluded `datasets/`) and `not_indexed` (no unit holds it): scan them, which
unumsearch itself does unless `scan_fallback` is off (`backend: "scan"`). `secret`: a secret
location, never indexed and never scanned; do not search it with another tool either (the HTTP,
RPC and MCP front-ends refuse such roots outright).

## HTTP and JSON-RPC API

`GET /status`, `GET /search`, `GET /files`, `GET /lookup`, and `POST /rpc` (a JSON-RPC 2.0
message). The same methods (`search`, `files`, `lookup`, `status`, `reindex`) are available over
`unumsearch rpc`.

`search` parameters: `pattern` (required), `root`, `mode` (`literal` default, or `regex`),
`ignore_case`/`ci`, `glob` (comma-separated or repeated), `files_only`, `candidates_only`,
`max_matches`, `max_files`, `scan_fallback`, `all_roots` (or `root=*`: every configured root in
one call; the answer adds a per-root `roots` summary). `files`: `root`, `glob`, `regex`,
`max_files`.

`lookup` answers "which files contain each of these patterns?" for many patterns in one call:
`patterns` (JSON array, or newline-separated over `GET`), `root` or `all_roots`, `mode`
(`literal` default), `ignore_case`, `glob`, `max_files` per pattern (default 100). The result has
one entry per pattern (`pattern`, `found`, `files`, `covered`, `fresh`, `truncated`, `complete`),
plus the `uncovered` paths of all its roots. It runs
inside the daemon, so it avoids per-request overhead and contention from many parallel calls.

```bash
curl 'http://127.0.0.1:7781/search?root=/src/repo&pattern=TODO&glob=*.rs&files_only=1'
curl 'http://127.0.0.1:7781/search?all_roots=1&pattern=parse_config&files_only=1'
curl -s -X POST http://127.0.0.1:7781/rpc \
  -d '{"jsonrpc":"2.0","id":1,"method":"lookup","params":{"patterns":["FooBar","baz_qux"],"all_roots":true}}'
echo '{"jsonrpc":"2.0","id":1,"method":"search","params":{"pattern":"TODO","root":"/src/repo"}}' | unumsearch rpc
```

A client that matches with its own regex dialect can ask for `candidates_only=1` and verify
the returned files itself; if `complete` is false (`fresh` false, `covered` false or `truncated`
true), it should fall back to its own scan.

Memory is bounded per query: besides `max_files` / `max_matches`, the returned paths and lines
are capped by `max_result_mb` (config, default 48 MiB). A query that reaches a cap stops early
and its answer is cut in path order (the same cut every time) with `truncated: true`. The daemon
runs at most `max_concurrent_queries` (default 2) searches at once; further requests wait.

## MCP server

`unumsearch mcp` exposes three read-only tools: `search`, `find_files`, `index_status`.

```bash
claude mcp add unumsearch -- unumsearch mcp                      # Claude Code
codex mcp add unumsearch -- unumsearch mcp                       # Codex CLI
```

Any MCP client can launch it the same way (`command: unumsearch`, `args: ["mcp"]`); see
[`docs/integrations/`](docs/integrations/README.md) for per-tool configuration. It reads
the index maintained by the daemon; `--watch` makes it maintain the index itself when no
daemon is running.

## Agent Skill

[`skills/unumsearch/SKILL.md`](skills/unumsearch/SKILL.md) describes the CLI in the Agent Skills
format. Copy the folder into your agent's skills directory (Claude Code: `~/.claude/skills/`;
Codex, OpenClaw and Pi: `~/.agents/skills/`). See [agent-skills.md](docs/integrations/agent-skills.md).

## Library

```rust
use unumsearch::{Config, Engine, SearchOpts};

let cfg = Config { roots: vec!["/src/repo".into()], ..Config::default() };
let engine = Engine::open(cfg, true)?;          // true: become the writer if possible
engine.index_all(false);
let r = engine.search(&SearchOpts {
    pattern: "fn main".into(),
    root: "/src/repo".into(),
    ..Default::default()
})?;
for m in r.matches { println!("{}:{}:{}", m.path, m.line, m.text); }
```

`unumsearch::watch::run(engine, stop)` runs the freshness loop; `unumsearch::api::call` is the
request surface every front-end uses.

## How it works

- **Index**: per unit, one or more shards. A shard holds the document table (relative path, size,
  mtime) and, for every byte trigram of the content (ASCII case-folded), a delta/varint posting
  list of documents, each with an 8-bit mask of the bytes that follow that trigram in the
  document. Shards are written to a temporary file and renamed into place, then mmapped.
- **Queries**: the pattern is parsed with `regex-syntax` and turned into a boolean formula over
  trigrams that every match must satisfy (literal sets are expanded through small classes,
  alternations and optional parts; anything that cannot be bounded becomes "no constraint").
  Candidates are then read and matched line by line with the `regex` crate.
- **Freshness**: watches are placed only on directories that contain indexed files (never on
  `node_modules/` or `target/`), so inotify limits are spent on the corpus. An event is checked
  against the corpus rules (gitignore, excludes, size): churn in ignored files does not make a unit
  dirty. Changed files are searched directly until the unit's rebuild (debounced) replaces its
  shards; directory moves and ignore-file edits mark the unit not fresh until then.

## Benchmarks

Measured 2026-10-09 on the author's machine (16-core/32-thread Ryzen AI MAX+ 395, 128 GB
unified memory, NVMe, Linux; a 92 GB local LLM was resident and serving during the runs, so
absolute numbers carry its noise). Corpus: the author's working tree, 247,535 files and 4.0 GB of
text after exclusions in 707 units (about 80 checkouts of one JavaScript monorepo, hundreds of
agent workspaces, a handful of other repositories).

Method ([`bench/run_private.sh`](bench/run_private.sh), which drives
[`race_bench.py`](bench/race_bench.py), [`cold_bench.py`](bench/cold_bench.py) and
[`summarize.py`](bench/summarize.py)): 20 regexes mined from a week of coding-agent tool calls
([`bench/queries-agent.json`](bench/queries-agent.json)), case-insensitive, files-only (`-l`).
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
  a 512 MB cap; see the [changelog](CHANGELOG.md).
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

[`bench/bench.py`](bench/bench.py) is a simpler single-pass version for a quick check on your own
tree.

### See it yourself

```bash
# a throwaway daemon for this directory (or use your running service if its roots include it)
unumsearch --root . --index-dir /tmp/race-index --listen 127.0.0.1:7790 serve &
python3 bench/race.py --root . --url http://127.0.0.1:7790   # --queries FILE for your own regexes
```

[`bench/race.py`](bench/race.py) (Python standard library only) runs the same queries through rg
(with the index's exclude rules), the unumsearch daemon and, if installed, ugrep, one engine at a
time and interleaved per query, with a live timer and bar per engine and a final card: totals,
speed-up and whether each engine returned the same files as rg. Nothing is replayed; the bars grow
only while a real process or request runs. `--unumsearch-cli` times one-shot CLI processes
instead of the daemon. A 12-query run over about 180 agent worktrees on the machine above (warm
cache): rg 4.0 s, unumsearch 0.22 s, ugrep 25 s; unumsearch returned the same files as rg for 12
of 12 queries, ugrep for 11 (it has no file-size cap).

## Platform status

| Target | Status |
| --- | --- |
| Linux x86_64 (glibc, musl static) | Built, tested, in daily use. |
| Linux aarch64 (musl static) | Cross-built with `rust-lld`; index, search, watcher and daemon verified on an aarch64 Android phone running Linux. CI runs the test suite on arm64. |
| macOS (arm64, x86_64) | Test suite runs in CI (arm64); release binaries for both. FSEvents recursive watches. |
| Windows x86_64 | Test suite runs in CI; release binary with static CRT. Replaced shards that are still mapped are removed at the next start. |

## Limitations

- A unit is rebuilt as a whole when it changes (changed files are searchable immediately
  through the overlay). Very large single units rebuild more slowly; split them with
  `split_roots`.
- Matching is line-oriented like ripgrep without `-U`; multi-line patterns are not supported.
- Case-insensitive matching of non-ASCII text works but gets less help from the index (non-ASCII
  trigrams are not case-folded), so it reads more candidate files.
- The HTTP API binds to loopback by default. It only answers for directories inside the
  configured roots, refuses secret locations and foreign `Host` headers, and caps request sizes
  (see [Security](#security)); it is still not designed to be exposed to untrusted networks.

## FAQ

**Does it need a daemon?** No. The CLI and `unumsearch mcp` work on their own: without an index
they scan directly with the same corpus rules. The daemon (`serve`, or the user service) keeps
the index fresh so queries take milliseconds; `mcp --watch` and `rpc --watch` maintain it
in-process instead.

**Will an agent ever see stale results?** Every answer reports `fresh`. Changed files are
searched directly until their unit is rebuilt, so edits show up within milliseconds; when a
unit cannot be trusted (directory moves, ignore-file edits) the answer says `fresh: false` and
clients such as OpenUnum fall back to their own scan.

**Does it index my secrets?** No. `.ssh/`, `.gnupg/`, `.aws/`, `.env*`, `*.key`, `*.pem`,
`id_rsa*`, `secrets.json`, `.netrc`, `.npmrc` and credentials files are excluded regardless of
configuration.

**How much memory does it use?** The index is mmapped (reclaimable page cache); the heap stays
around 100 to 150 MB for a 200,000-file corpus. `max_memory_mb` bounds index builds; the systemd
unit caps the service at 512 MB.

**How do I run it as a service on macOS or Windows?** macOS: a launchd agent in
`~/Library/LaunchAgents/` with `ProgramArguments` `[path/to/unumsearch, serve]`, `RunAtLoad` and
`KeepAlive`. Windows: a scheduled task at logon, e.g.
`schtasks /Create /SC ONLOGON /TN unumsearch /TR "\"%LOCALAPPDATA%\Programs\unumsearch\unumsearch.exe\" serve"`.
OpenUnum's installers set these up automatically.

**Is it a replacement for ripgrep?** For repeated searches over large, mostly unchanged trees,
yes; it returns the same file sets as `rg -l` over the same corpus. For one-off searches of a
small directory, `rg` is just as fast and needs no index.

**Is it on crates.io?** Not yet; install from the release binaries or with
`cargo install --git`.

## Security

- **Roots are authorised.** Over HTTP (and JSON-RPC over HTTP) the `root` of a request must lie
  inside a configured root after resolving symlinks and `..`; anything else is refused with an
  error, so the daemon cannot be used to scan arbitrary readable directories. The CLI, stdio
  JSON-RPC and MCP run as the calling user and may still search other directories.
- **Secret locations are refused wherever a walk starts.** `.ssh`, `.gnupg`, `.aws`, `.config`,
  `.kube`, ... are checked on the absolute path (below the configured root that contains it), so
  `root=~/.ssh` is denied for every front-end and scan fallback, not only skipped during a walk
  from a parent.
- **Loopback only unless authenticated.** `serve` refuses to bind a non-loopback address unless
  `auth_token` (config) or `UNUMSEARCH_TOKEN` is set; with a token, requests need
  `Authorization: Bearer TOKEN` or `X-Unumsearch-Token`. `Host` must be a loopback name (DNS
  rebinding), request bodies are capped at 1 MiB and result counts and pattern sizes are clamped.

## License

Apache-2.0. See [LICENSE](LICENSE).
