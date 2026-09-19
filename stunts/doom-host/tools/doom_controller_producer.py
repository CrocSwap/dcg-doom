#!/usr/bin/env python3
"""Emit one bounded DOOM-CI2 controller account from a retained capture.

The producer reads the real retained DEMO1 lump from the account-image
capture. A signed forward-move delta is an explicit controlled-input variant;
the emitted account is never hand-authored by the gate.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
ACCOUNT_IMAGE = Path(__file__).with_name("doom_account_image.py")
spec = importlib.util.spec_from_file_location("doom_account_image", ACCOUNT_IMAGE)
if spec is None or spec.loader is None:  # pragma: no cover
    raise RuntimeError(f"cannot load {ACCOUNT_IMAGE}")
account_image = importlib.util.module_from_spec(spec)
spec.loader.exec_module(account_image)


MAGIC = b"DOOM-CI2"
ACCOUNT_BYTES = 64


class ProducerRefusal(ValueError):
    pass


def controller_account(gametic: int, command: bytes) -> bytes:
    if not 0 <= gametic <= 0xFFFFFFFF:
        raise ProducerRefusal("gametic is outside u32")
    if len(command) != 4:
        raise ProducerRefusal("controller command must contain four LMP bytes")
    out = bytearray(ACCOUNT_BYTES)
    out[0:8] = MAGIC
    out[8:12] = (1).to_bytes(4, "little")
    out[12:16] = (28).to_bytes(4, "little")
    out[16:20] = ACCOUNT_BYTES.to_bytes(4, "little")
    out[20:24] = gametic.to_bytes(4, "little")
    out[24:28] = command
    return bytes(out)


def retained_command(capture: Path, tic: int) -> tuple[bytes, str, str]:
    if tic < 0:
        raise ProducerRefusal("tic must be non-negative")
    manifest, wad = account_image.verify_manifest(
        capture / "account/manifest.json", require_pinned_wad=True
    )
    lumps = account_image._wad_lumps(wad)
    demo = lumps.get("DEMO1")
    if demo is None or len(demo) < 17:
        raise ProducerRefusal("retained account image has no usable DEMO1")
    if demo[2] != 1 or not 1 <= demo[3] <= 9 or demo[9:13] != b"\x01\x00\x00\x00":
        raise ProducerRefusal("producer is bounded to the retained one-player E1 demo")
    offset = 13 + tic * 4
    if offset + 4 > len(demo) or demo[offset] == 0x80:
        raise ProducerRefusal(f"DEMO1 has no command at tic {tic}")
    return demo[offset : offset + 4], hashlib.sha256(demo).hexdigest(), manifest["wad_sha256"]


def produce(capture: Path, output: Path, tic: int, forwardmove_delta: int) -> dict[str, object]:
    if output.exists():
        raise ProducerRefusal(f"refusing to overwrite existing output: {output}")
    if not -128 <= forwardmove_delta <= 127:
        raise ProducerRefusal("forwardmove delta is outside the signed byte range")
    source_command, demo_sha256, wad_sha256 = retained_command(capture, tic)
    signed_forward = source_command[0] if source_command[0] < 128 else source_command[0] - 256
    changed_forward = signed_forward + forwardmove_delta
    if not -128 <= changed_forward <= 127:
        raise ProducerRefusal("derived forwardmove is outside the signed byte range")
    command = bytes((changed_forward & 0xFF, *source_command[1:]))
    account = controller_account(tic, command)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(account)
    return {
        "producer": "doom-controller-producer/1",
        "format": "DOOM-CI2",
        "format_version": 1,
        "capture_root": str(capture.resolve()),
        "wad_sha256": wad_sha256,
        "demo1_sha256": demo_sha256,
        "tic": tic,
        "source_command_hex": source_command.hex(),
        "forwardmove_delta": forwardmove_delta,
        "emitted_command_hex": command.hex(),
        "account_bytes": len(account),
        "account_sha256": hashlib.sha256(account).hexdigest(),
        "output": str(output.resolve()),
    }


def parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--tic", type=int, default=0)
    parser.add_argument("--forwardmove-delta", type=int, default=0)
    return parser


def main() -> int:
    args = parser().parse_args()
    try:
        result = produce(args.capture.resolve(), args.out.resolve(), args.tic, args.forwardmove_delta)
    except (ProducerRefusal, OSError, ValueError, json.JSONDecodeError) as exc:
        print(json.dumps({"status": "refused", "error": str(exc)}))
        return 2
    print(json.dumps({"status": "produced", **result}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
