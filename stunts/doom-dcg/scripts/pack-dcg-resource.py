#!/usr/bin/env python3
"""Pack the retained Doom RI2/WAD inputs as one authenticated DCG v2 resource."""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import struct

MAGIC = b"DOOMDGR1"
HEADER_BYTES = 32
RI2_BYTES = 261_056
WAD_BYTES = 4_196_020
WAD_ACCOUNT_BYTES = 1_048_576
WAD_HEADER_BYTES = 32
EXPECTED_WAD_SHA256 = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771"


def u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def pack(ri2_path: pathlib.Path, wad_dir: pathlib.Path) -> bytes:
    ri2 = ri2_path.read_bytes()
    if (
        len(ri2) != RI2_BYTES
        or ri2[:8] != b"DOOM-RI2"
        or u32(ri2, 8) != 4
        or u32(ri2, 16) != RI2_BYTES
    ):
        raise ValueError("RI2 input does not match the pinned E1M1 v4 image")

    wad_parts = []
    for ordinal in range(5):
        account = (wad_dir / f"wad-{ordinal:03}.bin").read_bytes()
        if (
            len(account) != WAD_ACCOUNT_BYTES
            or account[:8] != b"DOOM-WC1"
            or u32(account, 8) != 1
            or u32(account, 12) != WAD_HEADER_BYTES
            or u32(account, 16) != WAD_ACCOUNT_BYTES
            or u32(account, 20) != ordinal
        ):
            raise ValueError(f"invalid WAD account {ordinal}")
        payload_bytes = u32(account, 24)
        if payload_bytes > WAD_ACCOUNT_BYTES - WAD_HEADER_BYTES:
            raise ValueError(f"WAD account {ordinal} length exceeds its account")
        payload = account[WAD_HEADER_BYTES : WAD_HEADER_BYTES + payload_bytes]
        if any(account[WAD_HEADER_BYTES + payload_bytes :]):
            raise ValueError(f"WAD account {ordinal} has nonzero padding")
        wad_parts.append(payload)

    wad = b"".join(wad_parts)
    digest = hashlib.sha256(wad).digest()
    if len(wad) != WAD_BYTES or digest.hex() != EXPECTED_WAD_SHA256:
        raise ValueError("WAD payload does not match the retained golden")
    if ri2[72:104] != digest:
        raise ValueError("RI2 image does not commit to the packed WAD payload")

    total = HEADER_BYTES + len(ri2) + len(wad)
    header = MAGIC + struct.pack(
        "<6I", 1, HEADER_BYTES, total, len(ri2), len(wad), 0
    )
    return header + ri2 + wad


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("ri2", type=pathlib.Path)
    parser.add_argument("wad_account_dir", type=pathlib.Path)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    data = pack(args.ri2, args.wad_account_dir)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(data)
    print(
        f"bytes={len(data)} sha256={hashlib.sha256(data).hexdigest()} "
        f"path={args.output}"
    )


if __name__ == "__main__":
    main()
