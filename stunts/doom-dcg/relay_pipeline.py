#!/usr/bin/env python3
"""Doom on DCG behind the Node relayer: the playable demo's pipeline.

`stunts/doom-relayer/server.mjs` spawns a pipeline and speaks JSON lines with
it over stdin and stdout (`stunts/doom-relayer/sender.mjs`). The R-series
pipeline is `scripts/doom_relayer_server.py`; this one speaks the same
protocol but drives the DCG Doom session (`stunts/doom-dcg`: a stateful v3
session with render lanes, as `lanes_ordered.py` drives it):

  stdin   {"id": 1, "op": "hello" | "status" | "key_state" | "commands" | "resume" | "stop", ...}
  stdout  one answer per request, carrying its `id`, plus unprompted
          {"event": "started" | "progress" | "frame" | "finished", ...}

Each frame is eight tics. Its inputs are the player's live key state at the
moment the frame is built (attack and use presses since the last frame are
latched, so a short tap is not lost), written in the frame's first advance
transaction. A dead player's use press respawns the player (engine patch
0036). After a frame's commit lands, the nine views (the snapshot and the
eight strips) are read back, and the eight strips go to the page as the
`DOOM-RS1` strip accounts it decodes (`web/doom-play/strip.js`): each view
holds a 128-byte DCG view header, then those 8,128 bytes.

Keys, the session id and the program come from the lanes driver's run
directory (`--doom-run-dir`, `driver-config.json`). Testnet only.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import json
import os
import sys
import threading
import time
import traceback
from pathlib import Path

from solders.pubkey import Pubkey

import lanes_heal
import lanes_plan as P
import testnet_driver as D
from dcg.sequencer.ordered_lane import LaneStep, OrderedLane
from lanes_ordered import LIMITS, PRICE_BUDGET, RPC, account, bad_steps, session_counters, signer, wait_sync

# Vanilla linuxdoom g_game.c movement (walk, run), as the R-series relayer
# encodes them (scripts/doom_relayer_server.py, LiveCommands).
FORWARD_MOVE = (25, 50)
SIDE_MOVE = (24, 40)
BT_ATTACK = 0x01
BT_USE = 0x02
KEY_ATTACK = 1 << 6
KEY_USE = 1 << 7
VIEW_HEADER = 128
STRIP_ACCOUNT_BYTES = D.STRIP_BYTES  # DOOM-RS1: 128-byte header + 8,000 pixels
LAMPORTS = 1_000_000_000

_out_lock = threading.Lock()


def emit(message: dict) -> None:
    with _out_lock:
        sys.stdout.write(json.dumps(message, default=str) + "\n")
        sys.stdout.flush()


def ticcmd(mask: int, tic: int) -> bytes:
    """One Doom ticcmd for a held key mask (forward, back, left, right,
    strafe left, strafe right, attack, use, run)."""
    speed = 1 if mask & (1 << 8) else 0
    forward = (FORWARD_MOVE[speed] if mask & 1 else 0) - (FORWARD_MOVE[speed] if mask & 2 else 0)
    side = (SIDE_MOVE[speed] if mask & 32 else 0) - (SIDE_MOVE[speed] if mask & 16 else 0)
    turn = ((1 if mask & 4 else 0) - (1 if mask & 8 else 0)) if tic % 4 == 0 else 0
    buttons = (BT_ATTACK if mask & KEY_ATTACK else 0) | (BT_USE if mask & KEY_USE else 0)
    return bytes((forward & 0xFF, side & 0xFF, (turn * 4) & 0xFF, buttons))


class Keys:
    """The player's key state, written by the request handler and read when a
    frame is built."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.mask = 0
        self.latched = 0  # attack/use pressed since the last frame
        self.events = 0
        self.last_at = 0.0

    def set(self, mask: int) -> None:
        if mask < 0 or mask & ~0x1FF:
            raise ValueError("the key mask sets an unknown key")
        with self.lock:
            self.mask = mask
            self.latched |= mask & (KEY_ATTACK | KEY_USE)
            self.events += 1
            self.last_at = time.time()

    def take(self) -> int:
        with self.lock:
            mask = self.mask | self.latched
            self.latched = 0
            return mask


