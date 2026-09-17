"""Host-only reference for the Doom SBF step-5 wire and state controls.

This module deliberately accepts synthetic WAD bytes so focused tests can run
without a checked-in IWAD.  Its constants and byte operations mirror
``stunts/doom-sbf/src/step5.rs``.  It is a reference/control harness, not a
replacement for the later SBF and validator gate.
"""

from __future__ import annotations

import hashlib
import struct
from dataclasses import dataclass


INSTRUCTION_MAGIC = b"DOOM-INS"
PROTOCOL_VERSION = 1
OP_INIT = 1
OP_LOAD_LEVEL = 2
OP_TIC = 3
INIT_BYTES = 128
LOAD_LEVEL_BYTES = 56
TIC_BYTES = 92

CONFIG_BYTES = 4_096
LEVEL_STATE_BYTES = 131_072
MOBJ_POOL_BYTES = 65_536
WAD_CHUNKS = 5
WAD_ACCOUNT_BYTES = 1_048_576
WAD_HEADER_BYTES = 32
WAD_PAYLOAD_BYTES = WAD_ACCOUNT_BYTES - WAD_HEADER_BYTES
LEVEL_HEADER_BYTES = 128
LEVEL_IMAGE_OFFSET = 4_096
LEVEL_IMAGE_BYTES = 256
MOBJ_HEADER_BYTES = 4_096
MOBJ_SLOT_BYTES = 160
MOBJ_SLOTS = 384
PORT_VERSION = b"doom-host-0004"
BUTTON_ALLOCATE_PROBE = 0x80

CONFIG_MAGIC = b"DOOM-CFG"
LEVEL_MAGIC = b"DOOM-LS1"
POOL_MAGIC = b"DOOM-MP1"
STATE_MAGIC = b"DOOM-ST1"


class Refusal(ValueError):
    """A malformed, stale, or exhausted step-5 transition."""


class StaleHash(Refusal):
    pass


class PoolExhausted(Refusal):
    pass


@dataclass(frozen=True)
class WadSummary:
    lengths: tuple[int, ...]
    total: int
    root: bytes
    palette: bytes
    map_metadata: bytes


def sha256(*parts: bytes) -> bytes:
    h = hashlib.sha256()
    for part in parts:
        h.update(part)
    return h.digest()


def _u32(data: bytes | bytearray, offset: int) -> int:
    if offset < 0 or offset + 4 > len(data):
        raise Refusal("u32 outside bounds")
    return struct.unpack_from("<I", data, offset)[0]


def _put_u32(data: bytearray, offset: int, value: int) -> None:
    if offset < 0 or offset + 4 > len(data):
        raise Refusal("u32 outside bounds")
    struct.pack_into("<I", data, offset, value & 0xFFFFFFFF)


def _i32(data: bytes | bytearray, offset: int) -> int:
    value = _u32(data, offset)
    return value - (1 << 32) if value & (1 << 31) else value


def _put_i32(data: bytearray, offset: int, value: int) -> None:
    _put_u32(data, offset, value)


def _chunk(index: int, payload: bytes) -> bytes:
    if len(payload) > WAD_PAYLOAD_BYTES:
        raise Refusal("chunk payload exceeds account")
    header = struct.pack(
        "<8sIIIIII",
        b"DOOM-WC1",
        1,
        WAD_HEADER_BYTES,
        WAD_ACCOUNT_BYTES,
        index,
        len(payload),
        0,
    )
    return header + payload + bytes(WAD_ACCOUNT_BYTES - len(header) - len(payload))


def make_wad_chunks(wad: bytes) -> list[bytes]:
    return [
        _chunk(index, wad[index * WAD_PAYLOAD_BYTES : (index + 1) * WAD_PAYLOAD_BYTES])
        for index in range(WAD_CHUNKS)
    ]


def synthetic_wad() -> bytes:
    """Return a small valid IWAD with the canonical E1M1 lump set."""
    names = [
        b"PLAYPAL\0",
        b"E1M1\0\0\0\0",
        b"THINGS\0\0",
        b"LINEDEFS",
        b"SIDEDEFS",
        b"VERTEXES",
        b"SEGS\0\0\0\0",
        b"SSECTORS",
        b"NODES\0\0\0",
        b"SECTORS\0",
        b"REJECT\0\0",
        b"BLOCKMAP",
    ]
    payload = bytes((index * 17 + 3) & 0xFF for index in range(768))
    data = bytearray(payload)
    entries: list[bytes] = []
    playpal_pos = 12
    for index, name in enumerate(names):
        if index == 0:
            pos, length = playpal_pos, len(payload)
        else:
            pos, length = 12, 0
        entries.append(struct.pack("<II8s", pos, length, name))
    directory = 12 + len(data)
    return struct.pack("<4sII", b"IWAD", len(entries), directory) + data + b"".join(entries)


