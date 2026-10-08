# Editors and IDE agents (Cursor, VS Code / GitHub Copilot, Windsurf, Cline, Roo Code, Continue, Zed)

All of these launch MCP servers over stdio, so the server definition is always the same command,
`unumsearch mcp`, in the editor's own config format. Each snippet below was checked against the
editor's current documentation (October 2026); the editors themselves were not available on the
test machine, so none of them was smoke-tested end to end. The server itself is the one
smoke-tested with Claude Code, Codex CLI and OpenCode.

If `unumsearch` is not on the editor's `PATH` (GUI apps often get a minimal one), use the
absolute path to the binary as `command`.

## Cursor

[Docs](https://cursor.com/docs/context/mcp). `.cursor/mcp.json` in a project, or `~/.cursor/mcp.json`
for all projects:

```json
{
  "mcpServers": {
    "unumsearch": { "type": "stdio", "command": "unumsearch", "args": ["mcp"] }
  }
}
```

## VS Code (GitHub Copilot agent mode)

[Docs](https://code.visualstudio.com/docs/copilot/customization/mcp-servers). `.vscode/mcp.json`
in a workspace (note the top-level key is `servers`, not `mcpServers`):

```json
{
  "servers": {
    "unumsearch": { "type": "stdio", "command": "unumsearch", "args": ["mcp"] }
  }
}
```

Or add it to your user profile from a shell:

```bash
code --add-mcp '{"name":"unumsearch","command":"unumsearch","args":["mcp"]}'
```

## Windsurf

**Partly unverified.** Windsurf's documentation now redirects to the Devin desktop docs, which
give `~/.config/devin/mcp_config.json` (`%APPDATA%\devin\mcp_config.json` on Windows); existing
Windsurf installs and most guides use `~/.codeium/windsurf/mcp_config.json`. Open the right file
from the app (Settings, Cascade, MCP servers, "View raw config") and add:

```json
{
  "mcpServers": {
    "unumsearch": { "command": "unumsearch", "args": ["mcp"] }
  }
}
```

## Cline

[Docs](https://docs.cline.bot/mcp/configuring-mcp-servers). In the extension: MCP Servers icon,
Configure, "Configure MCP Servers" (opens `cline_mcp_settings.json`); the Cline CLI uses
`~/.cline/data/settings/cline_mcp_settings.json`. All three tools are read-only, so they can be
auto-approved:

```json
{
  "mcpServers": {
    "unumsearch": {
      "command": "unumsearch",
      "args": ["mcp"],
      "disabled": false,
      "autoApprove": ["search", "find_files", "index_status"]
    }
  }
}
```

Cline CLI: `cline mcp add unumsearch --yes -- unumsearch mcp`.

## Roo Code

[Docs](https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo). Global
`mcp_settings.json` ("Edit Global MCP") or project `.roo/mcp.json` ("Edit Project MCP"):

```json
{
  "mcpServers": {
    "unumsearch": {
      "command": "unumsearch",
      "args": ["mcp"],
      "alwaysAllow": ["search", "find_files", "index_status"],
      "disabled": false
    }
  }
}
```

## Continue

[Docs](https://docs.continue.dev/customize/deep-dives/mcp). A block file
`.continue/mcpServers/unumsearch.yaml`:

```yaml
name: unumsearch
version: 0.1.0
schema: v1
mcpServers:
  - name: unumsearch
    type: stdio
    command: unumsearch
    args:
      - mcp
```

(In `config.yaml`, put just the `mcpServers:` list.) MCP tools are available in agent mode.

## Zed

[Docs](https://zed.dev/docs/ai/mcp). In `settings.json`:

```json
{
  "context_servers": {
    "unumsearch": { "command": "unumsearch", "args": ["mcp"] }
  }
}
```

Or Settings, AI, MCP Servers, Add Server, Add Local Server.
