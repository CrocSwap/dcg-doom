#!/usr/bin/env python3
"""Doom frames through DCG's OrderedLane (dcg.sequencer.ordered_lane).

usage: lane_campaign.py --run-dir RUN --start-cursor C --frames N [--url URL]
"""
from __future__ import annotations
import argparse, asyncio, json, sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import testnet_driver as D
from dcg.sequencer.ordered_lane import LaneStep, OrderedLane
from solders.message import Message
from solders.pubkey import Pubkey

# Measured CU (testnet 10-01) plus margin, per Doom step kind.
LIMITS = {"input": 80_000, "advance": 420_000, "snapshot": 600_000, "strip": 1_150_000, "commit": 1_400_000}

def limit_for(steps, i):
    sid = steps[i].step_id
    if i + 1 < len(steps) and "-commit-" in steps[i + 1].step_id:
        return 1_400_000  # grouped final strip pair measured 1.28M
    return next((v for k, v in LIMITS.items() if f"-{k}-" in sid), 1_400_000)

def lane_steps(plan):
    steps = list(plan.steps)
    out = []
    for i, step in enumerate(steps):
        message = Message.from_bytes(step.build_message(type("L", (), {"blockhash": "11111111111111111111111111111111"})()))
        # Rebuild the step's instructions without the driver's compute-limit instruction.
        ixs = []
        keys = message.account_keys
        h = message.header
        signed, ro_signed, ro_unsigned = h.num_required_signatures, h.num_readonly_signed_accounts, h.num_readonly_unsigned_accounts
        writable = lambda j: j < signed - ro_signed if j < signed else j < len(keys) - ro_unsigned
        for cix in message.instructions[1:]:
            from solders.instruction import AccountMeta, Instruction
            metas = [AccountMeta(keys[j], j < signed, writable(j)) for j in cix.accounts]
            ixs.append(Instruction(keys[cix.program_id_index], bytes(cix.data), metas))
        out.append(LaneStep(step.step_id, tuple(ixs), limit_for(steps, i)))
    return out