def _wad_bytes(chunks: list[bytes]) -> bytes:
    if len(chunks) != WAD_CHUNKS:
        raise Refusal("expected five WAD chunks")
    payloads: list[bytes] = []
    for index, chunk in enumerate(chunks):
        if len(chunk) != WAD_ACCOUNT_BYTES:
            raise Refusal("wrong WAD account size")
        if chunk[:8] != b"DOOM-WC1" or _u32(chunk, 8) != 1:
            raise Refusal("wrong WAD chunk grammar")
        if _u32(chunk, 12) != WAD_HEADER_BYTES or _u32(chunk, 16) != WAD_ACCOUNT_BYTES:
            raise Refusal("wrong WAD chunk dimensions")
        if _u32(chunk, 20) != index or _u32(chunk, 28) != 0:
            raise Refusal("WAD chunk order mismatch")
        length = _u32(chunk, 24)
        if length > WAD_PAYLOAD_BYTES or any(chunk[WAD_HEADER_BYTES + length :]):
            raise Refusal("WAD chunk bounds/padding mismatch")
        payload = chunk[WAD_HEADER_BYTES : WAD_HEADER_BYTES + length]
        payloads.append(payload)
    return b"".join(payloads)


def _lumps(wad: bytes) -> list[tuple[bytes, int, int]]:
    if len(wad) < 12 or wad[:4] not in (b"IWAD", b"PWAD"):
        raise Refusal("WAD header is not IWAD/PWAD")
    count, directory = struct.unpack_from("<II", wad, 4)
    if directory < 12 or directory > len(wad) or count > (len(wad) - directory) // 16:
        raise Refusal("WAD directory outside image")
    result = []
    for index in range(count):
        pos, length, name = struct.unpack_from("<II8s", wad, directory + 16 * index)
        if pos > len(wad) or length > len(wad) - pos:
            raise Refusal("WAD lump outside image")
        result.append((name, pos, length))
    return result


def summarize_wad(chunks: list[bytes]) -> WadSummary:
    wad = _wad_bytes(chunks)
    lumps = _lumps(wad)
    root = sha256(wad)
    palette_row = next((row for row in lumps if row[0] == b"PLAYPAL\0"), None)
    map_row = next((index, row) for index, row in enumerate(lumps) if row[0] == b"E1M1\0\0\0\0")
    if palette_row is None:
        raise Refusal("PLAYPAL missing")
    palette = sha256(wad[palette_row[1] : palette_row[1] + palette_row[2]])
    required = (
        b"BLOCKMAP",
        b"VERTEXES",
        b"SECTORS\0",
        b"SIDEDEFS",
        b"LINEDEFS",
        b"SSECTORS",
        b"NODES\0\0\0",
        b"SEGS\0\0\0\0",
        b"REJECT\0\0",
        b"THINGS\0\0",
    )
    metadata = bytearray()
    for wanted in required:
        row = next((entry for entry in lumps[map_row[0] + 1 :] if entry[0] == wanted), None)
        if row is None:
            raise Refusal(f"missing E1M1 lump {wanted!r}")
        metadata += struct.pack("<II", row[1], row[2])
    lengths = tuple(_u32(chunk, 24) for chunk in chunks)
    return WadSummary(lengths, len(wad), root, palette, bytes(metadata))


def init_instruction(wad: bytes, *, port_version: bytes = PORT_VERSION) -> bytes:
    chunks = make_wad_chunks(wad)
    summary = summarize_wad(chunks)
    if port_version != PORT_VERSION:
        raise Refusal("unsupported port version")
    return struct.pack(
        "<8s10I32s32s16s",
        INSTRUCTION_MAGIC,
        PROTOCOL_VERSION,
        OP_INIT,
        summary.total,
        WAD_CHUNKS,
        WAD_ACCOUNT_BYTES,
        LEVEL_STATE_BYTES,
        MOBJ_POOL_BYTES,
        MOBJ_SLOT_BYTES,
        MOBJ_SLOTS,
        len(port_version),
        summary.root,
        summary.palette,
        port_version.ljust(16, b"\0"),
    )


def load_instruction(state_hash: bytes, *, episode: int = 1, map_number: int = 1) -> bytes:
    if len(state_hash) != 32:
        raise Refusal("state hash must be 32 bytes")
    return struct.pack(
        "<8s4I32s",
        INSTRUCTION_MAGIC,
        PROTOCOL_VERSION,
        OP_LOAD_LEVEL,
        episode,
        map_number,
        state_hash,
    )


