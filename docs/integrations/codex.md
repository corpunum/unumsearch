# OpenAI Codex CLI

Verified against the [Codex MCP docs](https://developers.openai.com/codex/mcp) and smoke-tested
with codex-cli 0.160 (`codex mcp add` wrote the entry below; `codex exec` called
`unumsearch/search` and returned the right file).

```bash
codex mcp add unumsearch -- unumsearch mcp
codex mcp list
```

Or edit `~/.codex/config.toml` directly:

```toml
[mcp_servers.unumsearch]
command = "unumsearch"
args = ["mcp"]
```

One-off, without touching the config:

```bash
codex exec -c 'mcp_servers.unumsearch.command="unumsearch"' \
           -c 'mcp_servers.unumsearch.args=["mcp"]' "find where FooBar is defined"
```

Codex also reads [Agent Skills](https://learn.chatgpt.com/docs/build-skills): copy
`skills/unumsearch/` into `~/.agents/skills/` (all repos) or `<repo>/.agents/skills/` to use the
CLI without MCP.
