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

async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", type=Path, required=True)
    ap.add_argument("--start-cursor", type=int, required=True)
    ap.add_argument("--frames", type=int, default=1)
    ap.add_argument("--url", default="http://64.130.37.72:8899")
    a = ap.parse_args()
    config = D.load_config(a.run_dir)
    signer = D.MultiKeypairSigner((config["key_paths"]["payer"], config["key_paths"]["authority"]))

    async def sign(message: bytes):
        signed = await signer.sign(message, None)
        return signed.signature, signed.raw_bytes

    lane = OrderedLane(a.url, Pubkey.from_string(config["pubkeys"]["payer"]), sign)
    t0 = time.monotonic()
    ok = 0
    for f in range(a.frames):
        cursor = a.start_cursor + 8 * f
        plan, _addresses, _commits = D.campaign_plan(config, 1, cursor, frame_offset=20_000 + f)
        r = await lane.run(lane_steps(plan))
        print(json.dumps({"cursor": cursor, "landed": r.landed, "first_failed": r.failed_first,
                          "repaired": r.repaired, "skipped": r.skipped,
                          "landed_s": round(r.landed_seconds, 2)}), flush=True)
        ok += 1
    total = time.monotonic() - t0
    print(json.dumps({"frames": ok, "seconds": round(total, 2), "frames_per_second": round(ok / total, 3)}))
    return 0

if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
