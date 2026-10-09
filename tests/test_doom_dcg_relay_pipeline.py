"""Offline tests for the DCG Doom relay pipeline's public-mode pieces:
pause, run-wide spend, and the death event (stunts/doom-dcg/relay_pipeline.py)."""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "stunts" / "doom-dcg"))
relay_pipeline = pytest.importorskip("relay_pipeline")
R = relay_pipeline


def snapshot_view(player_state: int, rsn_header: int = 192) -> bytes:
    view = bytearray(R.VIEW_HEADER + rsn_header + 128)
    rsn = R.VIEW_HEADER
    view[rsn:rsn + 8] = R.RSN_MAGIC
    view[rsn + 12:rsn + 16] = rsn_header.to_bytes(4, "little")
    r3 = rsn + rsn_header
    view[r3:r3 + 7] = R.R3_MAGIC
    view[r3 + R.R3_PLAYERSTATE:r3 + R.R3_PLAYERSTATE + 4] = player_state.to_bytes(4, "little")
    return bytes(view)


def bare_pipeline(cap_fogo: float = 1.0, baseline: int | None = None):
    pipe = object.__new__(R.Pipeline)
    pipe.args = argparse.Namespace(spend_cap_fogo=cap_fogo, payer_baseline_lamports=baseline)
    pipe.keys = R.Keys()
    pipe.resumed = asyncio.Event()
    pipe.stopping = False
    pipe.cursor = 0
    pipe.gametic = None
    pipe.spend_lamports = 0
    pipe.baseline_lamports = baseline
    pipe.player_state = None
    pipe.deaths = 0
    return pipe


def test_snapshot_player_state_reads_the_r3_block():
    assert R.snapshot_player_state(snapshot_view(0)) == 0
    assert R.snapshot_player_state(snapshot_view(R.PST_DEAD)) == R.PST_DEAD
    assert R.snapshot_player_state(snapshot_view(R.PST_DEAD, rsn_header=256)) == R.PST_DEAD


@pytest.mark.parametrize("damage", ["rsn", "r3", "short"])
def test_snapshot_player_state_refuses_other_layouts(damage):
    view = bytearray(snapshot_view(R.PST_DEAD))
    if damage == "rsn":
        view[R.VIEW_HEADER] ^= 0xFF
    elif damage == "r3":
        view[R.VIEW_HEADER + 192] ^= 0xFF
    else:
        view = view[:R.VIEW_HEADER + 192 + 20]
    assert R.snapshot_player_state(bytes(view)) is None


def test_death_is_emitted_once_per_transition(monkeypatch):
    events = []
    monkeypatch.setattr(R, "emit", events.append)
    pipe = bare_pipeline()
    pipe.note_player(R.PST_DEAD, 8)  # first sight: no transition known
    pipe.note_player(0, 16)
    pipe.note_player(R.PST_DEAD, 24)
    pipe.note_player(R.PST_DEAD, 32)
    pipe.note_player(None, 40)  # an unreadable snapshot changes nothing
    pipe.note_player(0, 48)
    pipe.note_player(R.PST_DEAD, 56)
    assert [e["cursor"] for e in events] == [24, 56]
    assert all(e["event"] == "death" for e in events)
    assert pipe.deaths == 2


def test_spend_is_measured_from_the_run_baseline_across_payers():
    lamports = R.LAMPORTS
    pipe = bare_pipeline(cap_fogo=1.0, baseline=10 * lamports)
    pipe.note_spend({"payer": 6 * lamports, "lane-0": 3 * lamports + lamports // 2})
    assert pipe.spend_lamports == lamports // 2
    assert not pipe.cap_reached()
    pipe.note_spend({"payer": 6 * lamports, "lane-0": 3 * lamports})
    assert pipe.cap_reached()  # 1.0 of a 1.0 cap
    pipe.note_spend({"payer": 20 * lamports})  # funded above the baseline
    assert pipe.spend_lamports == 0


def test_pause_clears_resume_and_a_key_press_resumes():
    pipe = bare_pipeline()

    async def run():
        assert await pipe.handle({"op": "resume"}) == {"resumed": True}
        assert pipe.resumed.is_set()
        assert await pipe.handle({"op": "pause"}) == {"paused": True}
        assert not pipe.resumed.is_set()
        await pipe.handle({"op": "key_state", "key_mask": 1})
        assert pipe.resumed.is_set()
        await pipe.handle({"op": "stop"})
        assert await pipe.handle({"op": "pause"}) == {"paused": True}
        assert pipe.resumed.is_set(), "a stopping pipeline is never re-paused"

    asyncio.run(run())
