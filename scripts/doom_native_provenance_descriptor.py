#!/usr/bin/env python3
"""Build an external acceptance descriptor for the Doom native probes.

The descriptor is deliberately generated after the source commit is named and
outside the Rust tree which embeds the build manifest.  It therefore cannot
become a fixed point containing its own expected root.  The upstream source is
materialised from its pinned Git commit in a temporary worktree and the
ordered host patches are applied there before any expected bytes are hashed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from typing import Iterable


UPSTREAM_COMMIT = "a77dfb96cb91780ca334d0d4cfd86957558007e0"
PATCH_0014 = "repo/stunts/doom-host/patches/0014-doom-zero-levspec-thinkers.patch"
Z_ZONE = "external/z_zone.c"
PROFILE = "DOOM-NATIVE-PROVENANCE/sweep_native+fresh_resume_probe/ctx-engine-host-release/1"
SCHEMA = "DOOM-SIM/2-NATIVE-ACCEPTANCE-DESCRIPTOR/1"


def run(args: list[str], cwd: Path | None = None, *, input_bytes: bytes | None = None) -> bytes:
    result = subprocess.run(
        args,
        cwd=cwd,
        input=input_bytes,
        check=False,
        capture_output=True,
    )
    if result.returncode:
        raise SystemExit(
            f"command failed ({result.returncode}): {' '.join(args)}\n"
            f"{result.stderr.decode(errors='replace')}"
        )
    return result.stdout


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_record(path: str, data: bytes) -> dict[str, object]:
    return {"path": path, "bytes": len(data), "sha256": sha256(data)}


def git_files(repo: Path, commit: str, prefix: str, suffixes: set[str]) -> list[str]:
    names = run(["git", "-C", str(repo), "ls-tree", "-r", "--name-only", commit, prefix])
    return [
        name
        for name in names.decode().splitlines()
        if Path(name).suffix in suffixes
    ]


def git_blob(repo: Path, commit: str, path: str) -> bytes:
    return run(["git", "-C", str(repo), "show", f"{commit}:{path}"])


def repo_inputs(repo: Path, source_commit: str) -> dict[str, bytes]:
    paths: list[str] = [
        "stunts/doom-sbf/build.rs",
        "stunts/doom-sbf/Cargo.toml",
        "stunts/doom-sbf/Cargo.lock",
        "stunts/doom-sbf/driver/Cargo.toml",
        "stunts/doom-sbf/driver/Cargo.lock",
        "stunts/doom-sbf/driver/src/bin/sweep_native.rs",
        "stunts/doom-sbf/driver/src/bin/fresh_resume_probe.rs",
        "stunts/doom-sbf/driver/src/bin/continuation_split_sbf.rs",
        "stunts/doom-sbf/driver/src/sweep_common.rs",
        "stunts/doom-host/build.sh",
        "stunts/doom-host/tools/doom_controller_producer.py",
        "stunts/doom-host/tools/doom_account_image.py",
    ]
    paths.extend(git_files(repo, source_commit, "stunts/doom-sbf/src", {".rs", ".c", ".h", ".inc"}))
    paths.extend(git_files(repo, source_commit, "stunts/doom-sbf/sbfinc", {".h"}))
    paths.extend(git_files(repo, source_commit, "stunts/doom-host/patches", {".patch"}))
    records: dict[str, bytes] = {}
    for path in paths:
        manifest_path = f"repo/{path}"
        if manifest_path in records:
            raise SystemExit(f"duplicate expected repository input: {manifest_path}")
        records[manifest_path] = git_blob(repo, source_commit, path)
    return records


def apply_ordered_patches(
    upstream_repo: Path,
    upstream_commit: str,
    patch_records: Iterable[tuple[str, bytes]],
) -> dict[str, bytes]:
    """Materialise clean upstream and apply exactly the build.sh patch order."""

    with tempfile.TemporaryDirectory(prefix="basanos-doom-provenance-upstream-") as scratch:
        worktree = Path(scratch) / "upstream"
        run(["git", "-C", str(upstream_repo), "worktree", "add", "--detach", str(worktree), upstream_commit])
        patch_dir = Path(scratch) / "patches"
        patch_dir.mkdir()
        try:
            for manifest_path, data in sorted(patch_records):
                patch = patch_dir / Path(manifest_path).name
                patch.write_bytes(data)
                special = patch.name in {
                    "0005-doom-reentrant-strips.patch",
                    "0008-doom-captured-controller-tick.patch",
                    "0010-doom-controller-ingress-causality.patch",
                    "0011-doom-controlled-reentry-pair-ingress.patch",
                }
                extra = ["--unidiff-zero", "--recount"] if special else []
                run(["git", "-C", str(worktree), "apply", "--check", *extra, str(patch)])
                run(["git", "-C", str(worktree), "apply", *extra, str(patch)])
            source = worktree / "linuxdoom-1.10"
            if not source.is_dir():
                raise SystemExit(f"upstream tree has no linuxdoom-1.10: {upstream_commit}")
            records: dict[str, bytes] = {}
            for path in sorted(source.rglob("*")):
                if path.is_file() and path.suffix in {".c", ".h", ".inc"}:
                    records[f"external/{path.relative_to(source).as_posix()}"] = path.read_bytes()
            return records
        finally:
            run(["git", "-C", str(upstream_repo), "worktree", "remove", "--force", str(worktree)])


def input_root(records: list[dict[str, object]]) -> str:
    digest = hashlib.sha256()
    digest.update(b"DOOM-NATIVE-SOURCE-INPUT-ROOT/1\0")
    for record in records:
        digest.update(str(record["path"]).encode())
        digest.update(b"\0")
        digest.update(str(record["bytes"]).encode())
        digest.update(b"\0")
        digest.update(str(record["sha256"]).encode())
        digest.update(b"\n")
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--upstream-tree", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--oracle-directory", type=Path, required=True)
    parser.add_argument("--sweep-binary", type=Path, required=True)
    parser.add_argument("--probe-binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    repo = args.repo.resolve()
    manifest_path = args.manifest.resolve()
    manifest_bytes = manifest_path.read_bytes()
    manifest = json.loads(manifest_bytes)
    if manifest.get("schema") != "DOOM-EX2-NATIVE-SOURCE-MANIFEST/2":
        raise SystemExit("descriptor requires source manifest schema /2")
    if manifest.get("source_profile") != PROFILE:
        raise SystemExit("descriptor requires the host native-provenance profile")
    if manifest.get("compile_source_commit") != args.source_commit:
        raise SystemExit("manifest commit does not equal the named descriptor commit")
    if manifest.get("compile_source_dirty") is not False:
        raise SystemExit("descriptor source commit is not clean")
    if manifest.get("upstream_commit") != UPSTREAM_COMMIT:
        raise SystemExit("manifest upstream commit is not pinned")

    expected = repo_inputs(repo, args.source_commit)
    patches = [(path, data) for path, data in expected.items() if path.startswith("repo/stunts/doom-host/patches/")]
    expected.update(apply_ordered_patches(args.upstream_tree.resolve(), UPSTREAM_COMMIT, patches))
    expected_records = [file_record(path, expected[path]) for path in sorted(expected)]
    actual_records = manifest.get("inputs")
    if actual_records != expected_records:
        raise SystemExit("embedded manifest inputs differ from clean ordered source construction")
    expected_root = input_root(expected_records)
    if manifest.get("input_count") != len(expected_records) or manifest.get("input_root") != expected_root:
        raise SystemExit("embedded manifest input count/root is not independently reproducible")

    patch_0014 = next(record for record in expected_records if record["path"] == PATCH_0014)
    z_zone = next(record for record in expected_records if record["path"] == Z_ZONE)
    if patch_0014["sha256"] != sha256(expected[PATCH_0014]):
        raise SystemExit("patch 0014 digest construction failed")
    if z_zone["bytes"] == 0:
        raise SystemExit("clean ordered construction produced empty z_zone.c")

    def binary_record(path: Path) -> dict[str, object]:
        data = path.resolve().read_bytes()
        return {"path": str(path.resolve()), "bytes": len(data), "sha256": sha256(data)}

    descriptor = {
        "schema": SCHEMA,
        "profile": PROFILE,
        "oracle_directory": str(args.oracle_directory.resolve()),
        "binaries": {
            "sweep_native": binary_record(args.sweep_binary),
            "fresh_resume_probe": binary_record(args.probe_binary),
        },
        "compile": {
            "target": manifest["target"],
            "sbf": manifest["sbf"],
            "source_profile": manifest["source_profile"],
            "source_commit": manifest["compile_source_commit"],
            "source_tree": manifest["compile_source_tree"],
            "source_dirty": manifest["compile_source_dirty"],
            "upstream_commit": manifest["upstream_commit"],
            "source_manifest_sha256": sha256(manifest_bytes),
            "input_root": expected_root,
            "input_count": len(expected_records),
            "inputs": expected_records,
            "patch_0014": patch_0014,
            "z_zone": z_zone,
            "build_metadata": manifest["build_metadata"],
            "compiler_flags": manifest["compiler_flags"],
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(descriptor, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"path": str(args.output.resolve()), "bytes": args.output.stat().st_size, "sha256": sha256(args.output.read_bytes())}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
