# Any MCP client

`unumsearch mcp` is a standard MCP server over stdio (protocol version `2025-06-18`, newline-
delimited JSON-RPC). Every client that can launch a local server needs the same two facts:

| Field | Value |
| --- | --- |
| command | `unumsearch` (or the absolute path to the binary) |
| args | `["mcp"]`, or `["mcp", "--watch"]` to maintain the index without a daemon |
| transport | stdio |
| env | none required; `UNUMSEARCH_CONFIG` selects a config file |

Most clients use the `mcpServers` JSON shape:

```json
{ "mcpServers": { "unumsearch": { "command": "unumsearch", "args": ["mcp"] } } }
```

## Tools

| Tool | Arguments | Notes |
| --- | --- | --- |
| `search` | `pattern` (required), `path`, `regex`, `ignore_case`, `glob` (array), `files_only`, `max_matches` | literal unless `regex: true`; result JSON includes `fresh`, `backend`, `truncated` |
| `find_files` | `path`, `glob` (array), `regex`, `max_files` | filename search |
| `index_status` | none | units, file counts, freshness |

All three are annotated `readOnlyHint: true`.

## Hand-rolled check

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"check","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | unumsearch mcp
```
