"""Doom frames over DCG render lanes (lanes slice 3).

A frame on lane `k = N mod L`:
- serial on the session: its input batch and eight advances, then one capture
  transaction (begin, the 2 MiB context copies with the snapshot, end);
- on the lane only: the snapshot phases and the eight strip renders, paid by
  the lane's own fee payer so they never write-lock what ADVANCE writes;
- the commit, in frame order (DCG refuses an older frame with 2341).

The next frame's advances wait only for this frame's capture, so lane renders
overlap the session's progress. Design: DCG docs/design/stateful-session-lanes-v1.md.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from solders.instruction import AccountMeta, Instruction
from solders.pubkey import Pubkey

import testnet_driver as D

OP_CREATE, OP_GROW, OP_CAPTURE_BEGIN, OP_CAPTURE_RUN, OP_CAPTURE_END, OP_RUN, OP_COMMIT, OP_ABORT = range(4, 12)
LANE_SEED = b"dcg-lane-v3"
WORKSPACE_ROLE_BASE, SCRATCH_ROLE_BASE = 0xE0, 0xE8
ENGINE_CONTEXT_BYTES = 9_734_160
SNAPSHOT_CACHE_BYTES = 640 * 1024
LANE_CAPTURE_BYTES = ENGINE_CONTEXT_BYTES + SNAPSHOT_CACHE_BYTES
LANE_CAPTURE_PHASE_BYTES = 2 * 1024 * 1024
# Snapshot phases are byte copies (about 16k CU per 13); 64 is the program's cap.
SNAPSHOT_PHASES_PER_IX = 64


@dataclass(frozen=True)
class Lane:
    index: int
    record: Pubkey
    workspace: Pubkey
    scratch: Pubkey


def lanes_for(program: Pubkey, a: D.Addresses, count: int) -> tuple[Lane, ...]:
    s = bytes(a.session)
    return tuple(Lane(k, D._pda(program, LANE_SEED, s, bytes([k])),
                      D._pda(program, b"dcg-view-v3", s, bytes([WORKSPACE_ROLE_BASE + k])),
                      D._pda(program, b"dcg-view-v3", s, bytes([SCRATCH_ROLE_BASE + k])))
                 for k in range(count))


def _op(program: Pubkey, op: int, k: int, rest: bytes, accounts: Sequence[AccountMeta]) -> Instruction:
    return Instruction(program, bytes([236, D.WIRE_VERSION, op, k]) + rest, list(accounts))


def _m(key: Pubkey, signer: bool = False, writable: bool = False) -> AccountMeta:
    return AccountMeta(key, signer, writable)


def lane_create(program: Pubkey, payer: Pubkey, authority: Pubkey, a: D.Addresses, lane: Lane) -> Instruction:
    rest = D.u32(D.VIEW_WORKSPACE_BYTES) + D.u32(D.FRAME_BYTES)
    return _op(program, OP_CREATE, lane.index, rest, [
        _m(payer, True, True), _m(authority, True), _m(a.session, writable=True), _m(lane.record, writable=True),
        _m(lane.workspace, writable=True), _m(lane.scratch, writable=True), _m(D.SYSTEM_PROGRAM)])


def lane_grow(program: Pubkey, payer: Pubkey, a: D.Addresses, lane: Lane, target: Pubkey) -> Instruction:
    return _op(program, OP_GROW, lane.index, b"", [
        _m(payer, True, True), _m(a.session), _m(lane.record), _m(target, writable=True), _m(D.SYSTEM_PROGRAM)])


def capture_instructions(program: Pubkey, authority: Pubkey, a: D.Addresses, lane: Lane, cursor: int) -> list[Instruction]:
    """Begin, every capture call, and end: one atomic transaction (~350k CU measured)."""
    ceiling = D.u32(D.KERNEL_COMPUTE_CEILING)
    out = [_op(program, OP_CAPTURE_BEGIN, lane.index, D.u32(cursor) + ceiling,
               [_m(authority, True), _m(a.session, writable=True), _m(lane.record, writable=True),
                *(_m(v) for v in a.views)])]
    at = 0
    while at < LANE_CAPTURE_BYTES:
        out.append(_op(program, OP_CAPTURE_RUN, lane.index, D.u32(cursor) + D.u32(at) + ceiling, [
            _m(a.states[0]), _m(authority, True), _m(a.session), _m(a.resource), _m(a.states[1]), _m(a.states[2]),
            _m(lane.record, writable=True), _m(lane.workspace, writable=True)]))
        at = min(at + LANE_CAPTURE_PHASE_BYTES, LANE_CAPTURE_BYTES)
    out.append(_op(program, OP_CAPTURE_END, lane.index, D.u32(cursor),
                   [_m(authority, True), _m(a.session, writable=True), _m(lane.record, writable=True)]))
    return out


def lane_run(program: Pubkey, authority: Pubkey, a: D.Addresses, lane: Lane, cursor: int, at: int, phases: int) -> Instruction:
    """The lane workspace is first: Doom renders at its fixed address."""
    return _op(program, OP_RUN, lane.index,
               D.u32(cursor) + D.u32(at) + D.u32(D.KERNEL_COMPUTE_CEILING) + bytes([phases]), [
                   _m(lane.workspace, writable=True), _m(authority, True), _m(lane.record, writable=True),
                   _m(lane.scratch, writable=True), _m(a.resource)])


def lane_commit(program: Pubkey, authority: Pubkey, a: D.Addresses, lane: Lane, cursor: int) -> Instruction:
    return _op(program, OP_COMMIT, lane.index, D.u32(cursor), [
        _m(authority, True), _m(a.session), _m(lane.record, writable=True), _m(lane.scratch),
        *(_m(v, writable=True) for v in a.views)])


def render_phases() -> list[tuple[int, int]]:
    """(byte cursor, phases) per render instruction: snapshot copies grouped,
    then one strip per instruction."""
    out, at = [], 0
    boundary = D.SNAPSHOT_BOUNDARY
    while at < boundary:
        n = min(SNAPSHOT_PHASES_PER_IX, (boundary - at) // D.VIEW_PHASE_BYTES)
        out.append((at, n))
        at += n * D.VIEW_PHASE_BYTES
    while at < D.FRAME_BYTES:
        out.append((at, 1))
        at += D.VIEW_PHASE_BYTES
    return out


def lanes_campaign_plan(config: dict[str, Any], frames: int, start_cursor: int, lane_count: int,
                        lane_payers: Sequence[Pubkey]) -> tuple[D.TransactionPlan, D.Addresses, dict[str, int]]:
    if frames <= 0 or start_cursor % 8 or not 1 <= lane_count <= 4 or len(lane_payers) != lane_count:
        raise ValueError("frames, start cursor (multiple of 8), 1-4 lanes, one payer per lane")
    program = Pubkey.from_string(config["program_id"])
    payer = Pubkey.from_string(config["pubkeys"]["payer"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = D.Addresses.derive(program, authority, config["session_id"])
    lanes = lanes_for(program, a, lane_count)
    commands = Path(config["commands_path"]).read_bytes()
    count = len(commands) // 4
    steps: list[D.TransactionStep] = []
    commit_to_cursor: dict[str, int] = {}
    serial_tail: str | None = None           # last session step
    lane_tail: dict[int, str | None] = {k: None for k in range(lane_count)}  # last commit per lane
    last_commit: str | None = None

    def step(name, ixs, deps, locks, cls, fee_payer, post=None):
        steps.append(D.make_step(program=program, payer=fee_payer, instructions=ixs, step_id=name,
                                 dependencies=tuple(d for d in deps if d), locks=locks, compute_class=cls,
                                 postcondition=post))
        return name

    for frame in range(frames):
        cursor = start_cursor + frame * 8
        target = cursor + 8
        lane = lanes[frame % lane_count]
        prefix = f"lane{lane.index}-frame-{frame:04d}"
        inputs = [D.write_input(program, authority, a, seq, commands[(seq % count) * 4:(seq % count + 1) * 4])
                  for seq in range(cursor + 1, cursor + 9)]
        serial_tail = step(f"{prefix}-input", inputs, (serial_tail,), (a.session, a.stream), "doom-input-batch", payer)
        for tic in range(8):
            at = cursor + tic
            serial_tail = step(f"{prefix}-advance-{at:08d}", [D.advance(program, authority, a, at)], (serial_tail,),
                               (a.session, a.stream, *a.states), "doom-advance", payer,
                               D.cursor_check(a.session, at + 1))
        # The lane must have committed (or never used) its previous frame.
        serial_tail = step(f"{prefix}-capture-{target:08d}", capture_instructions(program, authority, a, lane, target),
                           (serial_tail, lane_tail[lane.index]), (a.session, lane.record, lane.workspace),
                           "doom-lane-capture", payer)
        prev = serial_tail
        for at, n in render_phases():
            prev = step(f"{prefix}-render-{at:08d}", [lane_run(program, authority, a, lane, target, at, n)], (prev,),
                        (lane.record, lane.workspace, lane.scratch, lane_payers[lane.index]),
                        "doom-lane-render", lane_payers[lane.index])
        commit = step(f"{prefix}-commit-{target:08d}", [lane_commit(program, authority, a, lane, target)],
                      (prev, last_commit), (lane.record, *a.views, lane_payers[lane.index]),
                      "doom-lane-commit", lane_payers[lane.index])
        commit_to_cursor[commit] = target
        lane_tail[lane.index] = last_commit = commit
    signers = tuple(sorted({str(payer), *(str(p) for p in lane_payers)}))
    plan = D.TransactionPlan(
        genesis_hash=config.get("observed_genesis") or config.get("expected_genesis") or "local-genesis-pending",
        program_id=str(program),
        destination_accounts=tuple(sorted({str(program), str(a.session), str(a.stream), *(str(x) for x in a.states),
                                           str(a.resource), *(str(x) for x in a.views),
                                           *(str(x) for l in lanes for x in (l.record, l.workspace, l.scratch))})),
        signer_public_key=str(payer), steps=tuple(steps), signer_public_keys=signers)
    return plan, a, commit_to_cursor
