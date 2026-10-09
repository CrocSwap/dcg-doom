#!/usr/bin/env bash
# Public Doom on DCG (stunts/doom-dcg) on Tokyo: deploy, start, stop, status.
#
#   scripts/doom_dcg_public.sh deploy     # committed relayer, pipeline and page code -> ~/dcg-lane/v5/doom
#   DOOM_PUBLIC_ORIGIN=https://... scripts/doom_dcg_public.sh start
#   scripts/doom_dcg_public.sh status
#   scripts/doom_dcg_public.sh stop
#
# The DCG successor to scripts/doom_public.sh (the R-series public mode). The
# relayer (stunts/doom-relayer/server.mjs --public-mode) runs the DCG pipeline
# (stunts/doom-dcg/relay_pipeline.py) on the session in ~/dcg-lane/v5/run,
# bound to 127.0.0.1:8787; nothing is published. It shares Tokyo's Doom lock
# (/tmp/doom-play-maintenance.lock) with doom_public.sh, so only one public
# game runs at a time, and it refuses to start beside any running relayer.
#
# The spend cap (testnet FOGO, owner 2026-10-09: 1,000 with idle pause) holds
# for the whole public run: the relayer keeps the run's payer baseline and
# spend in DOOM_PUBLIC_STATE_PATH, and a restart with the same state file
# continues the same run and the same cap. An idle lobby pauses the pipeline
# (no frames, no spend) until the next accepted input.
#
# Environment:
#   DOOM_PUBLIC_ORIGIN       the exact HTTPS origin serving the page (or http://127.0.0.1:PORT for tests)
#   DOOM_PUBLIC_SPEND_CAP    testnet FOGO for the whole run, in (0, 1000]; default 1000
#   DOOM_PUBLIC_SECONDS      process duration, default 604800 (a week)
#   DOOM_PUBLIC_MAX_VIEWERS  1..200, default 200
#   DOOM_PUBLIC_STATE_PATH   the run's state file under ~/dcg-lane/v5/public-mode/
#   DOOM_PUBLIC_NAME         the run name (default public-<UTC time>)
#   DOOM_HOST                default basanos-tokyo-2
set -euo pipefail

ACTION=${1:-}
case "$ACTION" in deploy|start|stop|status) ;; *) echo "Usage: scripts/doom_dcg_public.sh deploy|start|stop|status" >&2; exit 2 ;; esac
REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
HOST=${DOOM_HOST:-basanos-tokyo-2}
ROOT=/home/ec2-user/dcg-lane/v5
LOCK=/tmp/doom-play-maintenance.lock
KEYS=/home/ec2-user/dcg-lane/v4/run/keys
PY=/home/ec2-user/dcg-lane/.venv/bin/python
RESOURCE_SOURCE=${DOOM_RESOURCE_SOURCE:-G9rRwB544pdp3LaeiJcKgcXdRTs6qb434sbsUyAX5sRG}
SSH=(ssh -o LogLevel=ERROR -o ConnectTimeout=15 -o BatchMode=yes -o ServerAliveInterval=5 -o ServerAliveCountMax=3)
# What counts as a running relayer, either series.
RELAYERS='[s]tunts/doom-relayer/server.mjs|[d]oom_relayer_server.py --run-dir|[r]elay_pipeline.py'
DEPLOY_PATHS=(stunts/doom-relayer stunts/doom-dcg web/doom-play scripts/doom_dcg_public_probe.mjs)

if [ "$ACTION" = deploy ]; then
  # Deploy committed code only: a fix in the working tree is not a fix on the
  # host (stunts/doom-relayer/start.sh says why).
  dirty=$(git -C "$REPO" status --porcelain -- "${DEPLOY_PATHS[@]}")
  if [ -n "$dirty" ]; then
    echo "uncommitted changes under ${DEPLOY_PATHS[*]}; commit them first:" >&2
    echo "$dirty" >&2
    exit 2
  fi
  commit=$(git -C "$REPO" rev-parse HEAD)
  if "${SSH[@]}" "$HOST" "pgrep -f '$RELAYERS'" >/dev/null; then
    echo "a Doom relayer is running on $HOST; stop it before deploying" >&2
    exit 3
  fi
  git -C "$REPO" archive --format=tar "$commit" -- "${DEPLOY_PATHS[@]}" \
    | "${SSH[@]}" "$HOST" "set -e; mkdir -p $ROOT/doom.next && tar -x -C $ROOT/doom.next \
        && for p in ${DEPLOY_PATHS[*]}; do rm -rf $ROOT/doom/\$p; mkdir -p \$(dirname $ROOT/doom/\$p); mv $ROOT/doom.next/\$p $ROOT/doom/\$p; done \
        && rm -rf $ROOT/doom.next && echo $commit > $ROOT/doom/DEPLOYED_COMMIT"
  echo "deployed $commit (${DEPLOY_PATHS[*]}) to $HOST:$ROOT/doom"
  exit 0
