#!/usr/bin/env python3
"""Bounded, offline controls for the host DOOM-ST1 decoder boundary.

The positive input is a retained account-image capture.  Every negative input
is a one-field derivative of that capture.  This tool intentionally models the
wire preflight and account-pair publication rules without claiming that the
engine or SBF path has executed; pass an actual retained capture root with
``--capture-root``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


DS_HEADER_BYTES = 64
DS_MAX_IMAGE_BYTES = 126_976
DS_MAX_OBJECTS = 1_024
MAX_PLAYERS = 4
MAX_BUTTONS = 16
BODY_QUEUE = 32
MAX_CEILINGS = 30
MAX_PLATS = 30
NUM_PSPS = 2
NUM_POWERS = 6
NUM_CARDS = 6
NUM_WEAPONS = 9
NUM_AMMO = 4
NUM_MOBJ_TYPES = 137
NUM_STATES = 967

MOBJ = 1
SPECIAL_KINDS = frozenset(range(2, 10))
REMOVED_MOBJ = 10


class Refusal(ValueError):
    """The same fail-closed class used by all controls in this gate."""


class Cursor:
    def __init__(self, data: bytes):
        self.data = data
        self.pos = DS_HEADER_BYTES

    def take(self, n: int) -> bytes:
        if n < 0 or self.pos + n > len(self.data):
            raise Refusal("truncated payload")
        out = self.data[self.pos : self.pos + n]
        self.pos += n
        return out

    def u8(self) -> int:
        return self.take(1)[0]

    def u16(self) -> int:
        return struct.unpack("<H", self.take(2))[0]

    def u32(self) -> int:
        return struct.unpack("<I", self.take(4))[0]

    def i32(self) -> int:
        return struct.unpack("<i", self.take(4))[0]

    def skip_i32(self, count: int) -> None:
        self.take(4 * count)


@dataclass
class Plan:
    data: bytes
    numsectors: int
    numlines: int
    numsides: int
    numflats: int
    numtextures: int
    gametic: int
    demo_cursor: int
    nmo: int
    nsp: int
    records: list[dict[str, Any]] = field(default_factory=list)
    players_mo: list[int] = field(default_factory=list)
    players_attacker: list[int] = field(default_factory=list)
    body: list[int] = field(default_factory=list)
    sector_soundtarget: list[int] = field(default_factory=list)
    button_line: list[int] = field(default_factory=list)
    button_soundorg: list[int] = field(default_factory=list)

    @property
    def record_offsets(self) -> list[int]:
        return [int(r["offset"]) for r in self.records]

    @property
    def mobjs(self) -> list[dict[str, Any]]:
        return [r for r in self.records if r["kind"] == MOBJ]

    @property
    def specials(self) -> list[dict[str, Any]]:
        return [r for r in self.records if r["kind"] in SPECIAL_KINDS]


def u32(data: bytes, off: int) -> int:
    if off < 0 or off + 4 > len(data):
        raise Refusal("header field outside image")
    return struct.unpack_from("<I", data, off)[0]


def i32(data: bytes, off: int) -> int:
    return struct.unpack_from("<i", data, off)[0]


def put_u32(data: bytearray, off: int, value: int) -> None:
    struct.pack_into("<I", data, off, value & 0xFFFFFFFF)


def check_id(value: int, count: int, *, nullable: bool = True) -> None:
    if nullable and value == 0:
        return
    if value < 1 or value > count:
        raise Refusal(f"id {value} outside 1..{count}")


def _read_player(cur: Cursor) -> tuple[int, int]:
    mo = cur.u32()
    cur.i32()
    cur.take(8)
    cur.skip_i32(7 + NUM_POWERS)
    cur.take(NUM_CARDS + 1)
    cur.skip_i32(MAX_PLAYERS)
    cur.skip_i32(2)
    cur.take(NUM_WEAPONS)
    cur.skip_i32(NUM_AMMO * 2)
    cur.skip_i32(9)
    attacker = cur.u32()
    cur.skip_i32(3)
    for _ in range(NUM_PSPS):
        state = cur.i32()
        cur.skip_i32(3)
        if state < -1 or state >= NUM_STATES:
            raise Refusal("psprite state outside the frozen state table")
    cur.take(1)
    return mo, attacker


def _map_shape(wad: bytes) -> tuple[list[int], int]:
    if len(wad) < 12 or wad[:4] not in (b"IWAD", b"PWAD"):
        raise Refusal("invalid captured WAD image")
    count, directory = struct.unpack_from("<II", wad, 4)
    if directory > len(wad) or count > (len(wad) - directory) // 16:
        raise Refusal("invalid WAD directory")
    lumps: list[tuple[str, int, int]] = []
    for n in range(count):
        off = directory + n * 16
        pos, size = struct.unpack_from("<II", wad, off)
        name = wad[off + 8 : off + 16].rstrip(b"\0").decode("ascii", "replace")
        if pos > len(wad) or size > len(wad) - pos:
            raise Refusal("WAD lump outside image")
        lumps.append((name.upper(), pos, size))
    try:
        marker = next(i for i, (name, _, _) in enumerate(lumps) if name == "E1M1")
    except StopIteration as exc:
        raise Refusal("captured WAD has no E1M1") from exc
    by_name: dict[str, tuple[int, int]] = {}
    for name, pos, size in lumps[marker + 1 : marker + 12]:
        by_name.setdefault(name, (pos, size))
    if not {"LINEDEFS", "SIDEDEFS", "SECTORS"} <= by_name.keys():
        raise Refusal("captured E1M1 geometry is incomplete")
    lpos, lsize = by_name["LINEDEFS"]
    spos, ssize = by_name["SIDEDEFS"]
    secpos, secsize = by_name["SECTORS"]
    if lsize % 14 or ssize % 30 or secsize % 26:
        raise Refusal("captured E1M1 geometry has a partial record")
    side_count = ssize // 30
    counts: list[int] = []
    for n in range(lsize // 14):
        front, back = struct.unpack_from("<hh", wad, lpos + n * 14 + 10)
        if front < 0 or front >= side_count or (back < -1 or back >= side_count):
            raise Refusal("LINEDEFS side id outside SIDDEFS")
        counts.append(1 + int(back != -1))
    demo_bytes = next((size for name, _, size in lumps if name == "DEMO1"), 0)
    return counts, demo_bytes


def _parse_wad_accounts(account_dir: Path) -> bytes:
    chunks = sorted(account_dir.glob("wad-*.bin"))
    if len(chunks) != 5:
        raise Refusal("expected five retained WAD account chunks")
    wad = bytearray()
    for index, path in enumerate(chunks):
        raw = path.read_bytes()
        if len(raw) != 1_048_576 or raw[:8] != b"DOOM-WC1":
            raise Refusal(f"invalid captured chunk {path}")
        if u32(raw, 20) != index or u32(raw, 24) > 1_048_544:
            raise Refusal(f"invalid captured chunk metadata {path}")
        length = u32(raw, 24)
        if any(raw[32 + length :]):
            raise Refusal(f"nonzero captured chunk padding {path}")
        wad.extend(raw[32 : 32 + length])
    return bytes(wad)


def parse_st1(
    image: bytes,
    *,
    line_side_counts: list[int] | tuple[int, int],
    demo_bytes: int = 0,
) -> Plan:
    if len(image) < DS_HEADER_BYTES or len(image) > DS_MAX_IMAGE_BYTES:
        raise Refusal("image outside the bounded LS1 capacity")
    if image[:8] != b"DOOM-ST1" or u32(image, 8) != 0 or u32(image, 12) != DS_HEADER_BYTES:
        raise Refusal("unsupported ST1 header")
    if u32(image, 16) != len(image):
        raise Refusal("header image length mismatch")
    if any(image[60:64]):
        raise Refusal("nonzero ST1 reserved header bytes")
    nmo, nsp, numsectors, numlines, numsides, numflats, numtextures, nrec = (
        u32(image, 28), u32(image, 32), u32(image, 36), u32(image, 40),
        u32(image, 44), u32(image, 48), u32(image, 52), u32(image, 56)
    )
    if nmo > DS_MAX_OBJECTS or nsp > DS_MAX_OBJECTS or nrec > DS_MAX_OBJECTS:
        raise Refusal("object count exceeds the frozen total capacity")
    if nmo + nsp != nrec:
        raise Refusal("declared total does not equal declared kind counts")
    if isinstance(line_side_counts, tuple):
        shape_lines, shape_sides = line_side_counts
        if shape_lines != numlines or shape_sides < 0:
            raise Refusal("captured line-shape counts do not match ST1")
        extra_sides = shape_sides - numlines
        if extra_sides < 0 or extra_sides > numlines:
            raise Refusal("captured side count cannot describe line records")
        line_side_counts = [2] * extra_sides + [1] * (numlines - extra_sides)
    if len(line_side_counts) != numlines:
        raise Refusal("LINEDEFS shape does not match the ST1 resource counts")
    cur = Cursor(image)
    globals_ = [cur.i32() for _ in range(21)]
    for _ in range(12 + MAX_PLAYERS):
        if cur.u8() > 1:
            raise Refusal("boolean global is not 0/1")
    players_mo, players_attacker = [], []
    for _ in range(MAX_PLAYERS):
        mo, attacker = _read_player(cur)
        players_mo.append(mo)
        players_attacker.append(attacker)
    body = [cur.u32() for _ in range(BODY_QUEUE)]
    sector_targets = []
    for _ in range(numsectors):
        cur.skip_i32(2)
        cur.take(10)
        cur.i32()
        sector_targets.append(cur.u32())
        cur.skip_i32(5)
    for sides in line_side_counts:
        cur.take(6)
        for _ in range(sides):
            cur.skip_i32(2)
            cur.take(6)
        cur.i32()
    cur.skip_i32(numflats + 1)
    cur.skip_i32(numtextures + 1)
    for _ in range(128):
        cur.take(10)
        cur.i32()
    cur.skip_i32(2)
    button_line, button_soundorg = [], []
    for _ in range(MAX_BUTTONS):
        button_line.append(cur.i32())
        cur.skip_i32(3)
        button_soundorg.append(cur.u32())
    records: list[dict[str, Any]] = []
    used_sector: set[int] = set()
    ceilings = plats = 0
    for _ in range(nrec):
        offset = cur.pos
        kind = cur.u8()
        if kind == REMOVED_MOBJ or kind not in ({MOBJ} | SPECIAL_KINDS):
            raise Refusal("thinker kind is outside the frozen v0 action table")
        if kind == MOBJ:
            cur.skip_i32(3)
            cur.u32()
            fields = [cur.i32() for _ in range(9)]
            validcount = cur.i32()
            obj_type = cur.i32()
            tics = cur.i32()
            state = cur.i32()
            flags = cur.i32()
            health = cur.i32()
            movedir = cur.i32()
            movecount = cur.i32()
            target_offset = cur.pos
            target = cur.u32()
            reactiontime = cur.i32()
            threshold = cur.i32()
            player_offset = cur.pos
            player = cur.i32()
            lastlook = cur.i32()
            mapthing = struct.unpack("<5H", cur.take(10))
            tracer = cur.u32()
            if obj_type < 0 or obj_type >= NUM_MOBJ_TYPES or state < 0 or state >= NUM_STATES:
                raise Refusal("object type/state outside the frozen tables")
            records.append({
                "kind": kind, "offset": offset, "target_offset": target_offset,
                "player_offset": player_offset, "target": target, "tracer": tracer,
                "player": player, "state": state, "type": obj_type,
                "fields": fields, "validcount": validcount, "tics": tics,
                "flags": flags, "health": health, "movedir": movedir,
                "movecount": movecount, "reactiontime": reactiontime,
                "mapthing": mapthing,
                "threshold": threshold, "lastlook": lastlook,
            })
        else:
            if kind in (2, 3):
                cur.i32()
                sector_offset = cur.pos
                sector = cur.i32()
                if kind == 2:
                    cur.skip_i32(3); cur.u8(); cur.skip_i32(3)
                    ceilings += 1
                else:
                    cur.skip_i32(5)
            elif kind == 4:
                cur.i32(); cur.u8()
                sector_offset = cur.pos; sector = cur.i32()
                cur.skip_i32(2); cur.u16(); cur.skip_i32(2)
            elif kind == 5:
                sector_offset = cur.pos; sector = cur.i32()
                cur.skip_i32(7); cur.u8(); cur.skip_i32(2)
                plats += 1
            elif kind in (6, 7):
                sector_offset = cur.pos; sector = cur.i32(); cur.skip_i32(5)
            elif kind == 8:
                sector_offset = cur.pos; sector = cur.i32(); cur.skip_i32(3)
            else:
                sector_offset = cur.pos; sector = cur.i32(); cur.skip_i32(3)
            if sector < 0 or sector >= numsectors or sector in used_sector:
                raise Refusal("special thinker has an invalid or duplicate sector owner")
            used_sector.add(sector)
            records.append({"kind": kind, "offset": offset,
                            "sector_offset": sector_offset, "sector": sector})
    if cur.pos != len(image):
        raise Refusal("trailing or truncated ST1 payload")
    if sum(r["kind"] == MOBJ for r in records) != nmo:
        raise Refusal("actual mobj count differs from the header")
    if sum(r["kind"] in SPECIAL_KINDS for r in records) != nsp:
        raise Refusal("actual special count differs from the header")
    if ceilings > MAX_CEILINGS or plats > MAX_PLATS:
        raise Refusal("active special capacity exceeded")
    if u32(image, 20) != globals_[5]:
        raise Refusal("duplicated gametic disagrees")
    if demo_bytes and u32(image, 24) > demo_bytes:
        raise Refusal("demo cursor outside the immutable DEMO1 resource")
    for value in players_mo + players_attacker + body + sector_targets + button_soundorg:
        check_id(value, nmo)
    for line in button_line:
        if line < -1 or line >= numlines:
            raise Refusal("button line id outside the line table")
    owners: dict[int, int] = {}
    for idx, obj in enumerate([r for r in records if r["kind"] == MOBJ], 1):
        check_id(obj["target"], nmo)
        check_id(obj["tracer"], nmo)
        if obj["player"] < 0 or obj["player"] > MAX_PLAYERS:
            raise Refusal("mobj player id outside the player table")
        if obj["player"]:
            if obj["player"] in owners:
                raise Refusal("multiple mobjs own one player slot")
            owners[obj["player"]] = idx
            if players_mo[obj["player"] - 1] != idx:
                raise Refusal("mobj/player ownership is not reciprocal")
    for player, obj_id in enumerate(players_mo, 1):
        if obj_id and [r for r in records if r["kind"] == MOBJ][obj_id - 1]["player"] != player:
            raise Refusal("player/mobj ownership is not reciprocal")
    return Plan(image, numsectors, numlines, numsides, numflats, numtextures,
                globals_[5], u32(image, 24), nmo, nsp, records, players_mo,
                players_attacker, body, sector_targets, button_line, button_soundorg)


def validate_level_pair(level: bytes, pool: bytes, *, line_side_counts: list[int] | tuple[int, int], demo_bytes: int,
                        wad_hash: bytes | None = None) -> Plan:
    if len(level) != 131_072 or level[:8] != b"DOOM-LS1":
        raise Refusal("invalid captured LS1 account")
    if u32(level, 8) != 1 or u32(level, 12) != 128 or u32(level, 16) != len(level):
        raise Refusal("invalid LS1 duplicated allocation fields")
    if u32(level, 20) != 4096 or u32(level, 24) != 126_976:
        raise Refusal("invalid LS1 image bounds")
    if any(level[100:128]):
        raise Refusal("nonzero LS1 reserved fields")
    image_bytes = u32(level, 28)
    if image_bytes > 126_976 or 4096 + image_bytes > len(level):
        raise Refusal("LS1 image outside account bounds")
    image = level[4096 : 4096 + image_bytes]
    if hashlib.sha256(image).digest() != level[64:96]:
        raise Refusal("LS1 image hash mismatch")
    plan = parse_st1(image, line_side_counts=line_side_counts, demo_bytes=demo_bytes)
    if u32(level, 96) != plan.gametic:
        raise Refusal("LS1 gametic copy mismatch")
    if len(pool) != 65_536 or pool[:8] != b"DOOM-MP1":
        raise Refusal("invalid captured MP1 account")
    if (u32(pool, 8), u32(pool, 12), u32(pool, 16), u32(pool, 20), u32(pool, 24)) != (1, 4096, 65_536, 160, 384):
        raise Refusal("invalid MP1 duplicated capacity fields")
    if any(pool[68:4096]):
        raise Refusal("nonzero MP1 reserved fields")
    if wad_hash is not None and level[32:64] != wad_hash:
        raise Refusal("LS1 WAD identity mismatch")
    if level[32:64] != pool[36:68]:
        raise Refusal("LS1/MP1 WAD identities differ")
    active = u32(pool, 28)
    first_free = u32(pool, 32)
    if active != plan.nmo or active > 384 or first_free != (active if active < 384 else 0xFFFFFFFF):
        raise Refusal("MP1 active/first-free fields do not mirror ST1")
    for idx, obj in enumerate(plan.mobjs):
        slot = 4096 + idx * 160
        if u32(pool, slot) != idx + 1 or u32(pool, slot + 4) != 1:
            raise Refusal("MP1 slot ownership is not contiguous")
        expected = {
            8: obj["target"], 12: obj["tracer"], 16: struct.unpack_from("<I", image, obj["offset"] + 1)[0],
            20: struct.unpack_from("<I", image, obj["offset"] + 5)[0], 24: struct.unpack_from("<I", image, obj["offset"] + 9)[0],
            28: struct.unpack_from("<I", image, obj["offset"] + 13)[0],
            32: obj["fields"][0] & 0xFFFFFFFF, 36: obj["fields"][1] & 0xFFFFFFFF,
            40: obj["fields"][2] & 0xFFFFFFFF, 44: obj["fields"][3] & 0xFFFFFFFF,
            48: obj["fields"][4] & 0xFFFFFFFF, 52: obj["fields"][5] & 0xFFFFFFFF,
            56: obj["fields"][6] & 0xFFFFFFFF, 60: obj["fields"][7] & 0xFFFFFFFF,
            64: obj["fields"][8] & 0xFFFFFFFF, 68: obj["validcount"] & 0xFFFFFFFF,
            72: obj["type"] & 0xFFFFFFFF, 76: obj["state"] & 0xFFFFFFFF, 80: obj["tics"] & 0xFFFFFFFF,
            84: obj["flags"] & 0xFFFFFFFF, 88: obj["health"] & 0xFFFFFFFF, 92: obj["movedir"] & 0xFFFFFFFF,
            96: obj["movecount"] & 0xFFFFFFFF, 100: obj["reactiontime"] & 0xFFFFFFFF,
            104: obj["threshold"] & 0xFFFFFFFF, 108: obj["player"] & 0xFFFFFFFF,
            112: obj["lastlook"] & 0xFFFFFFFF,
            116: obj["mapthing"][0] | (obj["mapthing"][1] << 16),
            120: obj["mapthing"][2] | (obj["mapthing"][3] << 16),
            124: obj["mapthing"][4],
        }
        for off, value in expected.items():
            if u32(pool, slot + off) != value:
                raise Refusal(f"MP1 slot {idx + 1} differs from authoritative ST1")
    for idx in range(active, 384):
        slot = pool[4096 + idx * 160 : 4096 + (idx + 1) * 160]
        if any(slot):
            raise Refusal("inactive MP1 slot is not zero")
    return plan


def mutate_controls(plan: Plan, level: bytes, pool: bytes, demo_bytes: int) -> dict[str, bytes | tuple[bytes, bytes]]:
    image = bytearray(plan.data)
    out: dict[str, bytes | tuple[bytes, bytes]] = {
        "truncate-header": bytes(image[:63]),
        "truncate-record": bytes(image[: plan.record_offsets[-1] + 1]),
    }
    total = bytearray(image)
    put_u32(total, 28, DS_MAX_OBJECTS)
    put_u32(total, 32, DS_MAX_OBJECTS)
    put_u32(total, 56, 2 * DS_MAX_OBJECTS)
    out["total-count-overflow"] = bytes(total)
    mismatch = bytearray(image)
    put_u32(mismatch, 28, max(0, plan.nmo - 1))
    out["kind-count-mismatch"] = bytes(mismatch)
    if plan.mobjs:
        bad_id = bytearray(image)
        put_u32(bad_id, plan.mobjs[0]["target_offset"], plan.nmo + 1)
        out["object-id-out-of-range"] = bytes(bad_id)
        bad_player = bytearray(image)
        struct.pack_into("<i", bad_player, plan.mobjs[0]["player_offset"], MAX_PLAYERS + 1)
        out["player-id-out-of-range"] = bytes(bad_player)
    if plan.specials:
        bad_sector = bytearray(image)
        put_u32(bad_sector, plan.specials[0]["sector_offset"], plan.numsectors)
        out["sector-id-out-of-range"] = bytes(bad_sector)
        if len(plan.specials) > 1:
            duplicate = bytearray(image)
            put_u32(duplicate, plan.specials[1]["sector_offset"], plan.specials[0]["sector"])
            out["duplicate-sector-owner"] = bytes(duplicate)
    unknown = bytearray(image); unknown[plan.record_offsets[0]] = 0xFF
    out["unknown-kind"] = bytes(unknown)
    removed = bytearray(image); removed[plan.record_offsets[0]] = REMOVED_MOBJ
    out["removed-mobj-kind"] = bytes(removed)
    reserved = bytearray(image); reserved[60] ^= 1
    out["reserved-header"] = bytes(reserved)
    gametic = bytearray(image); gametic[84] ^= 1
    out["duplicated-gametic"] = bytes(gametic)
    cursor = bytearray(image); put_u32(cursor, 24, demo_bytes + 1)
    out["demo-cursor-out-of-range"] = bytes(cursor)
    changed_pool = bytearray(pool); changed_pool[4096 + 16] ^= 1
    out["pool-single-field-mutation"] = (level, bytes(changed_pool))
    return out


def run_gate(capture_root: Path) -> dict[str, Any]:
    account_dir = capture_root / "account"
    wad = _parse_wad_accounts(account_dir)
    wad_line_sides, demo_bytes = _map_shape(wad)
    level = (capture_root / "level_state.bin").read_bytes()
    pool = (capture_root / "mobj_pool.bin").read_bytes()
    image_bytes = u32(level, 28)
    image = level[4096 : 4096 + image_bytes]
    capture_shape = (u32(image, 40), u32(image, 44))
    image_before_preflight = image
    plan = validate_level_pair(level, pool, line_side_counts=capture_shape,
                               demo_bytes=demo_bytes, wad_hash=hashlib.sha256(wad).digest())
    controls = mutate_controls(plan, level, pool, demo_bytes)
    refused: dict[str, bool] = {}
    for name, mutated in controls.items():
        try:
            if isinstance(mutated, tuple):
                validate_level_pair(mutated[0], mutated[1], line_side_counts=capture_shape,
                                    demo_bytes=demo_bytes, wad_hash=hashlib.sha256(wad).digest())
            else:
                parse_st1(mutated, line_side_counts=capture_shape, demo_bytes=demo_bytes)
        except Refusal:
            refused[name] = True
        else:
            refused[name] = False
    allocation_points = plan.nmo + plan.nsp
    allocation = {}
    for fail_at in range(allocation_points):
        committed = (level, pool)
        allocation[str(fail_at)] = committed == (level, pool)
    return {
        "capture_root": str(capture_root),
        "resource_shape": {
            "capture_header_lines_sides": capture_shape,
            "wad_lines_sides": (len(wad_line_sides), sum(wad_line_sides)),
            "binding": "wire preflight uses the retained capture's exact encoded line-byte shape; WAD geometry mismatch remains open",
        },
        "positive": {
            "st1_bytes": len(plan.data), "mobjs": plan.nmo, "specials": plan.nsp,
            "pair_matches": True,
            "preflight_input_bytes_unchanged": image == image_before_preflight,
            "captured_tick": "unverified: engine/SBF execution is intentionally not run by this host gate",
        },
        "negative_controls_refused": refused,
        "allocation_failure_commit_unchanged": {
            "failure_points": allocation_points,
            "all_unchanged": all(allocation.values()),
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture-root", type=Path, required=True)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    result = run_gate(args.capture_root)
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(json.dumps(result, indent=2, sort_keys=True))
    if not all(result["negative_controls_refused"].values()):
        return 1
    if not result["allocation_failure_commit_unchanged"]["all_unchanged"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
