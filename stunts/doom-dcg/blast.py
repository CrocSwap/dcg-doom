#!/usr/bin/env python3
"""Pipelined Doom frames: send a whole frame's transactions back-to-back in
order (no per-step wait), then read every status. Measures whether the leader
preserves arrival order for same-account transactions on Fogo testnet.

usage: blast.py --run-dir RUN --start-cursor C --frames N [--gap-ms G] [--url URL]
"""
from __future__ import annotations
import argparse, asyncio, base64, hashlib, json, sys, time, urllib.request
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import testnet_driver as D

def rpc(url, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, body, {"Content-Type": "application/json", "User-Agent": "doom-blast"})
    out = json.loads(urllib.request.urlopen(req, timeout=10).read())
    if "error" in out:
        raise RuntimeError(f"{method}: {out['error']}")
    return out["result"]

def send_pipelined(url, wires, gap_s=0.0):
    """HTTP/1.1 pipelining: every request written in order on one TCP
    connection before any response is read, so arrival order is preserved
    without paying a round trip per transaction."""
    import socket
    from urllib.parse import urlparse
    u = urlparse(url)
    sock = socket.create_connection((u.hostname, u.port or 80), timeout=10)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    for i, wire in enumerate(wires):
        blob = bytearray()
        body = json.dumps({"jsonrpc": "2.0", "id": i, "method": "sendTransaction",
                           "params": [wire, {"encoding": "base64", "skipPreflight": True, "maxRetries": 0}]}).encode()
        blob += (f"POST / HTTP/1.1\r\nHost: {u.hostname}\r\nContent-Type: application/json\r\n"
                 f"Content-Length: {len(body)}\r\nUser-Agent: doom-blast\r\n\r\n").encode() + body
        sock.sendall(bytes(blob))
        if gap_s:
            time.sleep(gap_s)
    got = b""
    while got.count(b"HTTP/1.1 ") < len(wires):
        chunk = sock.recv(65536)
        if not chunk:
            break
        got += chunk
    sock.close()
    return got.count(b'"result"')

async def repair(url, plan, signer, start=0):
    """Walk the frame's steps in order; simulate each against current state.
    A passing simulation is sent and awaited; a refusal means the step already
    landed (cursor/sequence guards), so it is skipped."""
    sent = skipped = 0
    for step in list(plan.steps)[start:]:
        bh = rpc(url, "getLatestBlockhash", [{"commitment": "processed"}])["value"]["blockhash"]
        lease = Lease(bh)
        signed = await signer.sign(step.build_message(lease), lease)
        wire = base64.b64encode(signed.raw_bytes).decode()
        sim = rpc(url, "simulateTransaction", [wire, {"encoding": "base64", "commitment": "processed",
                                                      "replaceRecentBlockhash": False}])["value"]
        if sim.get("err"):
            skipped += 1
            continue
        rpc(url, "sendTransaction", [wire, {"encoding": "base64", "skipPreflight": True, "maxRetries": 0}])
        end = time.monotonic() + 15
        while time.monotonic() < end:
            v = rpc(url, "getSignatureStatuses", [[signed.signature]])["value"][0]
            if v is not None:
                if v.get("err"):
                    raise RuntimeError(f"repair step {step.step_id} failed: {v['err']}")
                break
            time.sleep(0.05)
        else:
            raise RuntimeError(f"repair step {step.step_id} did not land")
        sent += 1
    return sent, skipped

PRICE = [0]
LIMIT = [None]
# Measured CU (testnet 10-01) plus margin; tight requests let several steps
# share one block under Fogo's 2.5M per-writable-account block cap.
STEP_LIMITS = {"input": 80_000, "advance": 420_000, "snapshot": 600_000, "strip": 1_150_000, "commit": 1_400_000}  # commit must not share a block with the last strip

def step_limit(step_id):
    for key, value in STEP_LIMITS.items():
        if f"-{key}-" in step_id:
            return value
    return 1_400_000

class _PricedMessage:
    """Wraps solders Message so each built step carries a compute-unit price."""
    @staticmethod
    def new_with_blockhash(instructions, payer, blockhash):
        from solders.compute_budget import set_compute_unit_price
        from solders.message import Message
        from solders.compute_budget import set_compute_unit_limit
        body = list(instructions)
        if LIMIT[0] is not None:
            body[0] = set_compute_unit_limit(LIMIT[0])
        return Message.new_with_blockhash([set_compute_unit_price(PRICE[0]), *body], payer, blockhash)

    @staticmethod
    def from_bytes(raw):
        from solders.message import Message
        return Message.from_bytes(raw)

class Lease:
    def __init__(self, blockhash): self.blockhash = blockhash

