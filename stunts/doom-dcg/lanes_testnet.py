#!/usr/bin/env python3
"""Doom on DCG render lanes, on Fogo testnet (lanes slice 4).

A fresh Doom program and session (owner, 2026-10-04); the existing Doom
testnet program is untouched. Only the explicit testnet RPC is used.

usage:
  lanes_testnet.py --run-dir RUN prepare --lanes 3
  lanes_testnet.py --run-dir RUN fund
  lanes_testnet.py --run-dir RUN deploy --image IMAGE.so
  lanes_testnet.py --run-dir RUN provision
  lanes_testnet.py --run-dir RUN campaign --lanes L --frames N --start-cursor C --journal NAME
Keys live under out/chain/<run name>/ (0600) and are never printed.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import subprocess
from pathlib import Path

from solders.pubkey import Pubkey

import lanes_plan as P
import testnet_driver as D

RPC = "http://64.130.44.172:8899"
FUNDER = D.ROOT / "out/chain/fogo-payer.json"
FUNDING = {"payer": 1_200, "lane": 30, "authority": 1}


def prepare(args) -> None:
    run_dir: Path = args.run_dir
    run_dir.mkdir(parents=True, exist_ok=True)
    config_path = run_dir / "driver-config.json"
    if config_path.exists():
        raise RuntimeError(f"refusing to replace {config_path}")
    key_dir = D.ROOT / "out/chain" / run_dir.name
    names = ["payer", "authority", "doom-program", *(f"lane-payer-{k}" for k in range(args.lanes))]
    key_paths, pubkeys = {}, {}
    for name in names:
        path = key_dir / f"{name}.json"
        key_paths[name] = str(path)
        pubkeys[name] = str(D.write_keypair(path).pubkey())
    config = {
        "schema": 1, "network": "testnet", "rpc_urls": [RPC], "expected_genesis": D.TESTNET_GENESIS,
        "key_paths": key_paths, "pubkeys": pubkeys, "program_id": pubkeys["doom-program"],
        "session_id": secrets.randbits(63), "lanes": args.lanes,
        "resource_key": str(D.RESOURCE_KEY), "commands_path": str(D.DEFAULT_COMMANDS.resolve()),
    }
    config_path.write_text(json.dumps(config, sort_keys=True, indent=2) + "\n")
    os.chmod(config_path, 0o600)
    print(json.dumps({"run_dir": str(run_dir), "public_ids": pubkeys}, sort_keys=True))


def fund(args) -> None:
    c = D.load_config(args.run_dir)
    for name, pubkey in c["pubkeys"].items():
        amount = FUNDING["lane"] if name.startswith("lane-payer") else FUNDING.get(name)
        if not amount:
            continue
        subprocess.run(["solana", "transfer", "--url", RPC, "--keypair", str(FUNDER), "--allow-unfunded-recipient",
                        "--commitment", "confirmed", pubkey, str(amount)], check=True,
                       stdout=subprocess.DEVNULL)
        print(json.dumps({"funded": name, "fogo": amount}))


def deploy(args) -> None:
    c = D.load_config(args.run_dir)
    subprocess.run(["solana", "program", "deploy", "--url", RPC, "--use-rpc", "--commitment", "confirmed",
                    "--keypair", c["key_paths"]["payer"], "--program-id", c["key_paths"]["doom-program"],
                    str(args.image)], check=True)
    print(json.dumps({"deployed": c["program_id"], "image": str(args.image)}))


def _patch(config) -> None:
    """Open with lanes, and sign with every lane payer as well."""
    lanes = config["lanes"]
    open_session = D.open_session

    def open_with_lanes(*a, **kw):
        # lanes (and features, on a v3-r2 program) in the order the program reads them
        return open_session(*a, lanes=lanes, **kw)
    D.open_session = open_with_lanes

    def make_sequencer(cfg, endpoints, key_paths, *, send_rate, max_in_flight, event_hook=None):
        seq = make_sequencer_orig(cfg, endpoints, key_paths, send_rate=send_rate,
                                  max_in_flight=max_in_flight, event_hook=event_hook)
        seq.signer = D.MultiKeypairSigner([key_paths["payer"], key_paths["authority"],
                                           *(key_paths[f"lane-payer-{k}"] for k in range(lanes))])
        return seq
    make_sequencer_orig = D.make_sequencer
    D.make_sequencer = make_sequencer


def _measured_init_cu() -> list[int]:
    """Measured CU per initialization phase (Doom ProgramTest on the lanes image)."""
    m = json.loads((D.ROOT / "out/runs/lanes-doom-slice2-2026-10-04/baseline-metrics.json").read_text())
    return [int(r["cu"]) for r in m["initialization"]["cu_by_phase"]]


def provision_plan(c, resource: bytes):
    """Provisioning as a dependency graph (rewritten after the serial plan hit
    the 600 s variance): resource, state and lane chains run in parallel, and
    initialization phases are packed by measured CU."""
    program = Pubkey.from_string(c["program_id"])
    payer = Pubkey.from_string(c["pubkeys"]["payer"])
    authority = Pubkey.from_string(c["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, c["session_id"])
    steps = []

    def step(name, ixs, deps, locks, cls, post=None):
        steps.append(D.make_step(program=program, payer=payer, instructions=ixs, step_id=name,
                                 dependencies=tuple(d for d in deps if d), locks=locks, compute_class=cls,
                                 postcondition=post))
        return name

    def chain(prefix, ixs, dep, locks, cls, per_tx=8):
        prev = dep
        for n, batch in enumerate(D.packet_safe_batches(program, payer, ixs)) if per_tx is None else \
                enumerate(ixs[i:i + per_tx] for i in range(0, len(ixs), per_tx)):
            prev = step(f"{prefix}-{n:04d}", list(batch), (prev,), locks, cls)
        return prev

    with_authority, features = D.session_api(c)
    creator = authority if with_authority else None
    s = step("open-session", [D.open_session(program, payer, authority, a, c["session_id"], features)], (),
             (a.session, a.resource), "doom-open-session", D.exists_check((a.session,)))
    s = step("create-stream", [D.create_stream(program, payer, a, creator)], (s,), (a.session, a.stream), "doom-create-stream")
    s = step("create-state", [D.create_state(program, payer, a, creator)], (s,), (a.session, *a.states), "doom-create-state")
    commands = Path(c["commands_path"]).read_bytes()
    base = step("write-input-0", [D.write_input(program, authority, a, 0, commands[:4])], (s,),
                (a.session, a.stream), "doom-write-input")
    # Resource: grow, then upload (packet-packed).
    grows, allocated = [], D.CHILD_GROW_BYTES
    while allocated < len(resource):
        allocated = min(allocated + D.CHILD_GROW_BYTES, len(resource))
        grows.append(D.resource_grow(program, payer, authority, a, allocated))
    r = chain("resource-grow", grows, base, (a.resource,), "doom-resource-grow", per_tx=30)
    levels = D.resource_tree(resource)
    uploads = [D.resource_upload(program, authority, a, i, D.resource_proof(levels, i)) for i in range(len(levels[0]))]
    r = chain("resource-upload", uploads, r, (a.resource,), "doom-resource-upload", per_tx=None)
    # State spans: one grow chain per span, in parallel.
    span_ends = []
    for index, desired in enumerate(D.STATE_LENGTHS):
        ops, allocated = [], min(desired, D.CHILD_GROW_BYTES)
        while allocated < desired:
            allocated = min(allocated + D.CHILD_GROW_BYTES, desired)
            ops.append(D.grow_state(program, payer, a, index))
        span_ends.append(chain(f"state-grow-{index}", ops, base, (a.states[index],), "doom-state-grow", per_tx=30))
    init = step("begin-initialization", [D.begin_initialization(program, authority, a)], (r, *span_ends),
                (a.session, a.resource, *a.states), "doom-begin-initialization")
    # Initialization phases, packed by measured CU (each phase is one instruction).
    cus = _measured_init_cu()
    batch, batch_cu, n = [], 0, 0
    for phase, cu in enumerate(cus):
        ix = D.run_initialization(program, authority, a, phase * 16_384)
        if batch and (batch_cu + cu > 1_150_000 or len(D.packet_safe_batches(program, payer, [*batch, ix])) > 1):
            init = step(f"initialize-{n:04d}", batch, (init,), (a.session, a.resource, *a.states), "doom-init")
            batch, batch_cu, n = [], 0, n + 1
        batch.append(ix)
        batch_cu += cu
    init = step(f"initialize-{n:04d}", batch, (init,), (a.session, a.resource, *a.states), "doom-init")
    # Output views (the single-workspace path is unused on a lane session).
    views = [D.create_view(program, payer, a, 0, D.SNAPSHOT_ABI, D.SNAPSHOT_BYTES, creator)]
    views += [D.create_view(program, payer, a, role, D.STRIP_ABIS[role - 1], D.STRIP_BYTES, creator) for role in range(1, 9)]
    v = step("create-views", views, (init,), (a.session, *a.views), "doom-view-create", D.exists_check(a.views))
    ops, allocated = [], min(D.SNAPSHOT_BYTES, D.CHILD_GROW_BYTES)
    while allocated < D.SNAPSHOT_BYTES:
        allocated = min(allocated + D.CHILD_GROW_BYTES, D.SNAPSHOT_BYTES)
        ops.append(D.grow_view(program, payer, a, 0))
    chain("snapshot-view-grow", ops, v, (a.views[0],), "doom-view-grow", per_tx=30)
    # Lanes: created one after another (each writes the session), grown in parallel.
    prev = v
    for lane in P.lanes_for(program, a, c["lanes"]):
        prev = step(f"lane-create-{lane.index}", [P.lane_create(program, payer, authority, a, lane)], (prev,),
                    (a.session, lane.record, lane.workspace, lane.scratch), "doom-lane-create",
                    D.exists_check((lane.record,)))
        ops = []
        for target, length in ((lane.workspace, D.VIEW_WORKSPACE_BYTES), (lane.scratch, D.FRAME_BYTES)):
            allocated = min(length, D.CHILD_GROW_BYTES)
            while allocated < length:
                allocated = min(allocated + D.CHILD_GROW_BYTES, length)
                ops.append(P.lane_grow(program, payer, a, lane, target))
        chain(f"lane-grow-{lane.index}", ops, prev, (lane.workspace, lane.scratch), "doom-lane-grow", per_tx=10)
    dest = {str(program), str(D.RESOURCE_KEY), str(a.session), str(a.stream), *(str(x) for x in a.states),
            str(a.resource), *(str(x) for x in a.views),
            *(str(x) for l in P.lanes_for(program, a, c["lanes"]) for x in (l.record, l.workspace, l.scratch))}
    return D.TransactionPlan(genesis_hash=c["expected_genesis"], program_id=str(program),
                             destination_accounts=tuple(sorted(dest)), signer_public_key=str(payer),
                             steps=tuple(steps))


async def provision(args) -> None:
    c = D.load_config(args.run_dir)
    if args.new_session:
        c["session_id"] = secrets.randbits(63)
        (args.run_dir / "driver-config.json").write_text(json.dumps(c, sort_keys=True, indent=2) + "\n")
    _patch(c)
    plan = provision_plan(c, Path(args.resource).read_bytes())
    print(json.dumps({"provision_steps": len(plan.steps)}), flush=True)
    await D.run_plan(args, plan)


async def campaign(args) -> None:
    c = D.load_config(args.run_dir)
    _patch(c)
    payers = [Pubkey.from_string(c["pubkeys"][f"lane-payer-{k}"]) for k in range(args.lanes)]
    plan, a, commits = P.lanes_campaign_plan(c, args.frames, args.start_cursor, args.lanes, payers)
    # The driver's per-commit receipt hook fetches every transaction's
    # metadata inside the sequencer loop (~4-5 s per frame); throughput runs
    # skip it and read frame times from the journal (--receipts restores it).
    await D.run_plan(args, plan, capture_frames=commits if args.receipts else None, addresses=a)


async def abort(args) -> None:
    """Return a lane cut off mid-publication (a capped campaign) to idle."""
    c = D.load_config(args.run_dir)
    _patch(c)
    program = Pubkey.from_string(c["program_id"])
    payer = Pubkey.from_string(c["pubkeys"]["payer"])
    authority = Pubkey.from_string(c["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, c["session_id"])
    lane = P.lanes_for(program, a, c["lanes"])[args.lane]
    ix = P._op(program, P.OP_ABORT, lane.index, D.u32(args.cursor),
               [P._m(authority, True), P._m(a.session, writable=True), P._m(lane.record, writable=True)])
    step = D.make_step(program=program, payer=payer, instructions=[ix], step_id=f"abort-{lane.index}-{args.cursor}",
                       dependencies=(), locks=(a.session, lane.record), compute_class="doom-lane-abort")
    plan = D.TransactionPlan(genesis_hash=c["expected_genesis"], program_id=str(program),
                             destination_accounts=(str(program), str(a.session), str(lane.record)),
                             signer_public_key=str(payer), steps=(step,))
    await D.run_plan(args, plan)


async def advance_to(args) -> None:
    """Advance a cut-off session to the next frame boundary (inputs already written)."""
    c = D.load_config(args.run_dir)
    _patch(c)
    program = Pubkey.from_string(c["program_id"])
    payer = Pubkey.from_string(c["pubkeys"]["payer"])
    authority = Pubkey.from_string(c["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, c["session_id"])
    steps = []
    for at in range(args.cursor, args.target):
        D._append_step(steps, program=program, payer=payer, instructions=[D.advance(program, authority, a, at)],
                       name=f"advance-{at:08d}", locks=(a.session, a.stream, *a.states), compute_class="doom-advance",
                       postcondition=D.cursor_check(a.session, at + 1))
    await D.run_plan(args, D.TransactionPlan(genesis_hash=c["expected_genesis"], program_id=str(program),
                                             destination_accounts=(str(program), str(a.session)),
                                             signer_public_key=str(payer), steps=tuple(steps)))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--run-dir", type=Path, required=True)
    sub = ap.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--lanes", type=int, default=3)
    sub.add_parser("fund")
    p = sub.add_parser("deploy")
    p.add_argument("--image", type=Path, required=True)
    for name in ("provision", "campaign", "abort", "advance"):
        p = sub.add_parser(name)
        p.add_argument("--node-count", type=int, default=1)
        p.add_argument("--send-rate", type=float, default=20.0)
        p.add_argument("--request-rate", type=float, default=60.0)
        p.add_argument("--max-in-flight", type=int, default=8)
        p.add_argument("--journal", default=name)
    sub.choices["provision"].add_argument("--resource", type=Path, required=True)
    sub.choices["provision"].add_argument("--new-session", action="store_true")
    sub.choices["advance"].add_argument("--cursor", type=int, required=True)
    sub.choices["advance"].add_argument("--target", type=int, required=True)
    sub.choices["abort"].add_argument("--lane", type=int, required=True)
    sub.choices["abort"].add_argument("--cursor", type=int, required=True)
    sub.choices["campaign"].add_argument("--lanes", type=int, required=True)
    sub.choices["campaign"].add_argument("--frames", type=int, required=True)
    sub.choices["campaign"].add_argument("--start-cursor", type=int, required=True)
    sub.choices["campaign"].add_argument("--golden", type=Path)
    sub.choices["campaign"].add_argument("--receipts", action="store_true")
    args = ap.parse_args()
    if args.command in ("provision", "campaign", "abort", "advance"):
        D.run_async({"provision": provision, "campaign": campaign, "abort": abort,
                     "advance": advance_to}[args.command](args))
    else:
        {"prepare": prepare, "fund": fund, "deploy": deploy}[args.command](args)


if __name__ == "__main__":
    main()
