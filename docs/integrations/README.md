# Integrations

unumsearch is harness-agnostic: one binary exposes the same search through a CLI, an MCP server
(`unumsearch mcp`), a local HTTP/JSON API, stdio JSON-RPC and an Agent Skill. Pick whichever your
tool speaks.

Status legend: **tested** = run end to end on Linux with the tool's current release;
**registered** = the tool accepted and listed/connected the server, no model turn run;
**docs** = snippet checked against the tool's current official docs only;
**partly unverified** = see the page.

| Harness | Interface | Status | Guide |
| --- | --- | --- | --- |
| [OpenUnum](https://github.com/corpunum/openunum) | built-in fast-search backend (HTTP API) | tested (OpenUnum test suite) | [openunum.md](openunum.md) |
| Claude Code | MCP + Agent Skill | tested (MCP tool call and skill) | [claude-code.md](claude-code.md) |
| OpenAI Codex CLI | MCP (+ Agent Skill) | tested (MCP tool call) | [codex.md](codex.md) |
| Gemini CLI | MCP | registered | [gemini-cli.md](gemini-cli.md) |
| OpenCode | MCP | registered (connected) | [opencode.md](opencode.md) |
| Cursor | MCP | docs | [editors.md](editors.md#cursor) |
| VS Code / GitHub Copilot agent mode | MCP | docs | [editors.md](editors.md#vs-code-github-copilot-agent-mode) |
| Windsurf | MCP | partly unverified (config path) | [editors.md](editors.md#windsurf) |
| Cline | MCP | docs | [editors.md](editors.md#cline) |
| Roo Code | MCP | docs | [editors.md](editors.md#roo-code) |
| Continue | MCP | docs | [editors.md](editors.md#continue) |
| Zed | MCP | docs | [editors.md](editors.md#zed) |
| Goose | MCP | docs | [goose.md](goose.md) |
| Hermes Agent | MCP | docs | [hermes.md](hermes.md) |
| OpenClaw | MCP + Agent Skill | docs + checked against a working config | [openclaw.md](openclaw.md) |
| Pi | Agent Skill (Pi has no MCP by design) | docs (bundled with pi 0.84) | [pi.md](pi.md) |
| Aider | CLI via `/run` (no MCP client) | docs | [aider.md](aider.md) |
| Any MCP client | MCP | protocol handshake tested | [generic-mcp.md](generic-mcp.md) |
| Agent Skills consumers | Agent Skill | spec-checked | [agent-skills.md](agent-skills.md) |
| LangChain, LlamaIndex, custom agents | HTTP API / JSON-RPC | plain Python tool tested; framework wrappers docs | [http-api.md](http-api.md) |
| Shell scripts, CI | CLI (JSON or `--text`) | tested | [shell-ci.md](shell-ci.md) |

Snippets were last checked in October 2026. Harness config formats change; if one of these
drifts, the server side does not: `command: unumsearch`, `args: ["mcp"]`, stdio.