fi

if [ "$ACTION" = status ]; then
  "${SSH[@]}" "$HOST" bash -s -- "$LOCK" "$ROOT" "$RELAYERS" <<'STATUS'
set -eu
LOCK=$1; ROOT=$2; RELAYERS=$3
echo "lock_owner=$(cat "$LOCK/owner" 2>/dev/null || echo none)"
echo "run_name=$(cat "$LOCK/run_name" 2>/dev/null || echo none)"
echo "deployed_commit=$(cat "$ROOT/doom/DEPLOYED_COMMIT" 2>/dev/null || echo unknown)"
echo "relayer_pids=$(pgrep -f "$RELAYERS" | tr '\n' ' ' || true)"
echo "health=$(curl --silent --show-error --max-time 3 http://127.0.0.1:8787/healthz 2>/dev/null || echo unavailable)"
STATUS
  exit $?
fi

if [ "$ACTION" = stop ]; then
  "${SSH[@]}" "$HOST" bash -s -- "$LOCK" "$ROOT" "$RELAYERS" <<'STOP'
set -euo pipefail
LOCK=$1; ROOT=$2; RELAYERS=$3
OWNER=$(cat "$LOCK/owner" 2>/dev/null || true)
case "$OWNER" in
  doom-dcg-public:*) ;;
  "") if pgrep -f "$RELAYERS" >/dev/null; then echo "a relayer runs without the public DCG lock; refusing to stop it" >&2; exit 3; fi
      echo "No public DCG relayer is active."; exit 0 ;;
  *) echo "Tokyo's Doom lock belongs to $OWNER; refusing to stop it" >&2; exit 3 ;;
esac
RUN=$ROOT/run/$(cat "$LOCK/run_name")
PID=$(cat "$RUN/launcher.pid" 2>/dev/null || true)
case "$PID" in ''|*[!0-9]*) PID= ;; esac
# SIGINT lets the relayer stop the pipeline, which writes its receipt.
if [ -n "$PID" ] && grep -q "server.mjs" "/proc/$PID/cmdline" 2>/dev/null; then
  kill -INT "$PID" 2>/dev/null || true
  for _ in $(seq 1 40); do kill -0 "$PID" 2>/dev/null || break; sleep 1; done
  kill -TERM "$PID" 2>/dev/null || true
fi
for _ in $(seq 1 10); do pgrep -f "[r]elay_pipeline.py.*--run-dir $RUN" >/dev/null || break; sleep 1; done
pkill -TERM -f "[r]elay_pipeline.py.*--run-dir $RUN" 2>/dev/null || true
sleep 2
LEFT=$(pgrep -f "$RELAYERS" || true)
[ -z "$LEFT" ] || { echo "relayer PIDs remain: $LEFT" >&2; exit 1; }
if [ "$(cat "$LOCK/owner" 2>/dev/null || true)" = "$OWNER" ]; then
  rm -f "$LOCK/owner" "$LOCK/run_name" "$LOCK/created_utc"; rmdir "$LOCK"
fi
echo "Public DCG relayer stopped; receipt: $RUN/relayer-session.json"
STOP
  exit $?
fi

