# Handover (living document)

Update this file with every change that alters state, open work or how to ship. Newest state on top.

## State (2026-11-11)

- Released: **v0.1.9** (2026-11-10, hot directories). `main` is one change ahead: `GET /health`
  (`ok`, `name`, `version`, `git_sha`, `started_at`), see `CHANGELOG.md` "Unreleased".
- CI: `ci.yml` (lint, tests on Linux x86/arm, macOS, Windows, static musl builds) and `release.yml`
  (on a `v*` tag). Docs-only pushes skip CI (`paths-ignore`).
- `build.rs` embeds the build commit as `UNUM_GIT_SHA`. Set `UNUM_GIT_SHA` in the environment to
  override it; outside a git checkout it is `unknown`. `cargo publish --dry-run` stays clean.

## How to ship

1. Detached worktree of `origin/main`, change, then the gates:
   `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`.
2. One push to `main`; wait for CI to end green.
3. Release: bump `Cargo.toml` + `CHANGELOG.md`, tag `vX.Y.Z`, push the tag (`release.yml` builds
   the binaries). Then replace the installed binary and restart the daemon; check
   `curl 127.0.0.1:7781/health` shows the new `git_sha`.

## Open work

- **#7** Windows watcher drops buffer overflows silently. Blocks 0.2.0 and the crates.io publish
  (the publish itself is an owner step: `cargo login` + `cargo publish`).
- **crates.io publish is also blocked by the `unum-usage` dependency**: it has only a git source,
  and `cargo publish --dry-run` fails with "dependency `unum-usage` does not specify a version".
  Either publish `unum-usage` first and add `version = "0.1"` next to the git pin, or put the
  counters behind an optional feature that the published crate leaves off.
- Delta shard for a changed file instead of a base rewrite, and the heartbeat out of
  `manifest.json` (the remaining parts of the hot-directory work; back-off shipped in 0.1.9).
- Release that carries `/health` (0.1.10 or 0.2.0), then deploy it so monitors can compare the
  running build with `main`.
- `/health` sits behind the same token rule as the other routes when `auth_token` is set.

## Traps

- The 4-OS matrix runs on every non-docs push; bundle changes into one push.
- CI installs ripgrep on Linux from the upstream release tarball (pinned 14.1.1), with apt as the
  fallback. On 2026-11-11 the apt mirrors did not resolve from the runners and both Linux test jobs
  failed in the install step, twice.
- `tests/hot_unit.rs` and the trust tests are timing-sensitive. `hot_unit` assumed the initial
  index takes a few ms; on a slow Windows runner (2026-11-11) the first rebuild was then not due
  within the test's 5 s. `setup` now waits out the initial interval. If it flakes again, look at
  the fixed 1.3 s / 1.5 s windows in the same file.
