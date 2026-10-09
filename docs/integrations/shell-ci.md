# Plain shell and CI

The CLI needs no daemon: without an index it scans directly with ripgrep's corpus rules
(`"backend": "scan"`). Output is JSON by default; `--text` gives `path:line:text`.

```bash
unumsearch search --text -i 'deprecated_api' .
unumsearch search -l 'TODO|FIXME' src -g '*.rs'      # files only
unumsearch files . -g '*.proto'
unumsearch search -F 'BEGIN PRIVATE KEY' . | jq -r '.result.files[]'
```

Exit status: JSON output exits 0 on success (also when nothing matches: check
`.result.files`); `--text` exits 1 when nothing matches, like grep; errors exit 2.

## GitHub Actions

```yaml
- name: Install unumsearch
  run: |
    curl -fsSL https://github.com/corpunum/unumsearch/releases/latest/download/install.sh | sh
    echo "$HOME/.local/bin" >> "$GITHUB_PATH"
- name: No leftover debug prints
  run: |
    n=$(unumsearch search -l 'dbg!\(' src | jq '.result.files | length')
    test "$n" -eq 0
```

Pin a version with `UNUMSEARCH_VERSION=v0.1.5` for reproducible builds. On Windows runners use
`install.ps1` (see the main README).
