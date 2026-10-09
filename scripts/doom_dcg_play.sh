#!/usr/bin/env bash
# Play Doom on DCG (stunts/doom-dcg) on Fogo testnet, from this machine.
#
#   scripts/doom_dcg_play.sh [SECONDS]        # default 300
#
# Starts the Node relayer (stunts/doom-relayer/server.mjs) with the DCG
# pipeline (stunts/doom-dcg/relay_pipeline.py), serves the page, and opens
# Chrome on web/doom-play with the testnet-dcg network. Ctrl-C stops
# everything; the pipeline writes its receipt under the run directory.
#
# By default the relayer runs on the Tokyo box (DOOM_HOST, installed under
# ~/dcg-lane/v5 next to the testnet nodes: about 3.7 frames/s), reached through
# an SSH tunnel. DOOM_DCG_LOCAL=1 runs it on this machine instead (about 0.5
# frames/s: slow motion). The keys never leave the machine the relayer runs on.
#
# Environment:
#   DOOM_DCG_RUN_DIR   the lanes driver's run directory (driver-config.json, keys)
#                      default: out/runs/doom-dcg-demo-2026-10-06
#   DCG_PYTHON         a Python with the dcg package (default: this repo's .venv)
#   SPEND_CAP          testnet FOGO cap for the session (default 2)
#   DOOM_HEADLESS=1    headless Chrome
#   CHROME             the Chrome binary
set -euo pipefail

SESSION_SECONDS=${1:-300}
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
MAIN_OUT=${MAIN_OUT:-$REPO/out}
DOOM_DCG_RUN_DIR=${DOOM_DCG_RUN_DIR:-$MAIN_OUT/runs/doom-dcg-demo-2026-10-06}
DCG_PYTHON=${DCG_PYTHON:-$REPO/.venv/bin/python}
# About 0.7 testnet FOGO a minute at 3.7 frames/s (measured 2026-10-06).
SPEND_CAP=${SPEND_CAP:-10}
DOOM_HOST=${DOOM_HOST:-basanos-tokyo-2}
REMOTE_ROOT=${REMOTE_ROOT:-dcg-lane/v5}
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
  if [ "${DOOM_DCG_LOCAL:-0}" != 1 ] && declare -F remote_cleanup >/dev/null; then remote_cleanup; fi
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  sleep 2
  for pid in "${PIDS[@]}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$PROFILE_DIR"
  mark "stopped; receipt: $RUN/relayer-session.json"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "${DOOM_DCG_LOCAL:-0}" = 1 ]; then
  mark "relayer on 127.0.0.1:$RELAY_PORT (session from $DOOM_DCG_RUN_DIR)"
  PYTHONPATH="$REPO/stunts/doom-dcg" node "$REPO/stunts/doom-relayer/server.mjs" \
    --mode chain --network testnet-dcg --port "$RELAY_PORT" --host 127.0.0.1 \
    --keys-dir "$KEYS_DIR" --payer-key "$PAYER_KEY" \
    --python "$DCG_PYTHON" --script "$REPO/stunts/doom-dcg/relay_pipeline.py" \
    --run-dir "$RUN" --seconds "$SESSION_SECONDS" --spend-cap-fogo "$SPEND_CAP" \
    > "$RUN/relayer.log" 2>&1 &
  PIDS+=("$!")
else
  REMOTE_RUN="$REMOTE_ROOT/run/play-$STAMP"
  SSH=(ssh -o LogLevel=ERROR -o ConnectTimeout=15 -o BatchMode=yes)
  # One relayer at a time on the box: the R-series demo and this one share it.
  if "${SSH[@]}" "$DOOM_HOST" "pgrep -f '[s]tunts/doom-relayer/server.mjs|[d]oom_relayer_server.py|[r]elay_pipeline.py'" >/dev/null; then
    echo "a Doom relayer is already running on $DOOM_HOST; stop it first" >&2
    exit 1
  fi
  mark "relayer on $DOOM_HOST (~/$REMOTE_ROOT), tunnelled to 127.0.0.1:$RELAY_PORT"
  "${SSH[@]}" "$DOOM_HOST" "mkdir -p ~/$REMOTE_RUN && cd ~/$REMOTE_ROOT/doom && \
    DOOM_DCG_RUN_DIR=~/$REMOTE_ROOT/run DOOM_RESOURCE_SOURCE=$DOOM_RESOURCE_SOURCE \
    PYTHONPATH=~/$REMOTE_ROOT/doom/stunts/doom-dcg:~/$REMOTE_ROOT/dcg-python \
    timeout $((SESSION_SECONDS + 90)) node stunts/doom-relayer/server.mjs \
      --mode chain --network testnet-dcg --port 8787 --host 127.0.0.1 \
      --keys-dir ~/dcg-lane/v4/run/keys --payer-key ~/dcg-lane/v4/run/keys/payer.json \
      --python ~/dcg-lane/.venv/bin/python --script ~/$REMOTE_ROOT/doom/stunts/doom-dcg/relay_pipeline.py \
      --run-dir ~/$REMOTE_RUN --seconds $SESSION_SECONDS --spend-cap-fogo $SPEND_CAP \
      > ~/$REMOTE_RUN/relayer.log 2>&1" &
  PIDS+=("$!")
  "${SSH[@]}" -o ExitOnForwardFailure=yes -N -L "${RELAY_PORT}:127.0.0.1:8787" "$DOOM_HOST" &
  PIDS+=("$!")
  remote_cleanup() {
    "${SSH[@]}" "$DOOM_HOST" "pkill -INT -f '[s]tunts/doom-relayer/server.mjs --mode chain --network testnet-dcg' || true; sleep 3; \
      pkill -f '[r]elay_pipeline.py' || true" || true
    scp -q -o LogLevel=ERROR "$DOOM_HOST:$REMOTE_RUN/relayer-session.json" "$RUN/" 2>/dev/null || true
    scp -q -o LogLevel=ERROR "$DOOM_HOST:$REMOTE_RUN/relayer.log" "$RUN/" 2>/dev/null || true
  }
fi
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