# start
ORIGIN=${DOOM_PUBLIC_ORIGIN:-}
if ! [[ "$ORIGIN" =~ ^https://[A-Za-z0-9.-]+(:[0-9]+)?$|^http://127\.0\.0\.1:[0-9]+$ ]]; then
  echo "Set DOOM_PUBLIC_ORIGIN to the exact HTTPS page origin (or a loopback origin for testing)." >&2; exit 2
fi
CAP=${DOOM_PUBLIC_SPEND_CAP:-1000}
DURATION=${DOOM_PUBLIC_SECONDS:-604800}
VIEWERS=${DOOM_PUBLIC_MAX_VIEWERS:-200}
NAME=${DOOM_PUBLIC_NAME:-public-$(date -u +%Y%m%dT%H%M%SZ)}
STATE_PATH=${DOOM_PUBLIC_STATE_PATH:-$ROOT/public-mode/public-state.json}
python3 -c 'import sys; c=float(sys.argv[1]); s=int(sys.argv[2]); v=int(sys.argv[3]); sys.exit(0 if 0<c<=1000 and 1<=s<=604800 and 1<=v<=200 else 1)' \
  "$CAP" "$DURATION" "$VIEWERS" || { echo "cap must be in (0, 1000], duration 1..604800 s, viewers 1..200" >&2; exit 2; }
[[ "$NAME" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "DOOM_PUBLIC_NAME has invalid characters" >&2; exit 2; }
case "$STATE_PATH" in "$ROOT"/public-mode/*.json) ;; *) echo "DOOM_PUBLIC_STATE_PATH must be a JSON file under $ROOT/public-mode/" >&2; exit 2 ;; esac
OWNER="doom-dcg-public:$NAME"

"${SSH[@]}" "$HOST" bash -s -- "$LOCK" "$OWNER" "$NAME" "$ROOT" "$KEYS" "$PY" "$RESOURCE_SOURCE" \
  "$CAP" "$DURATION" "$VIEWERS" "$STATE_PATH" "$ORIGIN" "$RELAYERS" <<'LAUNCH'
set -euo pipefail
LOCK=$1; OWNER=$2; NAME=$3; ROOT=$4; KEYS=$5; PY=$6; RESOURCE_SOURCE=$7
CAP=$8; DURATION=$9; VIEWERS=${10}; STATE_PATH=${11}; ORIGIN=${12}; RELAYERS=${13}
[ -s "$ROOT/doom/DEPLOYED_COMMIT" ] || { echo "nothing deployed; run scripts/doom_dcg_public.sh deploy" >&2; exit 2; }
running=$(pgrep -f "$RELAYERS" || true)
if [ -e "$LOCK" ] || [ -n "$running" ]; then
  echo "Tokyo is occupied. Lock owner: $(cat "$LOCK/owner" 2>/dev/null || echo none); PIDs: ${running:-none}" >&2
  exit 3
fi
mkdir -m 700 "$LOCK"
printf '%s\n' "$OWNER" > "$LOCK/owner"; printf '%s\n' "$NAME" > "$LOCK/run_name"
date -u +%Y-%m-%dT%H:%M:%SZ > "$LOCK/created_utc"
release() { if [ "$(cat "$LOCK/owner" 2>/dev/null || true)" = "$OWNER" ]; then rm -f "$LOCK/owner" "$LOCK/run_name" "$LOCK/created_utc"; rmdir "$LOCK"; fi; }
RUN=$ROOT/run/$NAME
mkdir -p "$RUN" "$(dirname "$STATE_PATH")"; chmod 700 "$RUN"
cd "$ROOT/doom"
nohup env DOOM_DCG_RUN_DIR="$ROOT/run" DOOM_RESOURCE_SOURCE="$RESOURCE_SOURCE" \
  PYTHONPATH="$ROOT/doom/stunts/doom-dcg:$ROOT/dcg-python" \
  node stunts/doom-relayer/server.mjs --mode chain --network testnet-dcg --port 8787 --host 127.0.0.1 \
    --keys-dir "$KEYS" --payer-key "$KEYS/payer.json" \
    --python "$PY" --script "$ROOT/doom/stunts/doom-dcg/relay_pipeline.py" --run-dir "$RUN" \
    --seconds "$DURATION" --spend-cap-fogo "$CAP" \
    --public-mode --public-state "$STATE_PATH" --public-max-viewers "$VIEWERS" --cors-origin "$ORIGIN" \
  > "$RUN/relayer.log" 2>&1 < /dev/null &
PID=$!; printf '%s\n' "$PID" > "$RUN/launcher.pid"
for _ in $(seq 1 120); do
  kill -0 "$PID" 2>/dev/null || break
  if curl --silent --fail --max-time 2 http://127.0.0.1:8787/healthz >/dev/null; then
    echo "run=$NAME pid=$PID commit=$(cat "$ROOT/doom/DEPLOYED_COMMIT")"
    exit 0
  fi
  sleep 1
done
echo "the relayer did not become healthy; see $RUN/relayer.log" >&2
kill -TERM "$PID" 2>/dev/null || true
release
exit 1
LAUNCH
echo "Relayer started on Tokyo loopback; nothing was published. Use status and stop."
