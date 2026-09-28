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
#  * strip rate, and the strip CU limit, together. They are one decision.
#    The 32 strips/s default existed because 40 and 64 strips/s came back
#    `InstructionError: [1, ProgramFailedToComplete]` on R12. **That was the CU
#    limit, not the rate.** A strip on this context measures 960,055-1,003,200 CU
#    and the old limit was 740,000, so a strip could not complete at *any* rate
#    once the view got expensive; the rate was blamed for a per-transaction
#    bound. Re-measured 2026-09-26 on R15 at STRIP_CU_LIMIT=1400000, two buffers,
#    60 s each: 32/s 2.85 fps, 40/s 2.86, 48/s 2.82, 64/s 2.84 -- zero
#    ProgramFailedToComplete, and `strip_rate_cap.binding` false at 48 and 64.
#    So the default is 64 strips/s, a 0.125 s floor on the frame interval that
#    nothing reaches, and STRIP_CU_LIMIT defaults to 1,400,000 with it. Setting
#    one without the other is the trap: 64/s at 740,000 CU loses every strip.
#
#  * the image. 2026-09-27 the program is qualified for the in-place LIF/2
#    reinitialise image (out/runs/doom-reinit-fix2-2026-09-27/, 1,067,616 B).
#    This image adds batched E1M1 setup used by R15 REINIT. The operator copies
#    the receipt-verified `.so` to IMAGE; this script derives its local digest
#    and size, and `preflight` reads the live chain image before it sends.
#    The earlier four-buffer image made the snapshot name in
#    RENDER-BUFFERED mandatory: a zero name is refused as malformed, so a
#    relayer whose `buffered_render_request` does not carry the account's own
#    hash spends nine transactions a frame to be told 301. `preflight` compares
#    the derived local digest and size to the image read from chain, so a host
#    running an older relayer against the newer program refuses to start rather
#    than burning transactions.
#
#  * the render buffer count. BUFFERS is 1..4, the program's own `BUFFER_COUNT`.
#    Buffers 0 and 1 are the accounts a two-buffer session already used, so
#    raising the count is a widening and not a move. All four are provisioned on
#    R15 as of 2026-09-26 (23.280030720 FOGO of rent for buffers 1..3 and their
#    24 strips), and the default is 4 because that is where the frame rate stops
#    improving: measured 60 s each on R15, one buffer 2.01 fps and 2 buffers
#    2.85, 3 buffers 3.14, 4 buffers 3.28. Two is the worst of the three above
#    one buffer, because at two the buffer is still held for the strips'
#    `await_strips` and the two round trips are serial, while at four the
#    release wait overlaps the next three submissions. At four buffers on the
#    *old* 30/s pacer it was 2.13 fps with 12 skips and a gateway 429, so the
#    buffer count and SEND_RATE below are one decision too.
#
#  * the send pacer. SEND_RATE is transactions per second per endpoint. A frame
#    sends nine transactions, so this is the frame's own send cost, and it is a
#    pacing choice on our side rather than a protocol bound. Measured 2026-09-26
#    on R15 at four buffers, 60 s each: 30/s 2.13 fps with 12 skips and a
#    `testnet.fogo.io` 429, 45/s 3.28 with none, 60/s 3.30 with none. 45 is the
#    default because 60 buys 0.02 fps and costs the input path: the landed ratio
#    through the page fell from 0.977 at 45/s to 0.75 at 60/s on two buffers,
#    because the deposits compete with the renders for the same two nodes.
#
#    RPC_C names a third node, and the measurement says not to. The eight strip
#    sends of a frame are one `asyncio.gather`, so the step is set by the
#    *slowest* node it touches, and the only other public nodes answer from
#    Tokyo at 281-491 ms against `.173`'s 4.6 ms. Adding the best of them
#    (`64.130.37.72`, genesis verified, never behind) took the frame rate from
#    3.28 to 2.23 fps, `strip_send` from 0.210 s to 0.423 s, and the
#    key-to-picture median from 548 ms to 1314 ms. The flag is here because the
#    pool is generic over its URLs and a node nearer than these would make it
#    worth using.
#
#    `64.130.44.172` stays out of every list: R9b measured it 14,336 slots behind
#    `.173`, and a node that far behind is a stalled readback, not a second
#    opinion. A node that will not answer at the rate answers 429, so the
#    receipt's `errors` and `config.send_pacer_rate_per_endpoint`, not the rate
#    itself, are what say whether a rate was sustainable.
#
#  * RPC_B. The second node, and `RPC_B=none` runs the session on one.
#    Measured 2026-09-26 on R15 at four buffers, 60 s, the two-node session froze
#    the *whole process* -- the engine's sweeps and the render together -- three
#    times for 10.2, 10.2 and 10.7 s, which is 31 s of a 60 s session, and the
#    render's own `sendTransaction` on the public node went from 0.13 s to
#    1.2 s inside those windows while the fast node answered in 4.6 ms. The
#    relayer's own error list has two `getSlot ... HTTP 429` from
#    `testnet.fogo.io` in the same 60 s, six lag checks. So the two-node pool
#    spends 52% of a session waiting on one rate-limited public endpoint, and
#    the switch is here to measure the alternative rather than argue about it.
#    One node makes the sender's dual-RPC agreement vacuous (see
#    `apply_rpc_health`) and leaves no fallback, so the default is still two.
#
#  * MIN_CURSOR_STEP, the smallest cursor step between two rendered frames, in
#    tics, default 1. A frame is only worth block space if it is a game state
#    nobody has rendered, and the renderer now gates on the highest cursor it has
#    already handed to a submission or got back from one -- not, as before, on the
#    last frame that *finished*, which let all four buffers take the same cursor
#    before any of them came back. Measured 2026-09-26 on R15 at four buffers,
#    60 s: 204 frames of 85 distinct cursors, so 3.37 fps was 1.4 new pictures/s
#    and about 58% of the render compute was spent re-rendering states the chain
#    had already published.
#
#    The default is 8, not 1, and the measurement says why. Against
#    ADV_TICS=4 sweeps, 1 renders every sweep and 8 renders every other one --
#    which sounds like a straight trade of pictures for engine tics, and is not:
#    measured 60 s each on R15, 1 gave 1.60 new cursors/s and 1.52 new pictures/s
#    at 17.5 engine tics/s with key-to-picture at 993 ms, and 8 gave 2.30 and
#    2.16 at 22.2 tics/s with key-to-picture at 408 ms. The renderer was not the
#    compute-bound term; the chain's release queue was (`await_strips` at a
#    0.50-0.60 s median, 7 blocks at 13 blocks/s). The compute it stops asking
#    for goes to the sweeps, the sweeps publish cursors faster, and there are
#    then more cursors to render than the renderer can use. 16 is not measured.
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
#    match the name the strips are sent with. Default is the measured 09-26
#    overlap path.
#
# Usage: start.sh [name] [seconds] [strip-rate]
set -eu
RD=${RELAYER_DIR:-/home/ec2-user/doom-relayer-20260925}
NAME=${1:-play-r15}
SECONDS_LIMIT=${2:-300}
STRIP_RATE=${3:-64}
REPO=${RELAYER_REPO:-$RD}
PIN=${PIN:-$REPO/scripts/doom_relayer_deploy.json}
PY=${PY:-/home/ec2-user/basanos-main/.venv/bin/python}
# The release image is copied to the host beside the relayer. Derive its
# identity from those exact bytes unless the operator supplies the digest and
# size from the retained build receipt. This keeps the source pin independent
# of the image digest while the verified buffer writer still enforces the
# reviewed digest during an upgrade.
IMAGE=${IMAGE:-$RD/image/doom_sbf.so}
if [ ! -f "$IMAGE" ]; then
  echo "missing relayer image: $IMAGE" >&2
  exit 2
