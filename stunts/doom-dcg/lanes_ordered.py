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
LIMITS = {"input": 60_000, "advance": 380_000, "capture": 420_000, "render": 840_000, "commit": 40_000}


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
    # priority_step 1: with the epoch offset the default (1000) prices an
    # 840k-CU strip near 1 FOGO, and the node refuses it at preflight.
    session = OrderedLane(RPC, payer, signer(kp["payer"], kp["authority"]), priority_step=1)
    render = [OrderedLane(RPC, Pubkey.from_string(c["pubkeys"][f"lane-payer-{k}"]),
                          signer(kp[f"lane-payer-{k}"], kp["authority"]), priority_step=1) for k in range(args.lanes)]
    cursor0 = int.from_bytes(account(RPC, a.session)[112:116], "little") if args.start_cursor < 0 else args.start_cursor
    if cursor0 % 8:
        raise SystemExit(f"session cursor {cursor0} is not at a frame boundary")
    commands = Path(c["commands_path"]).read_bytes()
    count = len(commands) // 4

    lane_free = [asyncio.Event() for _ in range(args.lanes)]
    for e in lane_free:
        e.set()
    committed: dict[int, asyncio.Event] = {}
    commit_sent: dict[int, asyncio.Event] = {}
    commits = OrderedLane(RPC, payer, signer(kp["payer"], kp["authority"]), priority_step=1)
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
            r = await asyncio.to_thread(wait_sync, session, capture_handle, watch_last=True, poll_seconds=0.03)
            note({"frame": frame, "captured": target, "session_s": round(r.landed_seconds, 2), "repaired": r.repaired})
            strips = [LaneStep(f"f{frame}-render-{at:08d}",
                               (P.lane_run(program, authority, a, lane, target, at, n),), LIMITS["render"])
                      for at, n in P.render_phases()]
            h = await render_lane.send(strips, blockhash=await asyncio.to_thread(render_lane.blockhash), epoch=frame)
            r = await asyncio.to_thread(wait_sync, render_lane, h, watch_last=True, poll_seconds=0.03)
            note({"frame": frame, "rendered": target, "render_s": round(r.landed_seconds, 2), "repaired": r.repaired,
                  "slots": r.slots, "error": str(r.first_error)[:80] if r.first_error else None})
            # Commits go out in frame order on one shared ordered lane: frame
            # N's commit is sent once frame N-1's is sent (not confirmed);
            # the newest-only guard and repair keep them in order.
            if frame > 0:
                await (commit_sent[frame - 1] if args.pipeline_commits else committed[frame - 1]).wait()
            commit = [LaneStep(f"f{frame}-commit-{target:08d}",
                               (P.lane_commit(program, authority, a, lane, target),), LIMITS["commit"])]
            via = commits if args.pipeline_commits else render_lane
            h = await via.send(commit, blockhash=await asyncio.to_thread(via.blockhash), epoch=frame)
            commit_sent[frame].set()
            r = await asyncio.to_thread(wait_sync, via, h, watch_last=True, poll_seconds=0.03)
            note({"frame": frame, "committed": target, "lane": lane.index, "repaired": r.repaired})
        except Exception as e:  # stop the campaign; heal with lanes_testnet.py abort/advance-to
            failure.append(f"frame {frame}: {e}")
            note({"frame": frame, "error": str(e)[:200]})
        finally:
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
        if time.monotonic() - bh_at > 3.0:
            blockhash, bh_at = await asyncio.to_thread(session.blockhash), time.monotonic()
        # The input batch rides in the first advance's transaction: sent
        # separately it can land after the advances that read it (2330,
        # input gap), which costs a repair pass per frame.
        inputs = tuple(D.write_input(program, authority, a, seq, commands[(seq % count) * 4:(seq % count + 1) * 4])
                       for seq in range(cursor + 1, cursor + 9))
        # Advances per transaction (--tics-per-tx; 4 x ~317k CU fit under 1.4M):
        # fewer serial transactions, fewer chances to land out of order.
        k = args.tics_per_tx
        steps = []
        for t in range(0, 8, k):
            ixs = (inputs if t == 0 else ()) + tuple(D.advance(program, authority, a, cursor + u) for u in range(t, t + k))
            limit = min(1_400_000, LIMITS["advance"] * k + (LIMITS["input"] if t == 0 else 0))
            steps.append(LaneStep(f"f{frame}-advance-{cursor + t:08d}x{k}", ixs, limit))
        steps.append(LaneStep(f"f{frame}-capture-{cursor + 8:08d}",
                              tuple(P.capture_instructions(program, authority, a, lane, cursor + 8)), LIMITS["capture"]))
        handle = await session.send(steps, blockhash=blockhash, epoch=frame)
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
    done = sum(1 for r in log if "committed" in r)
    summary = {"frames": done, "failures": failure, "seconds": round(total, 2),
               "frames_per_second": round(done / total, 3) if total else 0, "start_cursor": cursor0}
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
    ap.add_argument("--out", type=Path)
    return asyncio.run(run(ap.parse_args()))


if __name__ == "__main__":
    sys.exit(main())