class Pipeline:
    def __init__(self, args) -> None:
        self.args = args
        self.config = D.load_config(args.doom_run_dir)
        c = self.config
        self.program = Pubkey.from_string(c["program_id"])
        self.payer = Pubkey.from_string(c["pubkeys"]["payer"])
        self.authority = Pubkey.from_string(c["pubkeys"]["authority"])
        self.a = D.Addresses.derive(self.program, self.authority, c["session_id"])
        self.ring = bool(D.session_api(c)[1] & D.FEATURE_RING_STREAM)
        self.lanes = P.lanes_for(self.program, self.a, args.lanes)
        self.keys = Keys()
        self.resumed = asyncio.Event()
        self.stopping = False
        self.started = time.monotonic()
        self.frames: list[dict] = []
        self.latest_frame: dict | None = None
        self.failures: list[str] = []
        self.cursor = 0
        self.capacity = 0
        self.gametic = None
        self.spend_lamports = 0
        self.balances0: dict[str, int] = {}

    # --- chain reads -------------------------------------------------------------------

    def rpc(self, method: str, params: list):
        import urllib.request
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        req = urllib.request.Request(RPC, body, {"Content-Type": "application/json"})
        out = json.loads(urllib.request.urlopen(req, timeout=10).read())
        if "error" in out:
            raise RuntimeError(f"{method}: {out['error']}")
        return out["result"]

    def read_counters(self) -> tuple[int, int, int]:
        capacity, cursor, frontier = session_counters(account(RPC, self.a.session))
        self.capacity, self.cursor = capacity, cursor
        return capacity, cursor, frontier

    def payers(self) -> dict[str, str]:
        c = self.config
        out = {"payer": c["pubkeys"]["payer"]}
        for k in range(self.args.lanes):
            out[f"lane-payer-{k}"] = c["pubkeys"][f"lane-payer-{k}"]
        return out

    def balances(self) -> dict[str, int]:
        keys = list(self.payers().values())
        values = self.rpc("getMultipleAccounts", [keys, {"commitment": "confirmed", "dataSlice": {"offset": 0, "length": 0}}])["value"]
        return {k: (v or {}).get("lamports", 0) for k, v in zip(keys, values)}

    def read_frame(self, target: int, lane: int) -> dict:
        """The nine views after the frame at `target` committed."""
        keys = [str(self.a.views[role]) for role in range(9)]
        values = self.rpc("getMultipleAccounts", [keys, {"commitment": "confirmed", "encoding": "base64"}])["value"]
        if any(v is None for v in values):
            raise RuntimeError("a view account is missing")
        datas = [base64.b64decode(v["data"][0]) for v in values]
        strips = [d[VIEW_HEADER:VIEW_HEADER + STRIP_ACCOUNT_BYTES] for d in datas[1:]]
        if any(len(s) != STRIP_ACCOUNT_BYTES or s[:8] != b"DOOM-RS1" for s in strips):
            raise RuntimeError("a strip view does not hold a DOOM-RS1 strip")
        u32 = lambda raw, at: int.from_bytes(raw[at:at + 4], "little")
        cursors = {u32(s, 20) for s in strips}
        gametics = {u32(s, 24) for s in strips}
        if len(cursors) != 1 or len(gametics) != 1:
            raise RuntimeError(f"torn read: strip cursors {sorted(cursors)}")
        cursor = cursors.pop()
        pixels = bytearray(D.FRAME_PIXELS_BYTES)
        o = 0
        for row in range(D.FRAME_HEIGHT):
            for s in strips:
                start = 128 + row * D.STRIP_WIDTH
                pixels[o:o + D.STRIP_WIDTH] = s[start:start + D.STRIP_WIDTH]
                o += D.STRIP_WIDTH
        self.gametic = gametics.pop()
        return {
            "frame_index": len(self.frames),
            "cursor": cursor,
            "target": target,
            "gametic": self.gametic,
            "buffer": 0,
            "lane": lane,
            "frame_sha256": hashlib.sha256(pixels).hexdigest(),
            "snapshot_hash": strips[0][32:64].hex(),
            "state_root": strips[0][64:96].hex(),
            "snapshot_account": keys[0],
            "strip_accounts": keys[1:],
            "strip_accounts_b64": [base64.b64encode(s).decode() for s in strips],
            # Where each strip starts inside its view account (the page's chain
            # cross-check reads the DOOM-RS1 header from there).
            "view_header_bytes": VIEW_HEADER,
            "completed_seconds": round(time.monotonic() - self.started, 3),
        }

    # --- the protocol -------------------------------------------------------------------

    def describe(self) -> dict:
        return {
            "program": str(self.program),
            "context": str(self.a.session),
            "meta": str(self.a.session),
            "buffer": str(self.a.stream),
            "authority": str(self.authority),
            "state": str(self.a.states[0]),
            "mirror": str(self.a.states[1]),
            "snapshot_account": str(self.a.views[0]),
            "strip_accounts": [str(v) for v in self.a.views[1:9]],
            "capacity": self.capacity,
            "cursor": self.cursor,
            "gametic": self.gametic,
            "start_cursor": self.cursor,
            "staged_frontier": self.cursor,
            "staging_frontier": self.cursor + 64,
            "producer_next_cursor": self.cursor + 8,
            "rpc_urls": [RPC],
            "latest_frame": self.latest_view(),
            "frame_counters": {"completed": len(self.frames), "failed": len(self.failures)},
            "backend": "doom-dcg-lanes",
        }

    def latest_view(self) -> dict | None:
        if not self.latest_frame:
            return None
        return {k: v for k, v in self.latest_frame.items() if k != "strip_accounts_b64"}

    def status(self) -> dict:
        elapsed = max(time.monotonic() - self.started, 1e-9)
        recent = [f for f in self.frames if f["completed_seconds"] > elapsed - 10]
        return {
            **self.describe(),
            "halt": 0,
            "tics_per_second": round(len(recent) * 8 / 10, 2),
            "frames_completed": len(self.frames),
            "frames_per_second": round(len(recent) / 10, 2),
            "spend_fogo": round(self.spend_lamports / LAMPORTS, 6),
            "spend_cap_fogo": self.args.spend_cap_fogo,
            "key_events": self.keys.events,
            "failures": self.failures[-5:],
            "resumed": self.resumed.is_set(),
        }

    async def handle(self, request: dict) -> dict:
        op = request.get("op")
        if op == "hello":
            return self.describe()
        if op == "status":
            return self.status()
        if op == "key_state":
            self.keys.set(int(request.get("key_mask", 0)))
            if not self.resumed.is_set():
                self.resumed.set()  # a key press is a player: start the frames
            return {"accepted": 1, "accepted_from": self.cursor + 8, "cursor": self.cursor,
                    "gametic": self.gametic, "tic_assigned_at_ms": int(time.time() * 1000),
                    "key_state": {"seq": self.cursor + 8, "mask": self.keys.mask},
                    "producer_next_cursor": self.cursor + 8}
        if op == "commands":
            # The V1 path: take the last tic's buttons and movement as the key
            # state (this backend builds each frame from the live state).
            raw = base64.b64decode(request.get("commands", ""))
            count = len(raw) // 4
            return {"accepted": count, "first_seq": request.get("first_seq"), "cursor": self.cursor,
                    "producer_stage_limit": self.cursor + 64, "producer_generated_cursor": self.cursor}
        if op == "resume":
            self.resumed.set()
            return {"resumed": True}
        if op == "stop":
            self.stopping = True
            self.resumed.set()
            return {"stopping": True}
        raise ValueError(f"unknown op {op!r}")

    # --- frames ---------------------------------------------------------------------------

    async def heal(self) -> None:
        """Finish or abort lanes a previous run left mid-render."""
        ns = argparse.Namespace(run_dir=self.args.doom_run_dir, lanes=self.args.lanes)
        await lanes_heal.heal(ns)

    async def run_frames(self) -> None:
        args = self.args
        c = self.config
        kp = c["key_paths"]
        window = max(1, args.price_window)
        ceiling = window + 1
        step_price = max(1, PRICE_BUDGET // (3 * ceiling))
        session = OrderedLane(RPC, self.payer, signer(kp["payer"], kp["authority"]), priority_step=step_price)
        render = [OrderedLane(RPC, Pubkey.from_string(c["pubkeys"][f"lane-payer-{k}"]),
                              signer(kp[f"lane-payer-{k}"], kp["authority"]), priority_step=step_price)
                  for k in range(args.lanes)]
        commits = OrderedLane(RPC, self.payer, signer(kp["payer"], kp["authority"]), priority_step=step_price)
        capacity, cursor0, frontier = self.read_counters()
        if cursor0 % 8:
            raise RuntimeError(f"session cursor {cursor0} is not at a frame boundary")
        lane_free = [asyncio.Event() for _ in range(args.lanes)]
        for e in lane_free:
            e.set()
        committed: dict[int, asyncio.Event] = {}
        commit_sent: dict[int, asyncio.Event] = {}
        session_done: dict[int, asyncio.Event] = {}
        captured: set[int] = set()
        failure: list[str] = []
        tasks: list[asyncio.Task] = []

        async def lane_frame(frame: int, cursor: int, lane: P.Lane, handle, render_lane: OrderedLane):
            target = cursor + 8
            try:
                if frame > 0:
                    await session_done[frame - 1].wait()
                r = await asyncio.to_thread(wait_sync, session, handle, watch_last=True, poll_seconds=0.03,
                                            repair_delay=args.repair_delay)
                session_done[frame].set()
                captured.add(target)
                strips = [LaneStep(f"f{target}-render-{at:08d}",
                                   (P.lane_run(self.program, self.authority, self.a, lane, target, at, n),),
                                   LIMITS["render"]) for at, n in P.render_phases()]
                h = await render_lane.send(strips, blockhash=await asyncio.to_thread(render_lane.blockhash),
                                           epoch=frame % window, epoch_ceiling=ceiling)
                await asyncio.to_thread(wait_sync, render_lane, h, watch_last=True, poll_seconds=0.03,
                                        repair_delay=args.repair_delay)
                if frame > 0:
                    await (committed[frame - 1] if frame % window == 0 else commit_sent[frame - 1]).wait()
                commit = [LaneStep(f"f{target}-commit", (P.lane_commit(self.program, self.authority, self.a, lane, target),),
                                   LIMITS["commit"])]
                h = await commits.send(commit, blockhash=await asyncio.to_thread(commits.blockhash),
                                       epoch=frame % window, epoch_ceiling=ceiling)
                commit_sent[frame].set()
                await asyncio.to_thread(wait_sync, commits, h, watch_last=True, poll_seconds=0.03,
                                        repair_delay=args.repair_delay)
                # The views now hold this frame (newest-only publication), unless a
                # newer one already landed; either way the page gets a whole frame.
                record = await asyncio.to_thread(self.read_frame, target, lane.index)
                self.frames.append({"cursor": record["cursor"], "completed_seconds": record["completed_seconds"]})
                self.latest_frame = record
                self.cursor = max(self.cursor, target)
                emit({"event": "frame", **record, "frames_completed": len(self.frames), "push": True})
            except Exception as e:
                failure.append(f"frame at {target}: {e}")
                emit({"event": "frame_failed", "target": target, "error": str(e)[:300]})
            finally:
                session_done[frame].set()
                commit_sent[frame].set()
                committed[frame].set()
                lane_free[lane.index].set()
                for old in [k for k in committed if k < frame - 2 * args.lanes]:
                    committed.pop(old, None), commit_sent.pop(old, None), session_done.pop(old, None)

        blockhash, bh_at = None, 0.0
        frame = 0
        while not self.stopping and not failure:
            if args.seconds and time.monotonic() - self.started > args.seconds:
                break
            if self.spend_lamports > args.spend_cap_fogo * LAMPORTS:
                self.failures.append("spend cap reached")
                break
            cursor = cursor0 + 8 * frame
            lane = self.lanes[frame % args.lanes]
            await lane_free[lane.index].wait()
            lane_free[lane.index].clear()
            committed[frame] = asyncio.Event()
            commit_sent[frame] = asyncio.Event()
            session_done[frame] = asyncio.Event()
            if time.monotonic() - bh_at > 3.0:
                blockhash, bh_at = await asyncio.to_thread(session.blockhash), time.monotonic()
            mask = self.keys.take()
            inputs = tuple(D.write_input(self.program, self.authority, self.a, seq, ticcmd(mask, seq))
                           for seq in range(max(frontier, cursor), cursor + 8))
            frontier = cursor + 8
            k = args.tics_per_tx
            steps = []
            for t in range(0, 8, k):
                ixs = (inputs if t == 0 else ()) + tuple(D.advance(self.program, self.authority, self.a, cursor + u)
                                                         for u in range(t, t + k))
                limit = min(1_400_000, LIMITS["advance"] * k + (LIMITS["input"] if t == 0 else 0))
                steps.append(LaneStep(f"f{cursor}-advance-{cursor + t:08d}x{k}", ixs, limit))
            steps.append(LaneStep(f"f{cursor}-capture-{cursor + 8:08d}",
                                  tuple(P.capture_instructions(self.program, self.authority, self.a, lane, cursor + 8)),
                                  LIMITS["capture"]))
            handle = await session.send(steps, blockhash=blockhash, epoch=frame % window, epoch_ceiling=ceiling)
            tasks.append(asyncio.create_task(lane_frame(frame, cursor, lane, handle, render[lane.index])))
            tasks = [t for t in tasks if not t.done()]
            # The next frame's session chain follows this capture.
            while cursor + 8 not in captured and not failure and not self.stopping:
                await asyncio.sleep(0.02)
            frame += 1
        await asyncio.gather(*tasks)
        if failure:
            raise RuntimeError("; ".join(failure[:3]))

    async def produce(self) -> None:
        """Frames whenever resumed, healing and restarting after a failure."""
        await self.resumed.wait()
        attempts = 0
        while not self.stopping:
            try:
                await self.run_frames()
                break  # stopped, or the time or spend limit
            except Exception as e:
                attempts += 1
                self.failures.append(str(e)[:300])
                emit({"event": "pipeline_retry", "attempt": attempts, "error": str(e)[:300]})
                if attempts > self.args.max_retries:
                    raise
                await self.heal()

    async def watch_spend(self) -> None:
        while not self.stopping:
            try:
                now = await asyncio.to_thread(self.balances)
                self.spend_lamports = sum(max(0, self.balances0.get(k, v) - v) for k, v in now.items())
            except Exception:
                pass
            await asyncio.sleep(5.0)

    async def progress(self) -> None:
        while not self.stopping:
            await asyncio.sleep(5.0)
            emit({"event": "progress", **self.status()})


async def main_async(args) -> int:
    pipe = Pipeline(args)
    loop = asyncio.get_running_loop()
    requests: asyncio.Queue = asyncio.Queue()

    def reader() -> None:
        for line in sys.stdin:
            line = line.strip()
            if line:
                loop.call_soon_threadsafe(requests.put_nowait, line)
        loop.call_soon_threadsafe(requests.put_nowait, None)

    threading.Thread(target=reader, daemon=True).start()
    await pipe.heal()
    await asyncio.to_thread(pipe.read_counters)
    pipe.balances0 = await asyncio.to_thread(pipe.balances)
    try:
        record = await asyncio.to_thread(pipe.read_frame, pipe.cursor, -1)
        pipe.latest_frame = record
    except Exception:
        pass
    if not args.start_paused:
        pipe.resumed.set()
    emit({"event": "started", "backend": "doom-dcg-lanes", "cursor": pipe.cursor, "session": str(pipe.a.session)})
    producer = asyncio.create_task(pipe.produce())
    side = [asyncio.create_task(pipe.watch_spend()), asyncio.create_task(pipe.progress())]
    code = 0
    while True:
        get = asyncio.create_task(requests.get())
        done, _ = await asyncio.wait({get, producer}, return_when=asyncio.FIRST_COMPLETED)
        if producer in done and get not in done:
            get.cancel()
            if producer.exception():
                emit({"event": "error", "error": str(producer.exception())[:300]})
                code = 1
            break
        line = get.result()
        if line is None:
            pipe.stopping = True
            pipe.resumed.set()
            break
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            continue
        rid = request.get("id")
        try:
            answer = await pipe.handle(request)
            emit({"id": rid, "ok": True, **answer})
        except Exception as e:
            emit({"id": rid, "ok": False, "error": str(e)[:300]})
        if pipe.stopping:
            break
    pipe.stopping = True
    pipe.resumed.set()
    try:
        await asyncio.wait_for(producer, timeout=60)
    except Exception as e:
        emit({"event": "error", "error": f"stopping: {str(e)[:200]}"})
    for t in side:
        t.cancel()
    receipt = {"backend": "doom-dcg-lanes", "session": str(pipe.a.session), "program": str(pipe.program),
               "frames": len(pipe.frames), "end_cursor": pipe.cursor, "spend_fogo": pipe.spend_lamports / LAMPORTS,
               "failures": pipe.failures, "seconds": round(time.monotonic() - pipe.started, 1),
               "key_events": pipe.keys.events}
    if args.run_dir:
        Path(args.run_dir).mkdir(parents=True, exist_ok=True)
        (Path(args.run_dir) / "relayer-session.json").write_text(json.dumps(receipt, indent=1) + "\n")
    emit({"event": "finished", **receipt})
    return code


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--doom-run-dir", type=Path, default=os.environ.get("DOOM_DCG_RUN_DIR"),
                    help="the lanes driver's run directory (driver-config.json; default $DOOM_DCG_RUN_DIR)")
    ap.add_argument("--run-dir", type=Path, help="where the session receipt goes")
    ap.add_argument("--lanes", type=int, default=3)
    ap.add_argument("--tics-per-tx", type=int, default=4, choices=(1, 2, 4, 8))
    ap.add_argument("--price-window", type=int, default=250)
    ap.add_argument("--repair-delay", type=float, default=1.0)
    ap.add_argument("--seconds", type=float, default=0)
    ap.add_argument("--spend-cap-fogo", type=float, default=2.0)
    ap.add_argument("--start-paused", action="store_true")
    ap.add_argument("--max-retries", type=int, default=3)
    # The R-series flags the relayer passes (sender.mjs argv) mean nothing here.
    args, _unknown = ap.parse_known_args()
    if not args.doom_run_dir:
        ap.error("--doom-run-dir or DOOM_DCG_RUN_DIR is required")
    try:
        return asyncio.run(main_async(args))
    except Exception:
        emit({"event": "error", "error": traceback.format_exc()[-600:]})
        return 1


if __name__ == "__main__":
    sys.exit(main())