def multiphase_steps(config, plan, cursor, phases):
    """Replace the snapshot-copy steps with single-instruction multi-phase
    steps (DCG run_view_phase with a trailing phases byte)."""
    program = Pubkey.from_string(config["program_id"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, config["session_id"])
    base = lane_steps(plan)
    first = next(i for i, s in enumerate(base) if "-snapshot-0" in s.step_id)
    last = max(i for i, s in enumerate(base) if "-snapshot-0" in s.step_id)
    total = D.SNAPSHOT_BOUNDARY // D.VIEW_PHASE_BYTES
    out = base[:first]
    at = 0
    while at < total:
        n = min(phases, total - at)
        ix = D.run_view_phase(program, authority, a, at * D.VIEW_PHASE_BYTES, False)
        ix = type(ix)(ix.program_id, bytes(ix.data) + bytes([n]), ix.accounts)
        # ~23.5k CU per copied phase plus the heavier first phase (measured 10-01)
        # A uniform request keeps the copies in order: a smaller final copy
        # would otherwise fit a block its larger predecessors could not.
        out.append(LaneStep(f"frame-snapshot-x{n}-{at * D.VIEW_PHASE_BYTES:08d}", (ix,),
                            min(1_400_000, 260_000 + 26_000 * phases)))
        at += n
    return out + base[last + 1:]


def stream_capacity_steps(config, lane, steps, cursor, known=None):
    """Keep the input stream ahead: drop input writes at or past the stream's
    capacity, and at cursor == capacity grow the stream by 512 slots (DCG tag
    231 op 1) and write the input that the dropped write would have carried."""
    import base64, struct
    from solders.instruction import AccountMeta, Instruction
    program = Pubkey.from_string(config["program_id"])
    payer = Pubkey.from_string(config["pubkeys"]["payer"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, config["session_id"])
    if known is not None and known.get("capacity"):
        capacity = known["capacity"]
    else:
        raw = base64.b64decode(lane.rpc("getAccountInfo", [str(a.stream), {"encoding": "base64",
                                        "commitment": "processed"}])["value"]["data"][0])
        capacity = struct.unpack_from("<I", raw, 72)[0]
    commands = Path(config["commands_path"]).read_bytes()
    count = len(commands) // 4
    def write(seq):
        return D.write_input(program, authority, a, seq, commands[(seq % count) * 4:(seq % count + 1) * 4])
    prefix = []
    if cursor == capacity:
        grow = Instruction(program, bytes([231, D.WIRE_VERSION, 1]) + struct.pack("<I", capacity + 512),
                           [AccountMeta(payer, True, True), AccountMeta(a.session, False, True),
                            AccountMeta(a.stream, False, True), AccountMeta(D.SYSTEM_PROGRAM, False, False)])
        prefix = [LaneStep(f"stream-grow-{capacity + 512}", (grow,), 80_000),
                  LaneStep(f"input-{cursor:08d}", (write(cursor),), 80_000)]
        capacity += 512
    if known is not None:
        known["capacity"] = capacity
    out = []
    for step in steps:
        if "-input-" in step.step_id:
            kept = tuple(ix for ix, seq in zip(step.instructions, range(cursor + 1, cursor + 9)) if seq < capacity)
            if not kept:
                continue
            step = LaneStep(step.step_id, kept, step.compute_unit_limit)
        out.append(step)
    return prefix + out


def pack_steps(steps):
    """Fewer transactions per frame: pair advances, fold the input writes into
    the first advance pair, and fold the commit into the final strip
    transaction (atomic, so the commit can never land before it)."""
    adv = [s for s in steps if "-advance-" in s.step_id]
    inp = [s for s in steps if "-input-" in s.step_id]
    commit = [s for s in steps if "-commit-" in s.step_id]
    rest = [s for s in steps if s not in adv and s not in inp and s not in commit]
    out = [s for s in rest if s.step_id.startswith(("stream-grow", "input-"))]
    rest = [s for s in rest if s not in out]
    pairs = [adv[i:i + 2] for i in range(0, len(adv), 2)]
    for j, pair in enumerate(pairs):
        ixs = tuple(ix for s in pair for ix in s.instructions)
        if j == 0 and inp:
            ixs = tuple(ix for s in inp for ix in s.instructions) + ixs
        # 4 x 610k fits a 2.5M block; the first pair also carries the 8 input writes (~50k)
        out.append(LaneStep(pair[0].step_id.replace("-advance-", "-advance2-"), ixs, 610_000 if j else 680_000))
    # Strip renders: measured 604k-964k CU rising along the frame (10-01); tight,
    # non-decreasing requests let three early strips share a 2.5M block.
    strip_limits = [700_000, 800_000, 840_000, 880_000, 900_000, 900_000, 900_000]
    k = 0
    for i, step in enumerate(rest):
        if "-strip-phase-" in step.step_id and k < len(strip_limits) and i != len(rest) - 1:
            rest[i] = LaneStep(step.step_id, step.instructions, strip_limits[k])
            k += 1
    if commit and rest:
        last = rest[-1]
        rest[-1] = LaneStep(last.step_id + "+commit", last.instructions + commit[0].instructions, 1_400_000)
    return out + rest


def session_cursor(config, lane):
    import base64
    program = Pubkey.from_string(config["program_id"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, config["session_id"])
    raw = base64.b64decode(lane.rpc("getAccountInfo", [str(a.session), {"encoding": "base64",
                                    "commitment": "processed"}])["value"]["data"][0])
    return int.from_bytes(raw[112:116], "little")


async def heal(config, lane, view_phases, log=print):
    """Bring the session to a frame boundary: repair the frame before the
    cursor and the one at it (in-order simulate-and-skip), then keep repairing
    while frames with pre-written inputs still advance."""
    cursor = session_cursor(config, lane)
    start = max(0, (cursor // 8) * 8 - 8)
    c = start
    while True:
        plan, _x, _y = D.campaign_plan(config, 1, c, frame_offset=40_000)
        steps = multiphase_steps(config, plan, c, view_phases) if view_phases else lane_steps(plan)
        steps = stream_capacity_steps(config, lane, steps, c)
        # One input write per step (cursor..cursor+8): a multi-write step fails
        # atomically on any duplicate and can never fill a gap.
        program = Pubkey.from_string(config["program_id"])
        authority = Pubkey.from_string(config["pubkeys"]["authority"])
        addrs = D.Addresses.derive(program, authority, config["session_id"])
        commands = Path(config["commands_path"]).read_bytes()
        count = len(commands) // 4
        singles = [LaneStep(f"input-{q:08d}", (D.write_input(program, authority, addrs, q,
                            commands[(q % count) * 4:(q % count + 1) * 4]),), 80_000) for q in range(c, c + 9)]
        steps = singles + [st for st in steps if "-input-" not in st.step_id]
        sent, skipped = await lane.repair(steps, 0)
        now = session_cursor(config, lane)
        log(json.dumps({"heal_frame": c, "sent": sent, "skipped": skipped, "cursor": now}))
        if now < c + 8:
            return now
        non_input = sum(1 for st in steps if not st.step_id.startswith("input-"))
        if c >= cursor and (sent == 0 or sent >= non_input):
            # every frame step had to be sent: nothing was pre-written, so the
            # pollution is cleared
            return now
        c += 8


async def overlapped(config, lane, a, t0):
    """One frame in flight ahead of the one being confirmed. A failure drains
    the in-flight frame, heals from the session cursor and resumes there."""
    known, bh, bh_at = {}, None, 0.0
    cursor, done, failures, prev = a.start_cursor, 0, 0, None

    async def send_frame(c):
        nonlocal bh, bh_at
        if time.monotonic() - bh_at > 3.0:
            bh, bh_at = lane.blockhash(), time.monotonic()
        plan, _x, _y = D.campaign_plan(config, 1, c, frame_offset=50_000)
        steps = multiphase_steps(config, plan, c, a.view_phases) if a.view_phases else lane_steps(plan)
        steps = stream_capacity_steps(config, lane, steps, c, known)
        # Global priority order across in-flight frames; priority step 1
        # micro-lamport keeps the testnet fee near 0.07 FOGO per frame.
        return c, await lane.send(steps, blockhash=bh, epoch=c // 8)

    while done < a.frames:
        if prev is not None and a.overlap_delay:
            wait = prev[1].started + a.overlap_delay - time.monotonic()
            if wait > 0:
                time.sleep(wait)
        current = await send_frame(cursor)
        cursor += 8
        if prev is not None:
            r = await lane.wait(prev[1], watch_last=True, poll_seconds=0.02, repair=False)
            if r.failed_first is None:
                done += 1
                print(json.dumps({"cursor": prev[0], "ok": True, "landed_s": round(r.landed_seconds, 2)}), flush=True)
            else:
                failures += 1
                print(json.dumps({"cursor": prev[0], "ok": False, "error": str(r.first_error)[:100]}), flush=True)
                await lane.wait(current[1], watch_last=True, poll_seconds=0.05, repair=False, max_resends=0,
                                wait_seconds=2.0)
                known.clear()
                cursor = await heal(config, lane, a.view_phases, log=lambda m: None)
                prev = None
                continue
        prev = current
    if prev is not None:
        r = await lane.wait(prev[1], watch_last=True, poll_seconds=0.02)
        done += 1 if r.failed_first is None else 0
    total = time.monotonic() - t0
    print(json.dumps({"frames": done, "failures": failures, "seconds": round(total, 2),
                      "frames_per_second": round(done / total, 3)}))
    return 0


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", type=Path, required=True)
    ap.add_argument("--start-cursor", type=int, default=-1, help="-1: heal and start at the session cursor")
    ap.add_argument("--frames", type=int, default=1)
    ap.add_argument("--url", default="http://64.130.37.72:8899")
    ap.add_argument("--view-phases", type=int, default=0, help="phases per snapshot instruction (needs f2d1f59 image)")
    ap.add_argument("--fast-loop", action="store_true", help="reuse blockhash, cache capacity, watch only the commit")
    ap.add_argument("--overlap", action="store_true", help="keep one frame in flight ahead of the one being confirmed")
    ap.add_argument("--pack", action="store_true", help="pair advances, fold inputs and commit into neighbours")
    ap.add_argument("--overlap-delay", type=float, default=0.0, help="seconds after a frame is sent before the next is sent")
    a = ap.parse_args()
    config = D.load_config(a.run_dir)
    signer = D.MultiKeypairSigner((config["key_paths"]["payer"], config["key_paths"]["authority"]))

    async def sign(message: bytes):
        signed = await signer.sign(message, None)
        return signed.signature, signed.raw_bytes

    lane = OrderedLane(a.url, Pubkey.from_string(config["pubkeys"]["payer"]), sign,
                       priority_step=1 if a.overlap else 1000)
    if a.start_cursor < 0:
        a.start_cursor = await heal(config, lane, a.view_phases)
        if a.start_cursor % 8:
            raise SystemExit(f"session cursor {a.start_cursor} is not at a frame boundary after healing")
    t0 = time.monotonic()
    ok = 0
    known, bh, bh_at = {}, None, 0.0
    if a.overlap:
        return await overlapped(config, lane, a, t0)
    for f in range(a.frames):
        cursor = a.start_cursor + 8 * f
        plan, _addresses, _commits = D.campaign_plan(config, 1, cursor, frame_offset=20_000 + f)
        steps = multiphase_steps(config, plan, cursor, a.view_phases) if a.view_phases else lane_steps(plan)
        if a.fast_loop:
            if time.monotonic() - bh_at > 3.0:
                bh, bh_at = lane.blockhash(), time.monotonic()
            steps = stream_capacity_steps(config, lane, steps, cursor, known)
            if a.pack:
                steps = pack_steps(steps)
            r = await lane.run(steps, blockhash=bh, watch_last=True, poll_seconds=0.03, salt=(cursor // 8) % 997)
        else:
            steps = stream_capacity_steps(config, lane, steps, cursor)
            r = await lane.run(steps)
        print(json.dumps({"cursor": cursor, "landed": r.landed, "first_failed": r.failed_first,
                          "repaired": r.repaired, "skipped": r.skipped, "error": str(r.first_error)[:120], "slots": r.slots,
                          "landed_s": round(r.landed_seconds, 2)}), flush=True)
        ok += 1
    total = time.monotonic() - t0
    print(json.dumps({"frames": ok, "seconds": round(total, 2), "frames_per_second": round(ok / total, 3)}))
    return 0

if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
