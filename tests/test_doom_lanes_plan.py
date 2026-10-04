"""The Doom lanes campaign plan (lanes slice 3): the dependency graph and the
write locks that let lane renders overlap the session's advances."""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DCG_PYTHON = Path(os.environ.get("DCG_PYTHON", ROOT.parent / "durable-compute-graph" / "python"))
for p in (DCG_PYTHON, ROOT / "stunts" / "doom-dcg"):
    if str(p) not in sys.path:
        sys.path.insert(0, str(p))

solders = pytest.importorskip("solders")
D = pytest.importorskip("testnet_driver")
P = pytest.importorskip("lanes_plan")

from solders.keypair import Keypair  # noqa: E402
from solders.message import Message  # noqa: E402


def config() -> dict:
    return {
        "program_id": str(Keypair.from_seed(bytes([1]) * 32).pubkey()),
        "pubkeys": {"payer": str(Keypair.from_seed(bytes([2]) * 32).pubkey()),
                    "authority": str(Keypair.from_seed(bytes([3]) * 32).pubkey())},
        "session_id": 7,
        "commands_path": str(ROOT / "stunts/doom-dcg/tests/fixtures/commands.bin"),
    }


def plan(frames=4, lanes=2):
    payers = [Keypair.from_seed(bytes([10 + k]) * 32).pubkey() for k in range(lanes)]
    p, a, commits = P.lanes_campaign_plan(config(), frames, 0, lanes, payers)
    return p, a, commits, payers


def test_renders_lock_only_their_lane_and_never_the_session_or_state():
    p, a, _, payers = plan()
    session_side = {str(a.session), str(a.stream), *(str(x) for x in a.states)}
    renders = [s for s in p.steps if s.compute_class == "doom-lane-render"]
    assert renders
    for s in renders:
        assert not session_side & set(s.write_locks), s.step_id
        assert any(str(x) in s.write_locks for x in payers), "a render is paid by its lane's payer"


def test_the_next_frame_waits_for_the_capture_not_for_renders():
    p, *_ = plan()
    by_id = {s.step_id: s for s in p.steps}
    first_input_of_frame_1 = next(s for s in p.steps if s.step_id.startswith("lane1-frame-0001-input"))
    (dep,) = first_input_of_frame_1.dependencies
    assert by_id[dep].compute_class == "doom-lane-capture"


def test_commits_are_in_frame_order_and_a_lane_is_reused_only_after_its_commit():
    p, _, commits, _ = plan(frames=5, lanes=2)
    order = list(commits)
    by_id = {s.step_id: s for s in p.steps}
    for prev, nxt in zip(order, order[1:]):
        assert prev in by_id[nxt].dependencies
    capture_frame_2 = next(s for s in p.steps if s.step_id.startswith("lane0-frame-0002-capture"))
    assert order[0] in capture_frame_2.dependencies
    assert list(commits.values()) == [8, 16, 24, 32, 40]


def test_every_step_fits_a_packet():
    p, *_ = plan(frames=2, lanes=2)
    for s in p.steps:
        lease = type("L", (), {"blockhash": "11111111111111111111111111111111"})()
        raw = s.build_message(lease)
        assert len(raw) + 1 + 2 * 64 <= D.TX_PACKET_LIMIT, (s.step_id, len(raw))


def test_render_phases_cover_the_frame_exactly():
    covered = 0
    for at, n in P.render_phases():
        assert at == covered
        covered = min(at + n * D.VIEW_PHASE_BYTES, D.FRAME_BYTES)
    assert covered == D.FRAME_BYTES
