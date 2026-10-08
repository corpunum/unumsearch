# HTTP API and framework tool wrappers (LangChain, LlamaIndex, custom agents)

`unumsearch serve` (or the user service) answers on `http://127.0.0.1:7781`:
`GET /status`, `GET /search`, `GET /files`, `POST /rpc` (JSON-RPC 2.0). Responses are
`{"ok": true, "result": {...}}`. The API is loopback-only and unauthenticated; do not expose it.
The parameter set is stable; new fields are only ever added.

## A plain Python tool (standard library only)

Tested against a running daemon.

```python
import json, urllib.parse, urllib.request

UNUMSEARCH = "http://127.0.0.1:7781"

def unumsearch(pattern: str, root: str, regex: bool = False, glob: str = "", max_matches: int = 50) -> str:
    """Search file contents under `root` with unumsearch. Returns path:line:text lines."""
    q = {"pattern": pattern, "root": root, "mode": "regex" if regex else "literal",
         "max_matches": max_matches}
    if glob:
        q["glob"] = glob          # comma-separated ripgrep globs, e.g. "*.py,!tests/**"
    url = f"{UNUMSEARCH}/search?{urllib.parse.urlencode(q)}"
    with urllib.request.urlopen(url, timeout=10) as r:
        res = json.load(r)["result"]
    lines = [f'{m["path"]}:{m["line"]}:{m["text"]}' for m in res.get("matches", [])]
    if not res.get("fresh", True) or res.get("truncated"):
        lines.append("(note: results may be incomplete; index not fresh or truncated)")
    return "\n".join(lines) or "no matches"
```

## Wrapping it for a framework

These wrappers only register the function above; they were written against the frameworks'
documented tool APIs but not executed here.

```python
# LangChain
from langchain_core.tools import tool
unumsearch_tool = tool(unumsearch)

# LlamaIndex
from llama_index.core.tools import FunctionTool
unumsearch_tool = FunctionTool.from_defaults(fn=unumsearch)
```

Frameworks with MCP adapters (for example `langchain-mcp-adapters`) can instead launch
`unumsearch mcp` directly; see [generic-mcp.md](generic-mcp.md).

## Many lookups at once

To ground many references (identifiers, file names, literals) across every indexed root, send
them in one `lookup` call instead of one request each; it runs inside the daemon and returns one
entry per pattern:

```bash
curl -s -X POST http://127.0.0.1:7781/rpc -d '{"jsonrpc":"2.0","id":1,"method":"lookup",
  "params":{"patterns":["parse_config","FooBar"],"all_roots":true,"max_files":5}}'
# GET form: /lookup?all_roots=1&patterns=parse_config%0AFooBar
```

`search` also takes `all_roots=1` to search every configured root in one call.

## JSON-RPC

```bash
curl -s -X POST http://127.0.0.1:7781/rpc \
  -d '{"jsonrpc":"2.0","id":1,"method":"files","params":{"root":"/src/repo","glob":"*.toml"}}'
echo '{"jsonrpc":"2.0","id":1,"method":"search","params":{"pattern":"TODO","root":"/src/repo"}}' \
  | unumsearch rpc          # same methods over stdio, no daemon needed
```

## Freshness contract

If `fresh` is false, `covered` is false or `truncated` is true, treat the answer as possibly
incomplete and fall back to your own scan (this is what OpenUnum does). With
`candidates_only=1` the daemon returns only the files that can match, for clients that verify
with their own regex dialect.
