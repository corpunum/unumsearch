# OpenUnum

[OpenUnum](https://github.com/corpunum/openunum) is an autonomous local agent framework. It has
first-class unumsearch support: its built-in search tools (`file_search`, `file_grep` and the
workspace retrieval provider) ask a running unumsearch daemon which files can match, then read
and match those files themselves. Result format and match semantics are identical with or
without the index; the daemon only removes the directory walk.

**Status:** supported since OpenUnum's fast-search backend landed. OpenUnum installers ship
unumsearch and turn the backend on by default (rolling out; until your install includes it,
enable it as below).

## How it works

- OpenUnum talks to the daemon over its local HTTP API (`GET /search?candidates_only=1`,
  `GET /files`), default `http://127.0.0.1:7781`, with a short timeout (1.5 s).
- **Automatic fallback.** Any doubt returns to the built-in implementation: daemon absent or
  slow, directory not covered by the index, any covering unit not fresh, truncated candidate
  list, or a regex the index cannot plan. Turning the backend on can never make a search fail.
- Corpus: the index follows ripgrep's rules (`.gitignore` respected, secrets and build output
  never indexed). With the backend on, OpenUnum searches what ripgrep would search.
- `openunum doctor` reports a `fast_search_backend` check (a warning, not a failure, when the
  daemon is absent).

## Setup (installers do this for you)

1. Install the binary and run the daemon as a user service (see the main README): the OpenUnum
   installers download the release binary, verify its SHA256, and install a user service
   (systemd user unit on Linux, launchd agent on macOS, a scheduled task on Windows) with a
   memory cap.
2. Point it at the OpenUnum workspaces. Example `config.toml`:

   ```toml
   # One unit per agent workspace / worktree, so a change rebuilds one checkout only.
   roots = ["~/.openunum/workspaces", "~/.openunum/worktrees"]
   split_roots = ["~/.openunum/workspaces", "~/.openunum/worktrees"]
   max_memory_mb = 256
   ```

3. Enable the backend in OpenUnum's config (on by default in installs that ship unumsearch):

   ```json
   { "runtime": { "fastSearch": { "enabled": true, "url": "http://127.0.0.1:7781", "timeoutMs": 1500 } } }
   ```

   Or with environment variables: `OPENUNUM_FAST_SEARCH=1`, `OPENUNUM_FAST_SEARCH_URL=...`.

To turn it off, set `runtime.fastSearch.enabled` to `false`; the search tools behave exactly as
before.