def tic_instruction(
    number: int, before: bytes, after: bytes, command: bytes = b"\0\0\0\0"
) -> bytes:
    if len(before) != 32 or len(after) != 32 or len(command) != 4:
        raise Refusal("tic hash or command width mismatch")
    return struct.pack(
        "<8s4I32s32s4s",
        INSTRUCTION_MAGIC,
        PROTOCOL_VERSION,
        OP_TIC,
        number,
        1,
        before,
        after,
        command,
    )


def empty_accounts() -> tuple[bytearray, bytearray, bytearray]:
    return bytearray(CONFIG_BYTES), bytearray(LEVEL_STATE_BYTES), bytearray(MOBJ_POOL_BYTES)


def apply_init(config: bytearray, chunks: list[bytes], data: bytes) -> None:
    if len(data) != INIT_BYTES or data[:8] != INSTRUCTION_MAGIC:
        raise Refusal("malformed init instruction")
    if _u32(data, 8) != PROTOCOL_VERSION or _u32(data, 12) != OP_INIT:
        raise Refusal("wrong init version/opcode")
    if any(config):
        raise Refusal("config already initialized")
    summary = summarize_wad(chunks)
    if (
        _u32(data, 16) != summary.total
        or data[48:80] != summary.root
        or data[80:112] != summary.palette
        or data[112 : 112 + _u32(data, 44)] != PORT_VERSION
        or any(data[112 + _u32(data, 44) : 128])
    ):
        raise Refusal("init commitment mismatch")
    config[:8] = CONFIG_MAGIC
    values = (1, LEVEL_HEADER_BYTES, CONFIG_BYTES, len(PORT_VERSION))
    for offset, value in zip((8, 12, 16, 20), values):
        _put_u32(config, offset, value)
    config[24:56] = summary.root
    config[56:88] = summary.palette
    for offset, value in zip((88, 92, 96, 100, 104, 108), (WAD_CHUNKS, WAD_ACCOUNT_BYTES, LEVEL_STATE_BYTES, MOBJ_POOL_BYTES, MOBJ_SLOT_BYTES, MOBJ_SLOTS)):
        _put_u32(config, offset, value)


def _make_loaded(summary: WadSummary) -> tuple[bytearray, bytearray]:
    level = bytearray(LEVEL_STATE_BYTES)
    pool = bytearray(MOBJ_POOL_BYTES)
    image = bytearray(LEVEL_IMAGE_BYTES)
    image[:8] = STATE_MAGIC
    _put_u32(image, 12, 64)
    _put_u32(image, 16, LEVEL_IMAGE_BYTES)
    _put_u32(image, 28, 1)
    image[128:160] = sha256(b"DOOM-LEVEL1", summary.root, summary.map_metadata)
    level[:8] = LEVEL_MAGIC
    for offset, value in zip((8, 12, 16, 20, 24, 28, 96), (1, LEVEL_HEADER_BYTES, LEVEL_STATE_BYTES, LEVEL_IMAGE_OFFSET, LEVEL_STATE_BYTES - LEVEL_IMAGE_OFFSET, LEVEL_IMAGE_BYTES, 0)):
        _put_u32(level, offset, value)
    level[32:64] = summary.root
    level[64:96] = sha256(image)
    level[LEVEL_IMAGE_OFFSET : LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES] = image
    pool[:8] = POOL_MAGIC
    for offset, value in zip((8, 12, 16, 20, 24, 28, 32), (1, MOBJ_HEADER_BYTES, MOBJ_POOL_BYTES, MOBJ_SLOT_BYTES, MOBJ_SLOTS, 1, 1)):
        _put_u32(pool, offset, value)
    pool[36:68] = summary.root
    slot = memoryview(pool)[MOBJ_HEADER_BYTES : MOBJ_HEADER_BYTES + MOBJ_SLOT_BYTES]
    _put_u32(slot, 0, 1)
    _put_u32(slot, 4, 1)
    for offset, start in ((16, 0), (20, 4), (24, 8)):
        _put_i32(slot, offset, int.from_bytes(summary.root[start : start + 4], "little", signed=True))
    _put_u32(slot, 108, 1)
    return level, pool


def state_hash(root: bytes, level: bytes | bytearray, pool: bytes | bytearray) -> bytes:
    return sha256(b"DOOM-TIC1", struct.pack("<I", PROTOCOL_VERSION), root, level, pool)


def load_accounts(chunks: list[bytes]) -> tuple[bytearray, bytearray, bytes]:
    summary = summarize_wad(chunks)
    level, pool = _make_loaded(summary)
    return level, pool, state_hash(summary.root, level, pool)


