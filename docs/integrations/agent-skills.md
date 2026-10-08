# Agent Skills consumers

[`skills/unumsearch/SKILL.md`](../../skills/unumsearch/SKILL.md) follows the
[Agent Skills specification](https://agentskills.io/specification) (name matches the folder,
lowercase, description under 1024 characters). It teaches an agent to call the `unumsearch` CLI
through its shell tool, so it works in any harness that reads skills, with or without MCP.

Copy the folder (from a checkout or any release archive) into the harness's skills directory:

| Harness | User-level | Project-level |
| --- | --- | --- |
| Claude Code | `~/.claude/skills/` | `.claude/skills/` |
| Codex CLI | `~/.agents/skills/` | `.agents/skills/` |
| Pi | `~/.pi/agent/skills/` or `~/.agents/skills/` | `.pi/skills/` or `.agents/skills/` |
| OpenClaw | `~/.agents/skills/` | `<workspace>/skills/` |
| Others | see the harness docs | |

`~/.agents/skills/` is shared by several harnesses, so one copy there covers all of them.
