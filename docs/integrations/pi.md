# Pi (pi coding agent)

Pi deliberately ships without MCP ("build CLI tools with READMEs" instead), which is exactly
what the unumsearch skill is. Verified against the docs bundled with pi 0.84 (`docs/skills.md`);
skill discovery not exercised with a live model.

```bash
mkdir -p ~/.pi/agent/skills
cp -r skills/unumsearch ~/.pi/agent/skills/
# or, shared with Codex/OpenClaw/OpenCode: ~/.agents/skills/unumsearch/
```

Project-local: `.pi/skills/unumsearch/` or `.agents/skills/unumsearch/` (loaded once the project
is trusted). One-off: `pi --skill path/to/skills/unumsearch`. Force it in a session with
`/skill:unumsearch`.

If you already keep skills in `~/.claude/skills`, point Pi at them in `~/.pi/agent/settings.json`:

```json
{ "skills": ["~/.claude/skills"] }
```

For MCP proper, install a pi extension that adds an MCP client; the server command is
`unumsearch mcp`.
