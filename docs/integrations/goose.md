# Goose

Verified against the [Goose extensions docs](https://goose-docs.ai/docs/getting-started/using-extensions)
(not smoke-tested: Goose is not installed on the test machine).

Interactive: `goose configure`, Add Extension, Command-line Extension, command `unumsearch mcp`.

Or `~/.config/goose/config.yaml` (Windows: `%APPDATA%\Block\goose\config\config.yaml`):

```yaml
extensions:
  unumsearch:
    name: unumsearch
    type: stdio
    cmd: unumsearch
    args: [mcp]
    enabled: true
    timeout: 300
```

One session only: `goose session --with-extension "unumsearch mcp"`.
