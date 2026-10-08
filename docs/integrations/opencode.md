# OpenCode

Verified against the [OpenCode MCP docs](https://opencode.ai/docs/mcp-servers/) and
[config docs](https://opencode.ai/docs/config/); smoke-tested with OpenCode 1.18
(`opencode mcp list` reported the server `connected`).

`opencode.json` in the project root, or `~/.config/opencode/opencode.json` globally:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "unumsearch": { "type": "local", "command": ["unumsearch", "mcp"], "enabled": true }
  }
}
```

Check with `opencode mcp list`.
