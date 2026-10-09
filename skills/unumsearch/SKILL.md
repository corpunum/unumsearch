---
name: unumsearch
description: Fast indexed search over local code and files with the `unumsearch` CLI. Use it instead of grep/rg/find for content or filename searches in large directories (many checkouts, monorepos) when an index is configured; answers in milliseconds and reports whether the index is fresh.
---

# unumsearch

`unumsearch` keeps a compact trigram index of configured directories fresh in
the background (`unumsearch serve`, or a system service) and answers content
and filename queries from it. Output is JSON on stdout.

## When to use

- Searching file contents across big trees (`grep -r`, `rg`) -> `unumsearch search`.
- Finding files by name or glob (`find -name`) -> `unumsearch files`.
- Directories outside the index still work: they are scanned directly
  (`"backend": "scan"`), with the same corpus rules.

## Commands

```bash
# Content search: regex by default (ripgrep syntax), -F for a literal string.
unumsearch search 'fn\s+main' /path/to/repo
unumsearch search -F 'config.load(' /path/to/repo -i
unumsearch search -l 'TODO' /path/to/repo -g '*.rs' -g '!tests/**'   # files only, with globs
unumsearch search --text 'needle' /path/to/repo                        # path:line:text output

# Filename search: globs and/or a regex on the relative path or basename.
unumsearch files /path/to/repo -g '*.toml'
unumsearch files /path/to/repo --regex '(^|/)Dockerfile$'

# Index state.
unumsearch status
```

## Reading results

`search` returns `{"ok":true,"result":{...}}` with:

- `matches`: `[{path, line, text}]` (absent with `-l`), `files`: matching files.
- `backend`: `index`, `scan` (directory not indexed), or `none`.
- `fresh`: `true` when every covering index unit is up to date. When `false`,
  recent edits may be missing: re-run shortly or use `rg` for that directory.
- `truncated`: more results exist than returned (`-m N` raises the cap).
- `covered`: `false` when the index does not hold the whole directory (for
  example a gitignored or excluded tree); `uncovered` lists those paths with a
  `reason`. For `excluded`/`not_indexed`, scan them (`rg`), unless `backend`
  is already `scan`. For `secret`, do not search them with any tool.
- `complete`: `covered && fresh && !truncated`; only then is an empty answer
  proof that nothing matches.

## Corpus rules (same as ripgrep)

`.gitignore`/`.ignore` respected, hidden files included, binary files (NUL
byte) and files above `max_file_size` skipped. Build output, dependency trees,
media and archives are excluded by default; credentials (`.env*`, `*.key`,
`*.pem`, `id_rsa*`, `.ssh/`, `secrets.json`, ...) are never indexed.

## MCP

`unumsearch mcp` serves the same capabilities over the Model Context Protocol
(tools `search`, `find_files`, `index_status`), e.g.
`claude mcp add unumsearch -- unumsearch mcp`.