def _fixed_mul_div(image: bytes | bytearray) -> int:
    checksum = 0
    for index in range(10_000):
        a = 0x10000 + ((index * 97 + image[20]) & 0x7FFF)
        b = 0x08000 + ((index * 53 + image[21]) & 0x3FFF)
        checksum = (checksum + ((a * b) >> 16)) & 0xFFFFFFFFFFFFFFFF
        checksum ^= (a << 16) // b
    return checksum & 0xFFFFFFFF


def tic_transition(
    root: bytes,
    level: bytearray,
    pool: bytearray,
    number: int,
    before: bytes,
    command: bytes,
    after: bytes | None,
) -> None:
    if number == 0 or _u32(level, 96) + 1 != number or len(command) != 4:
        raise Refusal("tic is not the exact next tic")
    if state_hash(root, level, pool) != before:
        raise StaleHash("stale pre-state hash")
    active = _u32(pool, 28)
    first_free = _u32(pool, 32)
    allocate = bool(command[3] & BUTTON_ALLOCATE_PROBE)
    if allocate and first_free == 0xFFFFFFFF:
        raise PoolExhausted("mobj pool exhausted")
    if allocate and first_free >= MOBJ_SLOTS:
        raise Refusal("invalid free-slot index")
    old_level = bytes(level[64:100]) + bytes(level[LEVEL_IMAGE_OFFSET : LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES])
    old_pool_header = bytes(pool[28:36])
    old_slot0 = bytes(pool[MOBJ_HEADER_BYTES : MOBJ_HEADER_BYTES + MOBJ_SLOT_BYTES])
    old_allocated = bytes(pool[MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES : MOBJ_HEADER_BYTES + (first_free + 1) * MOBJ_SLOT_BYTES]) if allocate else b""
    image = memoryview(level)[LEVEL_IMAGE_OFFSET : LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES]
    image[64:68] = command
    _put_u32(image, 20, number)
    _put_u32(image, 68, _u32(image, 68) + 1)
    slot0 = memoryview(pool)[MOBJ_HEADER_BYTES : MOBJ_HEADER_BYTES + MOBJ_SLOT_BYTES]
    _put_i32(slot0, 16, _i32(pool, MOBJ_HEADER_BYTES + 16) + int.from_bytes(command[:1], "little", signed=True) * 256)
    _put_i32(slot0, 20, _i32(pool, MOBJ_HEADER_BYTES + 20) + int.from_bytes(command[1:2], "little", signed=True) * 256)
    _put_u32(image, 72, _fixed_mul_div(image))
    level[64:96] = sha256(image)
    _put_u32(level, 96, number)
    if allocate:
        start = MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES
        _put_u32(pool, start, first_free + 1)
        _put_u32(pool, start + 4, 1)
        _put_u32(pool, 28, active + 1)
        next_free = next((index for index in range(first_free + 1, MOBJ_SLOTS) if _u32(pool, MOBJ_HEADER_BYTES + index * MOBJ_SLOT_BYTES) == 0), 0xFFFFFFFF)
        _put_u32(pool, 32, next_free)
    if after is not None and state_hash(root, level, pool) != after:
        level[64:100] = old_level[:36]
        level[LEVEL_IMAGE_OFFSET : LEVEL_IMAGE_OFFSET + LEVEL_IMAGE_BYTES] = old_level[36:]
        pool[28:36] = old_pool_header
        pool[MOBJ_HEADER_BYTES : MOBJ_HEADER_BYTES + MOBJ_SLOT_BYTES] = old_slot0
        if allocate:
            start = MOBJ_HEADER_BYTES + first_free * MOBJ_SLOT_BYTES
            pool[start : start + MOBJ_SLOT_BYTES] = old_allocated
        raise StaleHash("stale post-state hash")


def exhausted_pool(root: bytes, level: bytearray, pool: bytearray) -> None:
    for index in range(MOBJ_SLOTS):
        start = MOBJ_HEADER_BYTES + index * MOBJ_SLOT_BYTES
        pool[start : start + MOBJ_SLOT_BYTES] = bytes(MOBJ_SLOT_BYTES)
        _put_u32(pool, start, index + 1)
        _put_u32(pool, start + 4, 1)
    _put_u32(pool, 28, MOBJ_SLOTS)
    _put_u32(pool, 32, 0xFFFFFFFF)
    # Keep this helper explicit about the expected root binding; callers pass
    # the root so a future reference implementation cannot silently mix WADs.
    if pool[36:68] != root:
        raise Refusal("pool root mismatch")
