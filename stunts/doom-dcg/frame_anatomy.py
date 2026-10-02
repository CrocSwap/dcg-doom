#!/usr/bin/env python3
"""Where does a Doom frame's time go? Run one frame through OrderedLane, then
read every step's transaction, block, and leader."""
from __future__ import annotations
import argparse, asyncio, json, sys, collections
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import testnet_driver as D
import lane_campaign as LC
from dcg.sequencer.ordered_lane import OrderedLane
from solders.pubkey import Pubkey

async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", type=Path, required=True)
    ap.add_argument("--start-cursor", type=int, required=True)
    ap.add_argument("--url", default="http://64.130.37.72:8899")
    ap.add_argument("--phases-per-tx", type=int, default=0)
    a = ap.parse_args()
    if a.phases_per_tx:
        D.MAX_PHASES_PER_TX = a.phases_per_tx
    config = D.load_config(a.run_dir)
    signer = D.MultiKeypairSigner((config["key_paths"]["payer"], config["key_paths"]["authority"]))
    async def sign(m):
        s = await signer.sign(m, None); return s.signature, s.raw_bytes
    lane = OrderedLane(a.url, Pubkey.from_string(config["pubkeys"]["payer"]), sign)
    plan, _a, _c = D.campaign_plan(config, 1, a.start_cursor, frame_offset=30_000)
    steps = LC.lane_steps(plan)
    blockhash = lane.blockhash(); n = len(steps)
    built = [await lane._build(s, blockhash, 1000 * (n - i)) for i, s in enumerate(steps)]
    lane._send_ordered([raw for _s, raw in built])
    import time
    sigs = [s for s, _r in built]
    end = time.monotonic() + 20
    while time.monotonic() < end:
        st = lane.rpc("getSignatureStatuses", [sigs])["value"]
        if all(st): break
        time.sleep(0.2)
    time.sleep(2)
    sizes = {}
    rows = []
    for i, (sig, step) in enumerate(zip(sigs, steps)):
        tx = None
        for _ in range(10):
            tx = lane.rpc("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 0, "commitment": "confirmed"}])
            if tx: break
            time.sleep(0.5)
        keys = tx["transaction"]["message"]["accountKeys"]; hdr = tx["transaction"]["message"]["header"]
        ns, nrs, nru = hdr["numRequiredSignatures"], hdr["numReadonlySignedAccounts"], hdr["numReadonlyUnsignedAccounts"]
        writable = [k for j, k in enumerate(keys) if (j < ns - nrs) or (ns <= j < len(keys) - nru)]
        for k in keys:
            if k not in sizes:
                info = lane.rpc("getAccountInfo", [k, {"encoding": "base64", "dataSlice": {"offset": 0, "length": 0}}])["value"]
                sizes[k] = info["space"] if info and "space" in info else (0 if not info else -1)
        rows.append({"i": i, "step": step.step_id.split("-", 2)[2], "slot": tx["slot"], "cu": tx["meta"]["computeUnitsConsumed"],
                     "err": tx["meta"]["err"], "loaded_kb": round(sum(max(sizes[k], 0) for k in keys) / 1024),
                     "writable_kb": round(sum(max(sizes[k], 0) for k in writable) / 1024), "n_writable": len(writable)})
    base = min(r["slot"] for r in rows)
    lo, hi = base, max(r["slot"] for r in rows)
    leaders = lane.rpc("getSlotLeaders", [lo, hi - lo + 1])
    blocks = {}
    for s in range(lo, hi + 1):
        try:
            b = lane.rpc("getBlock", [s, {"encoding": "json", "transactionDetails": "signatures", "rewards": False, "maxSupportedTransactionVersion": 0}])
            blocks[s] = len(b["signatures"]) if b else None
        except Exception as e:
            blocks[s] = f"skip"
    for r in rows:
        r["rel"] = r["slot"] - base
        print(json.dumps(r))
    print("slots", {s - base: (blocks[s], leaders[s - lo][:6]) for s in range(lo, hi + 1)})

asyncio.run(main())
