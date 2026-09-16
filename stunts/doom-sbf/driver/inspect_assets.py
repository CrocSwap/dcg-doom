import argparse
import hashlib
import json
import re
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "doom-host" / "tools"))
from doom_account_image import _verify_level_state, verify_manifest


def digest(image, wad_hash):
    return hashlib.sha256(b"DOOM-ST1" + bytes(4) + wad_hash + image).hexdigest()


def evidence_inventory(path):
    ids = re.findall(r"^\| M(\d+) \|", path.read_text(), re.M)
    assert len(ids) == len(set(ids))
    return {"rows": len(ids), "maximum": max(map(int, ids))}


def main():
    parser = argparse.ArgumentParser(description="Audit retained step-3 inputs; not SBF replay")
    parser.add_argument("--loader", type=Path, required=True)
    parser.add_argument("--goldens", type=Path, required=True)
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    manifest, wad = verify_manifest(args.loader / "account" / "manifest.json", require_pinned_wad=True)
    wad_hash = bytes.fromhex(manifest["wad_sha256"])
    demo = wad[23_468:23_468 + 20_118]
    assert hashlib.sha256(demo).hexdigest() == "7f32ed06308fc14d7fbe87baf9e318f3500516d724ce69a32b339d588ba1fe1b"
    account, image = _verify_level_state(args.loader / "level_state.bin")
    assert image == (args.loader / "dumped-doom-st1.bin").read_bytes()
    assert image == (args.goldens / "checkpoint-0.st1").read_bytes()
    checkpoints = []
    wanted = {0, 99}
    rows = {}
    with (args.goldens / "state-normal.jsonl").open() as stream:
        for line in stream:
            row = json.loads(line)
            if row["gametic"] in wanted:
                rows[row["gametic"]] = row["sha256"]
    s0 = json.loads((args.goldens / "s0.jsonl").read_text())["sha256"]
    for tic in (0, 1, 100):
        path = args.goldens / f"checkpoint-{tic}.st1"
        state = path.read_bytes()
        assert state == path.with_name(f"checkpoint-{tic}-resave.st1").read_bytes()
        assert struct.unpack_from("<I", state, 20)[0] == tic
        hashed = digest(state, wad_hash)
        expected = s0 if tic == 0 else rows[tic - 1]
        checkpoints.append({"after_tics": tic, "bytes": len(state), "state_digest": hashed,
                            "jsonl_digest": expected, "jsonl_equal": hashed == expected,
                            "episode_map": list(struct.unpack_from("<ii", state, 68)),
                            "path": str(path.resolve())})
    print(json.dumps({
        "classification": "measured retained inputs only; no engine executed",
        "loader": str(args.loader.resolve()), "wad_sha256": manifest["wad_sha256"],
        "chunk_bytes": [p.stat().st_size for p in sorted((args.loader / "account").glob("wad-*.bin"))],
        "level_bytes": len(account), "pool_bytes": (args.loader / "mobj_pool.bin").stat().st_size,
        "demo_header": list(demo[:13]), "first_command": list(demo[13:17]),
        "checkpoints": checkpoints,
        "evidence": evidence_inventory(args.evidence) if args.evidence else None,
    }, indent=2))


if __name__ == "__main__":
    main()
