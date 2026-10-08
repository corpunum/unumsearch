# Gemini CLI

Verified against the [Gemini CLI MCP docs](https://geminicli.com/docs/tools/mcp-server/);
registration smoke-tested with Gemini CLI 0.45 (`gemini mcp add` wrote the entry below and
`gemini mcp list` showed it; a folder must be trusted before project servers are enabled).

```bash
gemini mcp add unumsearch unumsearch mcp               # project scope: .gemini/settings.json
gemini mcp add -s user unumsearch unumsearch mcp       # user scope: ~/.gemini/settings.json
gemini mcp list
```

`settings.json` equivalent:

```json
{
  "mcpServers": {
    "unumsearch": { "command": "unumsearch", "args": ["mcp"] }
  }
}
```

Keep the server name free of underscores (the Gemini docs warn against them).
