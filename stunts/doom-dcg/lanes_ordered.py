#!/usr/bin/env python3
"""Doom frames over DCG render lanes with pipelined sends (lanes slice 4,
"Next" 1 in docs/experiments/doom-lanes-testnet-2026-10-04.md).

The confirm-per-transaction sequencer spends ~0.93 s per dependent step, so a
frame's 10-transaction session chain costs ~9.3 s. Here each frame's session
chain (input batch, eight advances, capture) goes out on one ordered,
pipelined lane (dcg.sequencer.OrderedLane), and each render lane's strips go
out on that lane's own ordered lane once its capture has landed:

- frame N's session chain waits only for lane N mod L's previous commit (the
  capture needs an idle lane);
- frame N's strips wait for frame N's capture;
- frame N's commit waits for its strips and for frame N-1's commit (DCG
  refuses an older frame, 2341).

Every step is guarded on chain (cursor, lane state, newest-only commit), the
precondition OrderedLane needs for exactly-once. Explicit testnet RPC only.

    lanes_ordered.py --run-dir RUN --frames N [--lanes 3] [--start-cursor C]
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import json
import sys
import threading
import time
from pathlib import Path

from solders.pubkey import Pubkey

import lanes_plan as P
import testnet_driver as D
from dcg.sequencer.ordered_lane import LaneStep, OrderedLane

RPC = "http://64.130.44.172:8899"
# Measured on this program (campaign-L3-nohook, getTransaction): input 48.7k,
# advance 317k, capture 348k, strips up to 776k, commit 21.8k; requests carry
# a margin (OrderedLane raises them to the running maximum along a lane).
# Strips vary with the frame: two exceeded 840k on 10-04 (frames 656, 664),
# so strips request 1.2M: two still fit the 2.5M per-account block cap.
LIMITS = {"input": 60_000, "advance": 380_000, "capture": 420_000, "render": 1_200_000, "commit": 40_000}


def signer(*paths):
    s = D.MultiKeypairSigner(tuple(paths))

    async def sign(message: bytes):
        signed = await s.sign(message, None)
        return signed.signature, signed.raw_bytes
    return sign


def wait_sync(lane: OrderedLane, handle, **kw):
    """OrderedLane.wait polls with blocking sleeps: run it on its own thread
    so lanes overlap."""
    return asyncio.run(lane.wait(handle, **kw))


def bad_steps(r) -> list:
    """(index, error, confirmation level) for every step whose status is
    missing or failed: shows which step costs a repair pass."""
    return [(i, (v or {}).get("err"), (v or {}).get("confirmationStatus")) for i, v in enumerate(r.statuses or [])
            if v is None or v.get("err")]


# A stream grows only when fully consumed (cursor == capacity), by at most
# MAX_STREAM_GROWTH_SLOTS per instruction (DCG stateful_v3.rs grow_stream).
STREAM_GROWTH = 512


def grow_stream(program: Pubkey, payer: Pubkey, a: D.Addresses, capacity: int):
    return D.ix(program, 231, bytes([D.WIRE_VERSION, 1]) + D.u32(capacity), (
        (payer, True, True), (a.session, False, True), (a.stream, False, True), (D.SYSTEM_PROGRAM, False, False)))


def session_counters(raw: bytes) -> tuple[int, int, int]:
    """(capacity, cursor, frontier) of a v3 session record."""
    u = lambda at: int.from_bytes(raw[at:at + 4], "little")
    return u(10), u(112), u(116)


def account(url: str, key: Pubkey) -> bytes:
    import urllib.request
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
                       "params": [str(key), {"encoding": "base64", "commitment": "confirmed"}]}).encode()
    out = json.loads(urllib.request.urlopen(urllib.request.Request(url, body, {"Content-Type": "application/json"}),
                                            timeout=10).read())
    return base64.b64decode(out["result"]["value"]["data"][0])


async def run(args) -> int:
    c = D.load_config(args.run_dir)
    program = Pubkey.from_string(c["program_id"])
    payer = Pubkey.from_string(c["pubkeys"]["payer"])
    authority = Pubkey.from_string(c["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, c["session_id"])
    lanes = P.lanes_for(program, a, args.lanes)
    kp = c["key_paths"]
    # Priority: offset = step * n * (ceiling - epoch). With the default
    # ceiling (100,000) the gap between a lane's steps is ~1 part in 300,000,
    # smaller than the scheduler's cost difference between transactions (the
    # first session transaction carries the inputs), so the later step won
    # and every frame paid a repair (10-04). A ceiling of frames + 1 makes the
    # gap ~1%. The default step (1000) with the old ceiling priced an 840k-CU
    # strip near 1 FOGO; with this ceiling --priority-step 100 costs ~0.0001.
    ceiling = args.frames + 1
    step_price = args.priority_step
    session = OrderedLane(RPC, payer, signer(kp["payer"], kp["authority"]), priority_step=step_price)
    render = [OrderedLane(RPC, Pubkey.from_string(c["pubkeys"][f"lane-payer-{k}"]),
                          signer(kp[f"lane-payer-{k}"], kp["authority"]), priority_step=step_price) for k in range(args.lanes)]
    capacity, live_cursor, frontier = session_counters(account(RPC, a.session))
    cursor0 = live_cursor if args.start_cursor < 0 else args.start_cursor
    if (capacity - cursor0) % 8:
        raise SystemExit(f"stream capacity {capacity} is not a whole number of frames past {cursor0}")
    if cursor0 % 8:
        raise SystemExit(f"session cursor {cursor0} is not at a frame boundary")
    commands = Path(c["commands_path"]).read_bytes()
    count = len(commands) // 4
    if args.strip_use:
        # The looped demo input presses USE (button bit 0x02, byte 3) at a
        # switch the kernel refuses past (10-05: tic 5,310, refusal 2334);
        # sustained runs clear it so the player stays in the level.
        commands = bytes(b & ~0x02 if i % 4 == 3 else b for i, b in enumerate(commands))

    lane_free = [asyncio.Event() for _ in range(args.lanes)]
    for e in lane_free:
        e.set()
    committed: dict[int, asyncio.Event] = {}
    commit_sent: dict[int, asyncio.Event] = {}
    commits = OrderedLane(RPC, payer, signer(kp["payer"], kp["authority"]), priority_step=step_price)
    # Frame N's session chain follows frame N-1's: wait (and repair) it only
    # once N-1's capture resolved, or its repair finds steps refused that are
    # merely early (10-04: a grow refused because N-1 had not yet landed).
    session_done: dict[int, asyncio.Event] = {}
    log: list[dict] = []
    t0 = time.monotonic()
    failure: list[str] = []

    def note(row):
        row["t"] = round(time.monotonic() - t0, 2)
        log.append(row)
        print(json.dumps(row), flush=True)

    async def lane_frame(frame: int, cursor: int, lane: P.Lane, capture_handle, render_lane: OrderedLane):
        target = cursor + 8
        try:
            # With repair on, a returned result means every step resolved
            # (repair raises otherwise); repaired counts out-of-order landings.
            if frame > 0:
                await session_done[frame - 1].wait()
            r = await asyncio.to_thread(wait_sync, session, capture_handle, watch_last=True, poll_seconds=0.03,
                                        repair_delay=args.repair_delay)
            session_done[frame].set()
            note({"frame": frame, "captured": target, "session_s": round(r.landed_seconds, 2), "repaired": r.repaired,
                  "bad": bad_steps(r)})
            strips = [LaneStep(f"f{frame}-render-{at:08d}",
                               (P.lane_run(program, authority, a, lane, target, at, n),), LIMITS["render"])
                      for at, n in P.render_phases()]
            h = await render_lane.send(strips, blockhash=await asyncio.to_thread(render_lane.blockhash), epoch=frame, epoch_ceiling=ceiling)
            r = await asyncio.to_thread(wait_sync, render_lane, h, watch_last=True, poll_seconds=0.03,
                                        repair_delay=args.repair_delay)
            note({"frame": frame, "rendered": target, "render_s": round(r.landed_seconds, 2), "repaired": r.repaired,
                  "slots": r.slots, "bad": bad_steps(r)})
            # Commits go out in frame order on one shared ordered lane: frame
            # N's commit is sent once frame N-1's is sent (not confirmed);
            # the newest-only guard and repair keep them in order.
            if frame > 0:
                await (commit_sent[frame - 1] if args.pipeline_commits else committed[frame - 1]).wait()
            commit = [LaneStep(f"f{frame}-commit-{target:08d}",
                               (P.lane_commit(program, authority, a, lane, target),), LIMITS["commit"])]
            via = commits if args.pipeline_commits else render_lane
            h = await via.send(commit, blockhash=await asyncio.to_thread(via.blockhash), epoch=frame, epoch_ceiling=ceiling)
            commit_sent[frame].set()
            r = await asyncio.to_thread(wait_sync, via, h, watch_last=True, poll_seconds=0.03,
                                        repair_delay=args.repair_delay)
            note({"frame": frame, "committed": target, "lane": lane.index, "repaired": r.repaired, "bad": bad_steps(r)})
        except Exception as e:  # stop the campaign; heal with lanes_testnet.py abort/advance-to
            failure.append(f"frame {frame}: {e}")
            note({"frame": frame, "error": str(e)[:200]})
        finally:
            session_done[frame].set()
            commit_sent[frame].set()
            committed[frame].set()
            lane_free[lane.index].set()

    tasks = []
    blockhash, bh_at = None, 0.0
    for frame in range(args.frames):
        if failure:
            break
        cursor = cursor0 + 8 * frame
        lane = lanes[frame % args.lanes]
        await lane_free[lane.index].wait()
        lane_free[lane.index].clear()
        committed[frame] = asyncio.Event()
        commit_sent[frame] = asyncio.Event()
        session_done[frame] = asyncio.Event()
        if time.monotonic() - bh_at > 3.0:
            blockhash, bh_at = await asyncio.to_thread(session.blockhash), time.monotonic()
        # The input batch rides in the first advance's transaction: sent
        # separately it can land after the advances that read it (2330,
        # input gap), which costs a repair pass per frame. Advancing from
        # cursor c reads input c, so a frame needs inputs cursor..cursor+7
        # (those not already written). Until 10-04 this wrote cursor+1..+8:
        # harmless inside the stream, but the last frame before capacity then
        # asked for an input past it and every later frame was refused (2328).
        grow = ()
        if cursor == capacity:
            capacity += STREAM_GROWTH
            grow = (grow_stream(program, payer, a, capacity),)
        inputs = grow + tuple(D.write_input(program, authority, a, seq, commands[(seq % count) * 4:(seq % count + 1) * 4])
                              for seq in range(max(frontier, cursor), cursor + 8))
        frontier = cursor + 8
        # Advances per transaction (--tics-per-tx; 4 x ~317k CU fit under 1.4M):
        # fewer serial transactions, fewer chances to land out of order. (The
        # capture cannot share a transaction with an advance: DCG refuses it,
        # 2334, measured 10-04.)
        k = args.tics_per_tx
        steps = []
        for t in range(0, 8, k):
            ixs = (inputs if t == 0 else ()) + tuple(D.advance(program, authority, a, cursor + u) for u in range(t, t + k))
            limit = min(1_400_000, LIMITS["advance"] * k + (LIMITS["input"] if t == 0 else 0))
            steps.append(LaneStep(f"f{frame}-advance-{cursor + t:08d}x{k}", ixs, limit))
        steps.append(LaneStep(f"f{frame}-capture-{cursor + 8:08d}",
                              tuple(P.capture_instructions(program, authority, a, lane, cursor + 8)), LIMITS["capture"]))
        handle = await session.send(steps, blockhash=blockhash, epoch=frame, epoch_ceiling=ceiling)
        note({"frame": frame, "sent": cursor + 8, "lane": lane.index})
        tasks.append(asyncio.create_task(lane_frame(frame, cursor, lane, handle, render[lane.index])))
        # The next frame's session chain must follow this capture: wait for it
        # to land before sending (same-session ordering), but not for renders.
        # --overlap sends the next frame as soon as its lane is free; the
        # epoch priority keeps older frames ahead in the leader's buffer.
        while not args.overlap and not any(r.get("captured") == cursor + 8 for r in log) and not failure:
            await asyncio.sleep(0.02)
    await asyncio.gather(*tasks)
    total = time.monotonic() - t0
    # Commits are verified (strict repair raises on a step that never
    # landed); the session cursor cross-checks them: it advances 8 tics per
    # captured frame, so it can run ahead of commits only when a frame failed.
    end_cursor = session_counters(account(RPC, a.session))[1]
    captured = (end_cursor - cursor0) // 8
    logged = sum(1 for r in log if "committed" in r)
    done = min(captured, logged)
    if captured != logged and not failure:
        failure.append(f"chain shows {captured} frames (cursor {cursor0} -> {end_cursor}), driver logged {logged}")
    summary = {"frames": done, "frames_captured": captured, "failures": failure, "seconds": round(total, 2),
               "frames_per_second": round(done / total, 3) if total else 0, "start_cursor": cursor0,
               "end_cursor": end_cursor}
    print(json.dumps(summary), flush=True)
    if args.out:
        args.out.write_text(json.dumps({"summary": summary, "log": log}, indent=1) + "\n")
    return 0 if not failure else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--run-dir", type=Path, required=True)
    ap.add_argument("--frames", type=int, default=3)
    ap.add_argument("--lanes", type=int, default=3)
    ap.add_argument("--start-cursor", type=int, default=-1, help="-1: the session's cursor")
    ap.add_argument("--tics-per-tx", type=int, default=4, choices=(1, 2, 4, 8))
    ap.add_argument("--overlap", action="store_true", help="do not wait for a frame's capture before sending the next")
    ap.add_argument("--pipeline-commits", action="store_true", help="send each commit once the previous is sent")
    ap.add_argument("--priority-step", type=int, default=100)
    ap.add_argument("--strip-use", action="store_true", help="clear the USE button in every command")
    ap.add_argument("--repair-delay", type=float, default=1.0,
                    help="seconds to let in-flight steps land before repair (repair checks original signatures)")
    ap.add_argument("--out", type=Path)
    return asyncio.run(run(ap.parse_args()))


if __name__ == "__main__":
    sys.exit(main())
