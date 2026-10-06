#!/usr/bin/env bash
# Play Doom on DCG (stunts/doom-dcg) on Fogo testnet, from this machine.
#
#   scripts/doom_dcg_play.sh [SECONDS]        # default 300
#
# Starts the Node relayer (stunts/doom-relayer/server.mjs) with the DCG
# pipeline (stunts/doom-dcg/relay_pipeline.py) on 127.0.0.1, serves the page,
# and opens Chrome on web/doom-play with the testnet-dcg network. Ctrl-C stops
# everything; the pipeline writes its receipt under the run directory.
#
# Environment:
#   DOOM_DCG_RUN_DIR   the lanes driver's run directory (driver-config.json, keys)
#                      default: out/runs/doom-dcg-demo-2026-10-06
#   DCG_PYTHON         a Python with DCG's packages (solders, httpx, ...)
#   SPEND_CAP          testnet FOGO cap for the session (default 2)
#   DOOM_HEADLESS=1    headless Chrome
#   CHROME             the Chrome binary
set -euo pipefail

SESSION_SECONDS=${1:-300}
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
MAIN_OUT=${MAIN_OUT:-$REPO/out}
DOOM_DCG_RUN_DIR=${DOOM_DCG_RUN_DIR:-$MAIN_OUT/runs/doom-dcg-demo-2026-10-06}
DCG_ROOT=${DCG_ROOT:-$(CDPATH= cd -- "$REPO/../durable-compute-graph" && pwd -P)}
DCG_PYTHON=${DCG_PYTHON:-$DCG_ROOT/python/.venv/bin/python}
SPEND_CAP=${SPEND_CAP:-2}
RELAY_PORT=${RELAY_PORT:-18787}
PAGE_PORT=${PAGE_PORT:-18000}
CHROME_DEBUG_PORT=${CHROME_DEBUG_PORT:-19222}
CHROME=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
RPC=${RPC:-http://64.130.44.172:8899}
# The sealed RI2/WAD resource source account the session was provisioned from.
export DOOM_RESOURCE_SOURCE=${DOOM_RESOURCE_SOURCE:-G9rRwB544pdp3LaeiJcKgcXdRTs6qb434sbsUyAX5sRG}
export DOOM_DCG_RUN_DIR

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
RUN="$DOOM_DCG_RUN_DIR/play-$STAMP"
mkdir -p "$RUN"
KEYS_DIR=$("$DCG_PYTHON" -c "import json,os,sys;c=json.load(open(sys.argv[1]));print(os.path.dirname(c['key_paths']['payer']))" "$DOOM_DCG_RUN_DIR/driver-config.json")
PAYER_KEY="$KEYS_DIR/payer.json"
PROFILE_DIR=$(mktemp -d /private/tmp/doom-dcg-chrome-XXXXXX)
URL="http://127.0.0.1:${PAGE_PORT}/web/doom-play/?net=testnet-dcg&rpc=${RPC}&relayer=http://127.0.0.1:${RELAY_PORT}&frame-source=push"
PIDS=()
mark() { echo "[doom-dcg ${SECONDS}s] $*"; }

cleanup() {
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  sleep 2
  for pid in "${PIDS[@]}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$PROFILE_DIR"
  mark "stopped; receipt: $RUN/relayer-session.json"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mark "relayer on 127.0.0.1:$RELAY_PORT (session from $DOOM_DCG_RUN_DIR)"
PYTHONPATH="$REPO/stunts/doom-dcg:$DCG_ROOT/python" node "$REPO/stunts/doom-relayer/server.mjs" \
  --mode chain --network testnet-dcg --port "$RELAY_PORT" --host 127.0.0.1 \
  --keys-dir "$KEYS_DIR" --payer-key "$PAYER_KEY" \
  --python "$DCG_PYTHON" --script "$REPO/stunts/doom-dcg/relay_pipeline.py" \
  --run-dir "$RUN" --seconds "$SESSION_SECONDS" --spend-cap-fogo "$SPEND_CAP" \
  > "$RUN/relayer.log" 2>&1 &
PIDS+=("$!")
python3 -m http.server "$PAGE_PORT" --bind 127.0.0.1 --directory "$REPO" >/dev/null 2>&1 &
PIDS+=("$!")
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:$RELAY_PORT/healthz" >/dev/null 2>&1 && break
  sleep 0.5
done
mark "opening $URL"
node "$REPO/scripts/doom_play_browser.mjs" "$URL" "$PROFILE_DIR" "$CHROME_DEBUG_PORT" "$CHROME" "${DOOM_HEADLESS:-0}" "$(($(date +%s) * 1000))" &
PIDS+=("$!")
mark "playing for ${SESSION_SECONDS}s: arrows move, Ctrl or Space fires, R uses (and respawns), Shift runs"
END=$((SECONDS + SESSION_SECONDS + 30))
while [ "$SECONDS" -lt "$END" ]; do
  if ! kill -0 "${PIDS[0]}" 2>/dev/null; then
    mark "the relayer exited; see $RUN/relayer.log"
    break
  fi
  sleep 1
done