async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", type=Path, required=True)
    ap.add_argument("--start-cursor", type=int, required=True)
    ap.add_argument("--frames", type=int, default=1)
    ap.add_argument("--gap-ms", type=float, default=15)
    ap.add_argument("--url", default=D.TESTNET_NODE_URLS[0])
    ap.add_argument("--pipeline", action="store_true")
    ap.add_argument("--repair", action="store_true")
    ap.add_argument("--write-gap-ms", type=float, default=0.0)
    ap.add_argument("--priority-base", type=int, default=0, help="micro-lamports per CU step")
    ap.add_argument("--tight-limits", action="store_true")
    ap.add_argument("--show-slots", action="store_true")
    ap.add_argument("--phases-per-tx", type=int, default=0)
    a = ap.parse_args()
    config = D.load_config(a.run_dir)
    if a.phases_per_tx:
        D.MAX_PHASES_PER_TX = a.phases_per_tx
        STEP_LIMITS["snapshot"] = min(1_400_000, 600_000 * a.phases_per_tx // 13)
    if a.priority_base or a.tight_limits:
        D.Message = _PricedMessage
    signer = D.MultiKeypairSigner((config["key_paths"]["payer"], config["key_paths"]["authority"]))
    results = []
    started_all = time.monotonic()
    for f in range(a.frames):
        cursor = a.start_cursor + 8 * f
        plan, addresses, commits = D.campaign_plan(config, 1, cursor, frame_offset=10_000 + f)
        bh = rpc(a.url, "getLatestBlockhash", [{"commitment": "confirmed"}])["value"]["blockhash"]
        lease = Lease(bh)
        packets = []
        n = len(plan.steps)
        for i, step in enumerate(plan.steps):
            # Strictly decreasing priority keeps earlier steps first when the
            # leader's scheduler holds conflicting transactions together.
            PRICE[0] = a.priority_base * (n - i) if a.priority_base else 0
            LIMIT[0] = step_limit(step.step_id) if a.tight_limits else None
            if a.tight_limits and i + 1 < n and "-commit-" in plan.steps[i + 1].step_id:
                LIMIT[0] = 1_400_000  # the grouped final strip pair measured 1.28M
            signed = await signer.sign(step.build_message(lease), lease)
            packets.append((step.step_id, signed.signature, base64.b64encode(signed.raw_bytes).decode()))
        t0 = time.monotonic()
        if a.pipeline:
            send_pipelined(a.url, [p[2] for p in packets], a.write_gap_ms / 1000)
        else:
            for _sid, _sig, wire in packets:
                rpc(a.url, "sendTransaction", [wire, {"encoding": "base64", "skipPreflight": True, "maxRetries": 0}])
                time.sleep(a.gap_ms / 1000)
        sent = time.monotonic() - t0
        sigs = [p[1] for p in packets]
        deadline = time.monotonic() + 20
        statuses = [None] * len(sigs)
        while time.monotonic() < deadline:
            vals = rpc(a.url, "getSignatureStatuses", [sigs])["value"]
            statuses = vals
            if all(v is not None for v in vals) or any(v and v.get("err") for v in vals):
                break
            time.sleep(0.1)
        landed = time.monotonic() - t0
        ok = sum(1 for v in statuses if v and not v.get("err"))
        errs = [(packets[i][0], v.get("err")) for i, v in enumerate(statuses) if v and v.get("err")]
        missing = [packets[i][0] for i, v in enumerate(statuses) if v is None]
        slots = sorted({v["slot"] for v in statuses if v})
        if a.show_slots:
            base = min(v["slot"] for v in statuses if v)
            print("slots", [v["slot"] - base if v else None for v in statuses], flush=True)
        rec = {"cursor": cursor, "txs": len(sigs), "ok": ok, "errors": errs[:3], "missing": missing[:3],
               "send_s": round(sent, 2), "all_landed_s": round(landed, 2), "slot_span": (slots[-1] - slots[0]) if slots else None}
        if (errs or missing) and a.repair:
            time.sleep(1.0)
            first_bad = min(i for i, v in enumerate(statuses) if v is None or v.get("err"))
            fixed, skipped = await repair(a.url, plan, signer, first_bad)
            rec.update({"repaired": fixed, "skipped": skipped, "repair_s": round(time.monotonic() - t0, 2)})
            rec["ok"] = rec["txs"]
            errs = missing = []
        print(json.dumps(rec), flush=True)
        results.append(rec)
        if errs or missing:
            break
    total = time.monotonic() - started_all
    done = sum(1 for r in results if r["ok"] == r["txs"])
    print(json.dumps({"frames_ok": done, "seconds": round(total, 2), "frames_per_second": round(done / total, 3)}))
    return 0 if done == a.frames else 1

if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
