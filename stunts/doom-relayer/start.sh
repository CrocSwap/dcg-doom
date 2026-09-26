#!/usr/bin/env bash
# Start the Doom relayer in chain mode, after checking that the host is running
# the code this repository has.
#
# The check is the point of this script existing in the repository. The relayer
# tree is rsynced from a worktree, so a fix that was committed is not a fix on
# the host, and on 2026-09-26 a session ran a relayer that never published a
# field the committed code published -- the tree was the unfixed file, and the
# only evidence was a sha256 someone had to go and compute. `deploy_check` below
# refuses to start on that, and says which file differs.
#
# Override the pin with PIN=... to run a different commit's code; the check then
# reports that the checkout and the pin disagree, which is the honest state of a
# tree that is about to be rsynced.
#
# The player reaches this through an SSH tunnel; nothing is published.
#
#  * strip rate. This is the one number that decides whether frames land at all.
#    Measured 2026-09-26 on the R12 context: at 64/s every strip transaction
#    came back `InstructionError: [1, ProgramFailedToComplete]` -- 740,000 CU is
#    not enough compute for that many strips beside a running sweep, and a
#    4.10 fps run only survived because the serial submit loop throttled it to
#    32.8 strips/s by accident. 40/s failed the same way. So the rate is stated
#    rather than assumed, and the default is 32 strips/s = 4 fps, under the 42-48
#    strips/s measured as sustainable and over the 4.10 fps this loop used to
#    reach by being slow. Raise it and the chain, not the sender, will say so.
#
#  * the strip CU limit. 740,000 is three strips to a block, and it was measured
#    on a cheap view. Render cost is a property of the picture, so a context that
#    has moved into a busier view needs more than the limit allows and every
#    strip comes back `ProgramFailedToComplete` at exactly 740,000 -- measured on
#    the R12 context from cursor 20,572 up, at every rate tried. STRIP_CU_LIMIT
#    raises it; the chain's own per-transaction ceiling is the only upper bound
#    this side knows about.
#
# Usage: start.sh [name] [seconds] [strip-rate]
set -eu
RD=${RELAYER_DIR:-/home/ec2-user/doom-relayer-20260925}
NAME=${1:-play-r13}
SECONDS_LIMIT=${2:-300}
STRIP_RATE=${3:-32}
REPO=${RELAYER_REPO:-$RD}
PIN=${PIN:-$REPO/scripts/doom_relayer_deploy.json}
PY=${PY:-/home/ec2-user/basanos-main/.venv/bin/python}
mkdir -p "$RD/out/$NAME"

# Every on-chain step in this repository goes through stage_cap.sh at 90 s
# (docs/project-rules.md rule 3). The deploy check is a filesystem read, so it
# is not an on-chain step and is not capped; it is also the gate, and a gate that
# can be killed at 90 s is not a gate.
"$PY" "$REPO/scripts/doom_relayer_deploy_check.py" --host-dir "$RD" --pin "$PIN" \
  --json "$RD/out/$NAME/deploy-check.json"

export PYTHONPATH=/home/ec2-user/basanos-main/src
cd "$RD"
exec node "$RD/stunts/doom-relayer/server.mjs" \
  --mode chain --port 8787 --host 127.0.0.1 \
  --keys-dir ${KEYS_DIR:-/home/ec2-user/doom-keys/r12-page} \
  --payer-key /home/ec2-user/doom-keys/tokyo-testnet-payer.json \
  --python "$PY" \
  --script "$RD/scripts/doom_relayer_server.py" \
  --run-dir "$RD/out/$NAME" \
  --rpc-a http://64.130.44.173:8899 --rpc-b https://testnet.fogo.io \
  --image-sha256 0d84966dbfb5f6faeaf338c3e2ebc16eb6703294dead4bd1f7dc1ba7abcefe54 \
  --image-bytes 916080 \
  --seconds "$SECONDS_LIMIT" --strip-rate "$STRIP_RATE" --render-buffers ${BUFFERS:-2} \
  --advance-tics ${ADV_TICS:-4} --advance-pipeline-depth 4 --sweep-rate ${SWEEP_RATE:-8} \
  --input-tics-per-second 35 --spend-cap-fogo ${SPEND_CAP:-2} \
  --frame-deadline-seconds ${FRAME_DEADLINE:-2.0} \
  --status-batch ${STATUS_BATCH:-100} --status-min-interval ${STATUS_MIN_INTERVAL:-0.25} \
  --strip-cu-limit ${STRIP_CU_LIMIT:-0} \
  --rpc-lag-drop-slots ${RPC_LAG_DROP:-512} --rpc-lag-check-seconds ${RPC_LAG_CHECK:-10} \
  --reset-start-account ${RESET_START_ACCOUNT:-9XpmEE9iBu5XuJj3S1H32tauME79HGXUR2hT31n9NjM1}
