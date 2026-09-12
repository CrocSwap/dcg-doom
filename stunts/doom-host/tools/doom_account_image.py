#!/usr/bin/env python3
"""Build and validate the host Doom account-image fixtures.

The tool deliberately keeps the shareware WAD out of the repository.  Build
accepts only the pinned v1.9 IWAD; the verifier is also useful for small
synthetic manifests used by the offline tests.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
from pathlib import Path
from typing import Any


PINNED_WAD_SHA256 = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771"
PORT_VERSION = "doom-host-0004"
MANIFEST_FORMAT = "DOOM-WAD-MANIFEST1"
CHUNK_COUNT = 5
ACCOUNT_BYTES = 1_048_576
CHUNK_HEADER_BYTES = 32
CHUNK_PAYLOAD_BYTES = ACCOUNT_BYTES - CHUNK_HEADER_BYTES
LEVEL_STATE_BYTES = 131_072
LEVEL_STATE_HEADER_BYTES = 128
LEVEL_STATE_IMAGE_OFFSET = 4_096
MOBJ_POOL_BYTES = 65_536
MOBJ_POOL_HEADER_BYTES = 4_096
MOBJ_SLOT_BYTES = 160
MOBJ_SLOTS = 384


class Refusal(ValueError):
    """An input failed a commitment-boundary check."""


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _read_exact(path: Path, expected: int) -> bytes:
    data = path.read_bytes()
    if len(data) != expected:
        raise Refusal(f"short or oversized account {path}: {len(data)} != {expected}")
    return data


def _u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def _wad_lumps(wad: bytes) -> dict[str, bytes]:
    if len(wad) < 12 or wad[:4] not in (b"IWAD", b"PWAD"):
        raise Refusal("WAD header is not IWAD/PWAD")
    count, directory = struct.unpack_from("<II", wad, 4)
    end = directory + count * 16
    if directory < 12 or end > len(wad):
        raise Refusal("WAD directory is outside the WAD image")
    lumps: dict[str, bytes] = {}
    for i in range(count):
        pos, size = struct.unpack_from("<II", wad, directory + i * 16)
        raw_name = wad[directory + i * 16 + 8 : directory + i * 16 + 16]
        name = raw_name.rstrip(b"\0").decode("ascii", "strict").upper()
        if pos > len(wad) or size > len(wad) - pos:
            raise Refusal(f"WAD lump {i} is outside the image")
        lumps[name] = wad[pos : pos + size]
    return lumps


def _chunk_header(index: int, length: int) -> bytes:
    return struct.pack(
        "<8sIIIIII",
        b"DOOM-WC1",
        1,
        CHUNK_HEADER_BYTES,
        ACCOUNT_BYTES,
        index,
        length,
        0,
    )


def _make_chunk(index: int, payload: bytes) -> bytes:
    if len(payload) > CHUNK_PAYLOAD_BYTES:
        raise Refusal("chunk payload exceeds the 1 MiB account")
    return _chunk_header(index, len(payload)) + payload + bytes(
        ACCOUNT_BYTES - CHUNK_HEADER_BYTES - len(payload)
    )


def _manifest_paths(manifest_path: Path, manifest: dict[str, Any]) -> list[Path]:
    chunks = manifest.get("chunks")
    if not isinstance(chunks, list):
        raise Refusal("manifest chunks is not a list")
    paths: list[Path] = []
    for index, chunk in enumerate(chunks):
        if not isinstance(chunk, dict) or chunk.get("index") != index:
            raise Refusal(f"chunk order/index mismatch at {index}")
        path = chunk.get("path")
        if not isinstance(path, str) or not path:
            raise Refusal(f"chunk {index} has no path")
        paths.append((manifest_path.parent / path).resolve())
    return paths


def build_manifest(wad_path: Path, output_dir: Path, manifest_path: Path | None = None) -> Path:
    wad = wad_path.read_bytes()
    wad_hash = _sha256(wad)
    if wad_hash != PINNED_WAD_SHA256:
        raise Refusal(f"refusing WAD sha256 {wad_hash}; expected {PINNED_WAD_SHA256}")
    lumps = _wad_lumps(wad)
    if "PLAYPAL" not in lumps:
        raise Refusal("WAD has no PLAYPAL lump")

    output_dir.mkdir(parents=True, exist_ok=True)
    chunks: list[dict[str, Any]] = []
    for index in range(CHUNK_COUNT):
        start = index * CHUNK_PAYLOAD_BYTES
        payload = wad[start : start + CHUNK_PAYLOAD_BYTES]
        filename = f"wad-{index:03d}.bin"
        (output_dir / filename).write_bytes(_make_chunk(index, payload))
        chunks.append(
            {
                "index": index,
                "path": filename,
                "length": len(payload),
                "sha256": _sha256(payload),
                "account_bytes": ACCOUNT_BYTES,
                "payload_offset": CHUNK_HEADER_BYTES,
            }
        )

    if manifest_path is None:
        manifest_path = output_dir / "manifest.json"
    manifest = {
        "format": MANIFEST_FORMAT,
        "port_version": PORT_VERSION,
        "wad_sha256": wad_hash,
        "wad_bytes": len(wad),
        "palette_sha256": _sha256(lumps["PLAYPAL"]),
        "chunk_count": CHUNK_COUNT,
        "chunk_account_bytes": ACCOUNT_BYTES,
        "chunk_header_bytes": CHUNK_HEADER_BYTES,
        "chunk_payload_bytes": CHUNK_PAYLOAD_BYTES,
        "chunks": chunks,
        "level_state": {
            "account_bytes": LEVEL_STATE_BYTES,
            "header_bytes": LEVEL_STATE_HEADER_BYTES,
            "image_offset": LEVEL_STATE_IMAGE_OFFSET,
        },
        "mobj_pool": {
            "account_bytes": MOBJ_POOL_BYTES,
            "slot_offset": MOBJ_POOL_HEADER_BYTES,
            "slot_bytes": MOBJ_SLOT_BYTES,
            "slot_count": MOBJ_SLOTS,
        },
    }
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n")
    return manifest_path


def verify_manifest(
    manifest_path: Path, *, require_pinned_wad: bool = False
) -> tuple[dict[str, Any], bytes]:
    try:
        manifest = json.loads(manifest_path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise Refusal(f"cannot read manifest: {exc}") from exc
    if not isinstance(manifest, dict) or manifest.get("format") != MANIFEST_FORMAT:
        raise Refusal("wrong manifest format")
    if manifest.get("port_version") != PORT_VERSION:
        raise Refusal("wrong port version")
    if manifest.get("chunk_count") != CHUNK_COUNT:
        raise Refusal("manifest must contain exactly five chunks")
    if manifest.get("chunk_account_bytes") != ACCOUNT_BYTES:
        raise Refusal("wrong chunk account size")
    if manifest.get("chunk_header_bytes") != CHUNK_HEADER_BYTES:
        raise Refusal("wrong chunk header size")
    if manifest.get("wad_bytes", -1) < 0:
        raise Refusal("missing WAD length")
    wad_hash = manifest.get("wad_sha256")
    if not isinstance(wad_hash, str) or len(wad_hash) != 64:
        raise Refusal("bad WAD sha256 field")
    if require_pinned_wad and wad_hash != PINNED_WAD_SHA256:
        raise Refusal("manifest WAD sha256 is not the pinned shareware WAD")

    paths = _manifest_paths(manifest_path, manifest)
    chunks = manifest["chunks"]
    payloads: list[bytes] = []
    for index, (chunk, path) in enumerate(zip(chunks, paths)):
        if chunk.get("length", -1) < 0 or chunk["length"] > CHUNK_PAYLOAD_BYTES:
            raise Refusal(f"bad length for chunk {index}")
        account = _read_exact(path, ACCOUNT_BYTES)
        if account[:8] != b"DOOM-WC1" or _u32(account, 8) != 1:
            raise Refusal(f"wrong chunk header for chunk {index}")
        if _u32(account, 12) != CHUNK_HEADER_BYTES or _u32(account, 16) != ACCOUNT_BYTES:
            raise Refusal(f"wrong chunk header dimensions for chunk {index}")
        if _u32(account, 20) != index or _u32(account, 24) != chunk["length"]:
            raise Refusal(f"chunk {index} order/length header mismatch")
        length = chunk["length"]
        payload = account[CHUNK_HEADER_BYTES : CHUNK_HEADER_BYTES + length]
        if _sha256(payload) != chunk.get("sha256"):
            raise Refusal(f"chunk {index} hash mismatch")
        if any(account[CHUNK_HEADER_BYTES + length :]):
            raise Refusal(f"chunk {index} has nonzero account padding")
        payloads.append(payload)

    wad = b"".join(payloads)
    if len(wad) != manifest["wad_bytes"]:
        raise Refusal("concatenated chunk length mismatch")
    if _sha256(wad) != wad_hash:
        raise Refusal("concatenated WAD hash mismatch")
    lumps = _wad_lumps(wad)
    palette_hash = manifest.get("palette_sha256")
    if "PLAYPAL" not in lumps or _sha256(lumps["PLAYPAL"]) != palette_hash:
        raise Refusal("palette hash mismatch")
    return manifest, wad


def _verify_level_state(account_path: Path) -> tuple[bytes, bytes]:
    account = _read_exact(account_path, LEVEL_STATE_BYTES)
    if account[:8] != b"DOOM-LS1" or _u32(account, 8) != 1:
        raise Refusal("wrong level_state magic/version")
    if _u32(account, 12) != LEVEL_STATE_HEADER_BYTES or _u32(account, 16) != LEVEL_STATE_BYTES:
        raise Refusal("wrong level_state header dimensions")
    offset = _u32(account, 20)
    capacity = _u32(account, 24)
    length = _u32(account, 28)
    if offset != LEVEL_STATE_IMAGE_OFFSET or capacity != LEVEL_STATE_BYTES - offset:
        raise Refusal("bad level_state image offset/capacity")
    if length > capacity or offset + length > len(account):
        raise Refusal("level_state image exceeds account bounds")
    if any(account[offset + length :]):
        raise Refusal("level_state tail is not zero")
    image = account[offset : offset + length]
    if _sha256(image) != account[64:96].hex():
        raise Refusal("level_state image hash mismatch")
    return account, image


def dump_level_state(account_path: Path, output_path: Path, expected: Path | None = None) -> None:
    _, image = _verify_level_state(account_path)
    if expected is not None and image != expected.read_bytes():
        raise Refusal("level_state image differs from expected DOOM-ST1")
    output_path.write_bytes(image)


def verify_level_state(account_path: Path, image_path: Path) -> None:
    _, image = _verify_level_state(account_path)
    expected = image_path.read_bytes()
    if image != expected:
        raise Refusal("level_state image differs from DOOM-ST1")
    if len(image) < 64 or image[:8] != b"DOOM-ST1" or _u32(image, 8) != 0:
        raise Refusal("expected file is not DOOM-ST1 v0")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("build", help="build five WAD account chunks and a manifest")
    build.add_argument("wad", type=Path)
    build.add_argument("--out-dir", type=Path, required=True)
    build.add_argument("--manifest", type=Path)
    verify = sub.add_parser("verify", help="verify ordered chunks, lengths, hashes, and WAD")
    verify.add_argument("manifest", type=Path)
    verify.add_argument("--require-pinned-wad", action="store_true")
    dump = sub.add_parser("dump-level-state", help="extract the DOOM-ST1 image from level_state")
    dump.add_argument("account", type=Path)
    dump.add_argument("--out", type=Path, required=True)
    dump.add_argument("--expect", type=Path)
    level = sub.add_parser("verify-level-state", help="compare level_state with a DOOM-ST1 file")
    level.add_argument("account", type=Path)
    level.add_argument("image", type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "build":
            path = build_manifest(args.wad, args.out_dir, args.manifest)
            print(path)
        elif args.command == "verify":
            manifest, wad = verify_manifest(args.manifest, require_pinned_wad=args.require_pinned_wad)
            print(f"ok chunks={len(manifest['chunks'])} wad_bytes={len(wad)} wad_sha256={_sha256(wad)}")
        elif args.command == "dump-level-state":
            dump_level_state(args.account, args.out, args.expect)
            print(f"ok image_bytes={args.out.stat().st_size}")
        else:
            verify_level_state(args.account, args.image)
            print("ok level_state matches DOOM-ST1")
    except (OSError, Refusal, struct.error) as exc:
        print(f"refused: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