fi
IMAGE_SHA=${IMAGE_SHA:-$(sha256sum "$IMAGE" | awk '{print $1}')}
IMAGE_BYTES=${IMAGE_BYTES:-$(wc -c < "$IMAGE" | tr -d ' ')}
# `overlap` is the one value that is not a boolean, because the overlap is only
# defined against the strips-only readback and asking for one without the other
# is asking for a flag that cannot work.
case "${READBACK:-overlap}" in
  serial) readback_flags=() ;;
  overlap) readback_flags=(--pre-read-snapshot --overlap-readback) ;;
  pre-read) readback_flags=(--pre-read-snapshot) ;;
  *) echo "READBACK must be serial, pre-read or overlap" >&2; exit 2 ;;
esac
case "${BATCH_RENDER_SENDS:-0}" in
  1|true|TRUE|yes|YES|on|ON) batch_render_flags=(--batch-render-sends) ;;
  *) batch_render_flags=() ;;
esac
# Pass the relayer-only ADVANCE compute budget through to the Python sender.
# The 520,000 default was measured on E1M1; later scripted play exhausted it.
export ADVANCE_CU_LIMIT="${ADVANCE_CU_LIMIT:-520000}"
if [ -n "${PRE_READ:-}" ] && [ "${PRE_READ}" != "0" ] \
   && [ "${#readback_flags[@]}" -eq 0 ]; then
  readback_flags=(--pre-read-snapshot)
