#!/usr/bin/env python3
"""Finish lanes left mid-render by lanes_ordered.py (e.g. a strip that ran out
of compute): render each lane's remaining strips at the full 1.4M limit and
commit its frame, oldest captured frame first; a frame older than the newest
published one (DCG refuses it, 2341) is aborted instead, returning the lane to
idle. Testnet RPC only; strict repair (a refused step raises).

    lanes_heal.py --run-dir RUN [--lanes 3]
"""

from __future__ import annotations

import argparse
import asyncio
from pathlib import Path

from solders.pubkey import Pubkey

import lanes_plan as P
import testnet_driver as D
from dcg.sequencer.ordered_lane import LaneStep, OrderedLane
from lanes_ordered import RPC, account, signer

RENDERING = 2
STALE_PUBLICATION = 2341


async def stale(lane: OrderedLane, commit: LaneStep) -> bool:
    """Whether DCG refuses this commit as older than the newest publication.
    A commit simulates as refused for other reasons while strips remain, so
    only the stale-publication code counts."""
    import base64
    _sig, raw = await lane._build(commit, lane.blockhash("processed"), 0)
    err = lane.rpc("simulateTransaction", [base64.b64encode(raw).decode(),
                                           {"encoding": "base64", "commitment": "processed"}])["value"].get("err")
    return bool(err) and f"'Custom': {STALE_PUBLICATION}" in str(err)


async def heal(args) -> None:
    c = D.load_config(args.run_dir)
    program = Pubkey.from_string(c["program_id"])
    payer = Pubkey.from_string(c["pubkeys"]["payer"])
    authority = Pubkey.from_string(c["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, c["session_id"])
    kp = c["key_paths"]
    u = lambda raw, at: int.from_bytes(raw[at:at + 4], "little")
    stuck = []
    for lane in P.lanes_for(program, a, args.lanes):
        raw = account(RPC, lane.record)
        if raw[72] == RENDERING:
            stuck.append((u(raw, 76), lane, u(raw, 88)))
    for target, lane, render_cursor in sorted(stuck, key=lambda s: s[0]):
        commit = OrderedLane(RPC, payer, signer(kp["payer"], kp["authority"]), priority_step=1)
        sim_commit = LaneStep(f"heal-{target}-commit", (P.lane_commit(program, authority, a, lane, target),), 40_000)
        abort = P._op(program, P.OP_ABORT, lane.index, D.u32(target),
                      [P._m(authority, True), P._m(a.session, writable=True), P._m(lane.record, writable=True)])
        render = OrderedLane(RPC, Pubkey.from_string(c["pubkeys"][f"lane-payer-{lane.index}"]),
                             signer(kp[f"lane-payer-{lane.index}"], kp["authority"]), priority_step=1)
        strips = [LaneStep(f"heal-{target}-render-{at:08d}", (P.lane_run(program, authority, a, lane, target, at, n),),
                           1_400_000) for at, n in P.render_phases() if at >= render_cursor]
        r = await render.run(strips, watch_last=True, poll_seconds=0.03, repair_delay=0.3) if strips else None
        # Staleness shows only once the strips are done (until then the commit
        # is refused for the missing strips).
        if await stale(commit, sim_commit):
            ra = await commit.run([LaneStep(f"heal-{target}-abort", (abort,), 40_000)], watch_last=True,
                                  poll_seconds=0.03, repair_delay=0.3)
            print({"lane": lane.index, "frame": target, "strips": len(strips), "aborted": ra.landed + ra.repaired},
                  flush=True)
            continue
        rc = await commit.run([sim_commit], watch_last=True, poll_seconds=0.03, repair_delay=0.3)
        print({"lane": lane.index, "frame": target, "strips": len(strips), "rendered_landed": r.landed if r else 0,
               "repaired": r.repaired if r else 0, "committed": rc.landed + rc.repaired}, flush=True)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--run-dir", type=Path, required=True)
    ap.add_argument("--lanes", type=int, default=3)
    asyncio.run(heal(ap.parse_args()))


if __name__ == "__main__":
    main()
