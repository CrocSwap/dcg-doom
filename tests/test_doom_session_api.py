"""Doom drivers: the DCG session API generation is chosen by driver config.
"v3" (the deployed program) keeps the old account lists and payload; "v3-r2"
(a program on DCG >= 5624a82) puts the session authority, signing, at account
2 of every child creator and may carry session features (ring, rejectable)."""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
for p in (ROOT / "stunts" / "doom-dcg",):
    if str(p) not in sys.path:
        sys.path.insert(0, str(p))

pytest.importorskip("solders")
D = pytest.importorskip("testnet_driver")
from solders.pubkey import Pubkey  # noqa: E402

PROGRAM, PAYER, AUTH = Pubkey.new_unique(), Pubkey.new_unique(), Pubkey.new_unique()


def addresses():
    return D.Addresses.derive(PROGRAM, AUTH, 7)


def test_session_api_defaults_to_the_deployed_program():
    assert D.session_api({}) == (False, 0)
    assert D.session_api({"dcg_session_api": "v3-r2", "session_features": 3}) == (True, 3)
    with pytest.raises(ValueError):
        D.session_api({"session_features": 1})  # features need v3-r2
    with pytest.raises(ValueError):
        D.session_api({"dcg_session_api": "v3-r2", "session_features": 4})
    with pytest.raises(ValueError):
        D.session_api({"dcg_session_api": "v4"})


def test_open_payload_forms():
    a = addresses()
    base = bytes(D.open_session(PROGRAM, PAYER, AUTH, a, 7).data)
    assert len(base) == 178, "unchanged for the deployed program"
    assert bytes(D.open_session(PROGRAM, PAYER, AUTH, a, 7, lanes=3).data) == base + bytes([3])
    assert bytes(D.open_session(PROGRAM, PAYER, AUTH, a, 7, features=1).data) == base + bytes([0, 1])
    assert bytes(D.open_session(PROGRAM, PAYER, AUTH, a, 7, features=3, lanes=2).data) == base + bytes([2, 3])


def test_creators_take_the_authority_only_on_v3_r2():
    a = addresses()
    for build in (lambda auth: D.create_stream(PROGRAM, PAYER, a, auth),
                  lambda auth: D.create_state(PROGRAM, PAYER, a, auth),
                  lambda auth: D.create_view(PROGRAM, PAYER, a, 0, D.SNAPSHOT_ABI, D.SNAPSHOT_BYTES, auth),
                  lambda auth: D.create_workspace(PROGRAM, PAYER, a, auth),
                  lambda auth: D.create_scratch(PROGRAM, PAYER, a, auth)):
        old, new = build(None), build(AUTH)
        assert len(new.accounts) == len(old.accounts) + 1
        meta = new.accounts[2]
        assert (meta.pubkey, meta.is_signer, meta.is_writable) == (AUTH, True, False)
        assert [m.pubkey for m in new.accounts[:2]] == [m.pubkey for m in old.accounts[:2]]
