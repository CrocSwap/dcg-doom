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
#  * the image. 2026-09-26 06:2x UTC the program was upgraded to the four-buffer
#    image (out/runs/doom-4-buffers-20260926/, sha 4bb632e2..., 916,496 B) and
#    the relayer followed it. That image made the snapshot name in
#    RENDER-BUFFERED mandatory: a zero name is refused as malformed, so a
#    relayer whose `buffered_render_request` does not carry the account's own
#    hash spends nine transactions a frame to be told 301. The hash below is the
#    one the live program was verified to carry, and `preflight` re-reads it from
#    the chain, so a host running an older relayer against the newer program
#    refuses to start rather than burning transactions.
#
#  * the render buffer count. BUFFERS is 1..4, the program's own `BUFFER_COUNT`.
#    Buffers 0 and 1 are the accounts a two-buffer session already used, so
#    raising the count is a widening and not a move.
#
#  * the send pacer. SEND_RATE is transactions per second per endpoint, and
#    RPC_C names a third healthy node if one is wanted; the two together are the
#    frame's own send cost, 8 / (SEND_RATE x endpoints) seconds for the strips.
#    At the default 30 over two endpoints that is 0.133 s, and 0.135 s is what
#    the eight sends measured on R15 at one buffer. This is a pacing choice on
#    our side, not a protocol bound, and a node that will not answer at the rate
#    answers 429 -- so `rpc_lag.throttled` in the receipt, not the rate, is what
#    says whether it was sustainable. `64.130.44.172` stays out of every list:
#    R9b measured it 14,336 slots behind `.173`.
#
#  * the one-buffer read path. At BUFFERS=1 the buffer is held from the moment a
#    submission starts until the readback has copied 1,113,792 bytes out of it,
#    and that is the whole reason one buffer rendered at 1.8 fps while four did
#    not: nothing else can be submitted meanwhile. READBACK=overlap hands the
#    buffer back as soon as the frame's strips have *executed* -- the point after
#    which the next frame's snapshot is safe to send, because a strip binds to
#    whatever snapshot is in the account when it executes and the program refuses
#    one naming a different snapshot (308) -- and reads the pixels in the shadow
#    of the next submission. The next frame's strip send waits for the previous
#    readback, so a slow readback delays a frame and cannot tear one.
#    READBACK=overlap also implies PRE_READ=1, which is what makes it safe: the
#    readback then reads the eight strip accounts only, and the snapshot account
#    -- the one account a snapshot transaction writes, and nothing else -- is out
#    of its bank view. The payload's hash is still recomputed and still has to
#    match the name the strips are sent with. Default is the old serial path.
#
# Usage: start.sh [name] [seconds] [strip-rate]
set -eu
RD=${RELAYER_DIR:-/home/ec2-user/doom-relayer-20260925}
NAME=${1:-play-r15}
SECONDS_LIMIT=${2:-300}
STRIP_RATE=${3:-32}
REPO=${RELAYER_REPO:-$RD}
PIN=${PIN:-$REPO/scripts/doom_relayer_deploy.json}
PY=${PY:-/home/ec2-user/basanos-main/.venv/bin/python}
# `overlap` is the one value that is not a boolean, because the overlap is only
# defined against the strips-only readback and asking for one without the other
# is asking for a flag that cannot work.
case "${READBACK:-serial}" in
  serial) readback_flags=() ;;
  overlap) readback_flags=(--pre-read-snapshot --overlap-readback) ;;
  pre-read) readback_flags=(--pre-read-snapshot) ;;
  *) echo "READBACK must be serial, pre-read or overlap" >&2; exit 2 ;;
esac
if [ -n "${PRE_READ:-}" ] && [ "${PRE_READ}" != "0" ] \
   && [ "${#readback_flags[@]}" -eq 0 ]; then
  readback_flags=(--pre-read-snapshot)
fi

mkdir -p "$RD/out/$NAME"

# Every on-chain step in this repository goes through stage_cap.sh at 90 s
# (docs/project-rules.md rule 3). The deploy check is a filesystem read, so it is
# not an on-chain step and is not capped; it is also the gate, and a gate that
# can be killed at 90 s is not a gate.
"$PY" "$REPO/scripts/doom_relayer_deploy_check.py" --host-dir "$RD" --pin "$PIN" \
  --json "$RD/out/$NAME/deploy-check.json"

export PYTHONPATH=/home/ec2-user/basanos-main/src
cd "$RD"
exec node "$RD/stunts/doom-relayer/server.mjs" \
  --mode chain --port 8787 --host 127.0.0.1 \
  --keys-dir ${KEYS_DIR:?KEYS_DIR must name the context keys directory} \
  --payer-key /home/ec2-user/doom-keys/tokyo-testnet-payer.json \
  --python "$PY" \
  --script "$RD/scripts/doom_relayer_server.py" \
  --run-dir "$RD/out/$NAME" \
  --rpc-a http://64.130.44.173:8899 --rpc-b https://testnet.fogo.io \
  ${RPC_C:+--rpc-c "$RPC_C"} --send-pacer-rate ${SEND_RATE:-30} \
  --image-sha256 4bb632e21d98f1dfbce8f54658c2b63d08c8a72b06a32107ea00d8a293658ba3 \
  --image-bytes 916496 \
  --seconds "$SECONDS_LIMIT" --strip-rate "$STRIP_RATE" --render-buffers ${BUFFERS:-2} \
  --advance-tics ${ADV_TICS:-4} --advance-pipeline-depth 4 --sweep-rate ${SWEEP_RATE:-8} \
  --input-tics-per-second 35 --spend-cap-fogo ${SPEND_CAP:-2} \
  --frame-deadline-seconds ${FRAME_DEADLINE:-2.0} \
  --status-batch ${STATUS_BATCH:-100} --status-min-interval ${STATUS_MIN_INTERVAL:-0.25} \
  --strip-cu-limit ${STRIP_CU_LIMIT:-0} \
  --rpc-lag-drop-slots ${RPC_LAG_DROP:-512} --rpc-lag-check-seconds ${RPC_LAG_CHECK:-10} \
  ${readback_flags[@]+"${readback_flags[@]}"} \
  ${RESET_START_ACCOUNT:+--reset-start-account "$RESET_START_ACCOUNT"}
