# OpenClaw

OpenClaw supports both MCP servers and Agent Skills. Verified against the OpenClaw docs
([MCP registry](https://docs.openclaw.ai/cli/mcp/registry), [skills](https://docs.openclaw.ai/tools/skills))
and against a working OpenClaw configuration (an existing stdio server entry under
`mcp.servers` with `command`/`args`). Not smoke-tested end to end.

## MCP server

```bash
openclaw mcp set unumsearch '{"command":"unumsearch","args":["mcp"]}'
openclaw mcp list
```

This stores the definition under `mcp.servers` in the OpenClaw config (`openclaw.json`):

```json
{
  "mcp": {
    "servers": {
      "unumsearch": { "command": "unumsearch", "args": ["mcp"] }
    }
  }
}
```

Remove it with `openclaw mcp unset unumsearch`.

## Skill

Copy `skills/unumsearch/` into one of OpenClaw's skill directories, for example
`<workspace>/skills/unumsearch/` (one agent workspace) or `~/.agents/skills/unumsearch/` (shared
with other harnesses that read `~/.agents/skills`).
