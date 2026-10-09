# Hermes Agent

Verified against the [Hermes Agent MCP docs](https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp)
(not smoke-tested: Hermes Agent is not installed on the test machine). MCP support ships with
the standard Hermes install.

Hermes reads MCP servers from `~/.hermes/config.yaml` under `mcp_servers`; a stdio server takes
`command` and `args`:

```yaml
mcp_servers:
  unumsearch:
    command: unumsearch
    args: [mcp]
```

Run `/reload-mcp` in a session (or start a new one), or check the server with
`hermes mcp test unumsearch`. The tools appear as `mcp_unumsearch_search`,
`mcp_unumsearch_find_files` and `mcp_unumsearch_index_status`; limit them with the per-server
`tools: {include: [...]}` / `exclude` lists if needed.

Start the daemon (`unumsearch serve` or the user service) so searches use a fresh index; without
it `unumsearch mcp` still answers, by scanning directories it has no index for.
