#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Reproduce the README benchmark without touching a production daemon or index:
#   1. build a PRIVATE index of your config twice (build time, peak RSS, size on disk),
#   2. serve it from a PRIVATE daemon on its own port (no memory cap; peak RSS recorded),
#   3. race rg / daemon / CLI over one repository and over every root,
#   4. cold page cache: rg vs the one-shot CLI,
#   5. summarise into $OUT/summary.json.
# Env: CONFIG (default: the platform config), BIN (unumsearch), RG (rg), OUT (./run-<date>),
#      PORT (7791), REPO (default: first configured root), LOCK (optional flock file for
#      machines that serialise heavy jobs). Needs python 3.11+, /usr/bin/time, curl.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
export CONFIG=${CONFIG:-$(python3 -c "import sys; sys.path.insert(0, '$HERE'); from common import default_config; print(default_config())")}
export BIN=${BIN:-unumsearch} RG=${RG:-rg} PORT=${PORT:-7791}
export OUT=${OUT:-$PWD/run-$(date +%Y%m%d)} HERE
export PRIV=$OUT/private-index REPO=${REPO:-}
mkdir -p "$OUT"
run() {
  set -euo pipefail
  snap() { { date -Is; free -m 2>/dev/null || true; uptime; } > "$OUT/env-$1.txt"; }
  snap start
  for i in 1 2; do
    rm -rf "$PRIV"
    /usr/bin/time -v "$BIN" --config "$CONFIG" --index-dir "$PRIV" index --force > "$OUT/build-$i.json" 2> "$OUT/build-$i.time"
    du -sb "$PRIV" > "$OUT/build-$i.du"
  done
  "$BIN" --config "$CONFIG" --index-dir "$PRIV" --listen 127.0.0.1:$PORT serve > "$OUT/serve.log" 2>&1 &
  SP=$!
  trap 'kill $SP 2>/dev/null || true' EXIT
  for _ in $(seq 1 300); do
    curl -sf 127.0.0.1:$PORT/status | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin)["fresh"] else 1)' 2>/dev/null && break
    sleep 1
  done
  common=(--config "$CONFIG" --index-dir "$PRIV" --url http://127.0.0.1:$PORT --rg "$RG" --bin "$BIN" --pid $SP)
  python3 "$HERE/race_bench.py" --set repo --runs 5 --repeat 3 "${common[@]}" ${REPO:+--repo "$REPO"} --out "$OUT/raw-repo.json" > "$OUT/log-repo.txt"
  python3 "$HERE/race_bench.py" --set tree --runs 3 --repeat 3 "${common[@]}" --out "$OUT/raw-tree.json" > "$OUT/log-tree.txt"
  kill $SP; wait $SP 2>/dev/null || true
  snap after-tree
  python3 "$HERE/cold_bench.py" --config "$CONFIG" --index-dir "$PRIV" --rg "$RG" --bin "$BIN" --repeat 3 --out "$OUT/raw-cold.json" > "$OUT/log-cold.txt"
  snap end
  rm -rf "$PRIV"
  python3 "$HERE/summarize.py" "$OUT" > /dev/null
  echo "summary: $OUT/summary.json"
}
export -f run
if [ -n "${LOCK:-}" ]; then exec flock "$LOCK" bash -c run; else run; fi
