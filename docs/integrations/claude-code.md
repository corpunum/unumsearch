# Claude Code

Verified against the [Claude Code MCP docs](https://code.claude.com/docs/en/mcp) and smoke-tested
with Claude Code 2.1 on Linux (server reported `Connected`; a headless `claude -p` session
called the `search` tool and returned the right file and line; the skill was loaded and used in
a second session).

## MCP server

```bash
claude mcp add unumsearch -- unumsearch mcp                    # this project, only you
claude mcp add --scope user unumsearch -- unumsearch mcp       # every project
claude mcp add --scope project unumsearch -- unumsearch mcp    # shared via .mcp.json
claude mcp get unumsearch                                      # shows "Connected"
```

Everything after `--` is the server command. The project-scoped `.mcp.json` equivalent:

```json
{
  "mcpServers": {
    "unumsearch": { "type": "stdio", "command": "unumsearch", "args": ["mcp"] }
  }
}
```

Tools appear as `mcp__unumsearch__search`, `mcp__unumsearch__find_files` and
`mcp__unumsearch__index_status`; all three are read-only, so they are safe to pre-approve
(`--allowedTools mcp__unumsearch__search` or `permissions.allow` in settings).

No daemon running? Use `["mcp", "--watch"]` so the MCP server maintains the index itself.

## Skill (no MCP needed)

```bash
mkdir -p ~/.claude/skills
cp -r skills/unumsearch ~/.claude/skills/        # from a checkout or a release archive
```

Project-local alternative: `.claude/skills/unumsearch/`. The skill teaches the agent to run the
`unumsearch` CLI through Bash; allow it with `Bash(unumsearch:*)`.
