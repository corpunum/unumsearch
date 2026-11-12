# Handover (living document)

Update this file with every change that alters state, open work or how to ship. Newest state on top.

## State (2026-11-12, morning)

- **Unreleased (queued 11-12):** `-h/--help` on every command and `files -m`. Rescan back-off:
  a clean watched unit is listed every 2nd, then every 4th rescan (`rescan_backoff_max`, default
  4, 1 = off), and a miss resets every unit; Windows and watcher-less setups are never backed off.
  Linux now also watches directories that hold no indexed file (the listing hands them to
  `attach`), which closed the main case only a rescan could catch. Test hooks: `watch::RESCAN_MS`,
  `RESCANS`, `RESCAN_SKIPS`, `DROP_WATCHES` (drops a watch silently, as a lost watch would).
- **Old branches dropped (11-12, corpunum-82's decision):** `s1/improve-v0.2.0` and its worktree
  `~/.openunum/worktrees/unumsearch-s1`, saved first in `~/openunum-backups/unumsearch-s1-20261112/`
  (3 patches, plus the uncommitted diff and untracked files). Ten stale local branches from
  10-09 to 10-12 were also removed; the 5 with commits not on main are saved as patches in
  `~/openunum-backups/unumsearch-branches-20261112/` (with `branch-tips.txt`). No remote
  branches besides `main`.
- **v0.1.11** (2026-11-12): `--json` accepted as a no-op; the writer's 30 s heartbeat writes the
  small `heartbeat` file in the index dir instead of rewriting `manifest.json` (800 KB on the rig).
  Readers take the later of the file and the manifest's `heartbeat_ms`; the manifest is still saved
  at every rescan (`Engine::checkpoint`), so older readers keep seeing the writer alive.
- **v0.1.10** (2026-11-11): `GET /health` (`ok`, `name`, `version`, `git_sha`, `started_at`) and
  the Windows watcher fix for #7 (every rescan re-adds each unit's watches and re-lists it; a
  watched unit is fresh only if listed within 2x `rescan_secs`). `tests/rearm.rs` forces the
  re-arm path on every OS (`watch::FORCE_REARM`, `watch::REARMS` are test hooks).
- CI: `ci.yml` (lint, tests on Linux x86/arm, macOS, Windows, static musl builds) and `release.yml`
  (on a `v*` tag). Docs-only pushes skip CI (`paths-ignore`).
- `build.rs` embeds the build commit as `UNUM_GIT_SHA`. Set `UNUM_GIT_SHA` in the environment to
  override it; outside a git checkout it is `unknown`.

## How to ship

1. Detached worktree of `origin/main`, change, then the gates:
   `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`.
   Also compile the tests for the other OSes before queuing (the targets are installed on the
   rig; a check needs no linker): `cargo check --tests --target x86_64-pc-windows-msvc` and
   `--target aarch64-apple-darwin`. Twice on 11-12 main went red on Windows only: a test that
   assumed Linux behaviour, then a test using `std::os::unix`. Full suites and builds run under
   `~/UnumOrchestrator/scripts/lease.sh run heavy-cpu -- <cmd>`.
2. One push to `main`; wait for CI to end green.
3. Release: bump `Cargo.toml` + `CHANGELOG.md`, tag `vX.Y.Z`, push the tag (`release.yml` builds
   the binaries). Tag by hand only a commit unumship has already shipped green (its rebased sha on
   `origin/main`, not the local one); unumship has no tag command yet. Then replace the installed binary and restart the daemon; check
   `curl 127.0.0.1:7781/health` shows the new `git_sha`.

## Open work

- **crates.io publish** (owner approved option a, 11-11): `unum-usage` 0.1.0 first. Its
  `cargo publish --dry-run` is clean with `~/.cargo/bin` cargo (the system cargo cannot read the
  v4 lock file); the names `unum-usage` and `unumsearch` were free on 11-11. Owner step:
  `cargo login`, then `cd ~/unum-usage && cargo publish`. Then add `version = "0.1"` next to the
  git pin of `unum-usage` here, re-run `cargo publish --dry-run`, and the unumsearch publish is a
  second owner step. Without that, the dry-run fails: "dependency `unum-usage` does not specify a
  version".
- **notify 9**: its Windows backend re-arms after an overflow and emits a `Rescan` event (which
  the event loop already maps to `mark_all_dirty`). Once 9.0 is stable (rc.8 on 11-11, MSRV 1.91),
  upgrading may let the per-rescan re-arm go; keep the freshness bound until verified on Windows.
- Delta shard for a changed file instead of a base rewrite (the remaining part of the
  hot-directory work; back-off shipped in 0.1.9, the heartbeat file in 0.1.11).
- crates.io: after each release, `cargo publish` from the shipped commit (the crate records the
  commit it was built from). Check the version numbers first: a version cannot be re-uploaded.
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