fi

# The two RPC endpoints, and the switch that runs a session on one of them.
# `RPC_B` is *always* passed, empty or not, because `--rpc-b` has the public
# endpoint as its own default: a session that wants one node has to say so by
# name, or it gets two and the receipt says two.
rpc_a=${RPC_A:-http://64.130.44.173:8899}
rpc_b=${RPC_B-}
case "$rpc_b" in none|off|NONE|OFF) rpc_b="" ;; esac

mkdir -p "$RD/out/$NAME"

# Every on-chain step in this repository goes through stage_cap.sh at 90 s
# (docs/project-rules.md rule 3). The deploy check is a filesystem read, so it is
# not an on-chain step and is not capped; it is also the gate, and a gate that
# can be killed at 90 s is not a gate.
"$PY" "$REPO/scripts/doom_relayer_deploy_check.py" --host-dir "$RD" --pin "$PIN" \
  --json "$RD/out/$NAME/deploy-check.json"

export PYTHONPATH=/home/ec2-user/basanos-main/src
cd "$RD"
# RESET/9 answers the program's dead-player use refusal (295). The start
# account is sealed from the E1M1 image; keep recovery enabled by default.
exec node "$RD/stunts/doom-relayer/server.mjs" \
  --mode chain --port 8787 --host 127.0.0.1 \
  --keys-dir ${KEYS_DIR:?KEYS_DIR must name the context keys directory} \
  --payer-key /home/ec2-user/doom-keys/tokyo-testnet-payer.json \
  --python "$PY" \
  --script "$RD/scripts/doom_relayer_server.py" \
  --run-dir "$RD/out/$NAME" \
  --rpc-a "$rpc_a" --rpc-b "$rpc_b" \
  ${RPC_C:+--rpc-c "$RPC_C"} --send-pacer-rate ${SEND_RATE:-45} \
  --image-sha256 "$IMAGE_SHA" \
  --image-bytes "$IMAGE_BYTES" \
  --seconds "$SECONDS_LIMIT" --strip-rate "$STRIP_RATE" --render-buffers ${BUFFERS:-4} \
  --advance-tics ${ADV_TICS:-4} --advance-pipeline-depth ${ADV_PIPELINE_DEPTH:-4} --sweep-rate ${SWEEP_RATE:-8} \
  --input-tics-per-second 35 --spend-cap-fogo ${SPEND_CAP:-2} \
  --frame-deadline-seconds ${FRAME_DEADLINE:-2.0} \
  --min-cursor-step-tics ${MIN_CURSOR_STEP:-8} \
  --render-pool-workers ${RENDER_POOL_WORKERS:-0} \
  --status-batch ${STATUS_BATCH:-100} --status-min-interval ${STATUS_MIN_INTERVAL:-0.25} \
  --strip-cu-limit ${STRIP_CU_LIMIT:-1400000} \
  --atomic-live-inputs \
  --rpc-lag-drop-slots ${RPC_LAG_DROP:-512} --rpc-lag-check-seconds ${RPC_LAG_CHECK:-10} \
  ${readback_flags[@]+"${readback_flags[@]}"} \
  ${batch_render_flags[@]+"${batch_render_flags[@]}"} \
  ${STRIPS_BEHIND_SNAPSHOT:+--strips-behind-snapshot} \
  --reset-start-account "${RESET_START_ACCOUNT:-9XpmEE9iBu5XuJj3S1H32tauME79HGXUR2hT31n9NjM1}"
