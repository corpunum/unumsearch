# Aider

Aider has no MCP client (MCP support is an open feature request), so use the CLI. Verified
against the [aider command reference](https://aider.chat/docs/usage/commands.html).

In a chat, run a search and feed the results to the model:

```text
/run unumsearch search --text -i 'parse_config' .
/run unumsearch files . -g '*.toml'
```

`/run` (alias `!`) adds the command output to the chat. `--text` prints compact
`path:line:text` lines, which is what you want in a prompt.

To have the model know about the tool, add a conventions file that it always reads
(`aider --read CONVENTIONS.md`) with a line such as:

```markdown
For code search, suggest `unumsearch search --text PATTERN DIR` (regex; `-F` literal, `-i`
ignore case, `-g GLOB`) and `unumsearch files DIR -g GLOB` instead of grep or find.
```
