#!/usr/bin/env python3
"""Generate the two-stage Doom native provenance descriptors.

The source/build descriptor is made before ``sweep_native`` and is the only
descriptor that can admit oracle creation.  The final acceptance descriptor is
made after the oracle completes and pins the exact oracle file table/root,
capture/resource inputs, command bytes, source manifest/root, and both retained
executables.  Neither descriptor copies expected build values from the
manifest: the host operator supplies an independently reviewed expected-build
configuration for the named release profile.
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
PROFILE = "DOOM-NATIVE-PROVENANCE/sweep_native+fresh_resume_probe/ctx-engine-host-release/2"
SOURCE_MANIFEST_SCHEMA = "DOOM-EX2-NATIVE-SOURCE-MANIFEST/3"
BUILD_DESCRIPTOR_SCHEMA = "DOOM-SIM/2-NATIVE-BUILD-DESCRIPTOR/2"
FINAL_DESCRIPTOR_SCHEMA = "DOOM-SIM/2-NATIVE-ACCEPTANCE-DESCRIPTOR/2"
EXPECTED_CONFIG_SCHEMA = "DOOM-NATIVE-HOST-RELEASE-CONFIG/1"
ORACLE_FILE_TABLE_DOMAIN = b"DOOM-SIM/2-NATIVE-ORACLE-FILE-TABLE/1\0"
INPUT_FILE_TABLE_DOMAIN = b"DOOM-SIM/2-NATIVE-INPUT-FILE-TABLE/1\0"
COMMAND_DOMAIN = "DOOM-SIM/2-COMMANDS/1"
COMMAND_WIDTH = 4
ORACLE_REQUIRED_FILES = {
    "commands.bin",
    "controls.json",
    "events.jsonl",
    "final-meta.bin",
    "final-mirror.mp1",
    "final-state.st1",
    "identity.json",
    "oracle-mp1.bin.gz",
    "oracle-per-tic.jsonl",
    "oracle-st1.bin.gz",
    "source-manifest.json",
    "summary.json",
}
CAPTURE_REQUIRED_FILES = {
    *(f"account/wad-{index:03d}.bin" for index in range(5)),
    "dumped-doom-st1.bin",
    "mobj_pool.bin",
}
RESOURCE_REQUIRED_FILES = {"resource-image.ri2"}
EXPECTED_CARGO_FEATURES = ["ctx-engine"]
EXPECTED_BINARY_TARGETS = ["sweep_native", "fresh_resume_probe"]
EXPECTED_NATIVE_C_FLAGS = [
    "-O2",
    "-std=gnu89",
    "-DNORMALUNIX",
    "-DHEADLESS_DOOM",
    "-Wno-deprecated-non-prototype",
    "-Wno-implicit-int",
    "-Wno-int-conversion",
    "-Wno-incompatible-pointer-types",
    "-Wno-format-security",
]


def run(args: list[str], cwd: Path | None = None, *, input_bytes: bytes | None = None) -> bytes:
    result = subprocess.run(args, cwd=cwd, input=input_bytes, check=False, capture_output=True)
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


def path_record(path: Path) -> dict[str, object]:
    resolved = path.resolve()
    return {"path": str(resolved), "bytes": resolved.stat().st_size, "sha256": sha256(resolved.read_bytes())}


def git_files(repo: Path, commit: str, prefix: str, suffixes: set[str]) -> list[str]:
    names = run(["git", "-C", str(repo), "ls-tree", "-r", "--name-only", commit, prefix])
    return [name for name in names.decode().splitlines() if Path(name).suffix in suffixes]


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


def input_root(records: list[dict[str, object]], domain: bytes = b"DOOM-NATIVE-SOURCE-INPUT-ROOT/1\0") -> str:
    digest = hashlib.sha256()
    digest.update(domain)
    for record in records:
        digest.update(str(record["path"]).encode())
        digest.update(b"\0")
        digest.update(str(record["bytes"]).encode())
        digest.update(b"\0")
        digest.update(str(record["sha256"]).encode())
        digest.update(b"\n")
    return digest.hexdigest()


def directory_records(root: Path, expected: set[str], label: str) -> list[dict[str, object]]:
    root = root.resolve()
    if not root.is_dir():
        raise SystemExit(f"{label} directory is missing: {root}")
    records = []
    for relative in sorted(expected):
        path = root / relative
        if not path.is_file() or path.is_symlink():
            raise SystemExit(f"{label} required file is missing or is a symlink: {relative}")
        records.append(file_record(relative, path.read_bytes()))
    return records


def validate_expected_configuration(manifest: dict[str, object], expected: dict[str, object]) -> None:
    if expected.get("schema") != EXPECTED_CONFIG_SCHEMA:
        raise SystemExit("expected build configuration has an unsupported schema")
    if expected.get("target") != manifest.get("target"):
        raise SystemExit("manifest target differs from the independently pinned host target")
    for key in (
        "allowed_cargo_features",
        "enabled_cargo_features",
        "input_scope",
        "rust_module_closure",
        "c_translation_units",
        "c_translation_units_scope",
        "c_headers",
        "build_scripts_and_locks",
        "build_metadata",
        "compiler_flags",
    ):
        if manifest.get(key) != expected.get(key):
            raise SystemExit(f"manifest {key} differs from the independently pinned host profile")
    if manifest.get("allowed_cargo_features") != EXPECTED_CARGO_FEATURES:
        raise SystemExit("native host profile advertises an unexpected Cargo feature allowlist")
    if manifest.get("enabled_cargo_features") != EXPECTED_CARGO_FEATURES:
        raise SystemExit("native host build has extra or missing Cargo features")
    metadata = manifest.get("build_metadata")
    if not isinstance(metadata, dict) or metadata.get("target") != manifest.get("target"):
        raise SystemExit("native host build metadata is missing its effective target")
    profile = metadata.get("profile")
    if not isinstance(profile, dict) or profile != {
        "name": "release",
        "opt_level": "3",
        "debug": "false",
        "debug_assertions": "false",
        "overflow_checks": "false",
        "lto": "true",
        "codegen_units": "1",
    }:
        raise SystemExit("native host build is not the pinned release/no-debug profile")
    for tool_name in ("rustc", "cargo", "c_compiler", "linker", "archiver"):
        tool = metadata.get(tool_name)
        if not isinstance(tool, dict) or not tool.get("path") or not tool.get("version"):
            raise SystemExit(f"native host build is missing {tool_name} path/version")
        version = str(tool["version"])
        if version.startswith(("unavailable:", "failed:")):
            raise SystemExit(f"native host build has an unsuccessful {tool_name} version output")
    rust = metadata.get("rust")
    c_environment = metadata.get("c_environment")
    c_compilation = metadata.get("c_compilation")
    if not isinstance(rust, dict) or not isinstance(c_environment, dict) or not isinstance(c_compilation, dict):
        raise SystemExit("native host build lacks normalized Rust/C effective flags")
    if rust.get("rustflags") or rust.get("encoded_rustflags"):
        raise SystemExit("native host release profile has unexpected Rust flags")
    if c_environment.get("cflags") or c_environment.get("cppflags") or c_environment.get("c_include_path"):
        raise SystemExit("native host release profile has unexpected C environment flags")
    if manifest.get("compiler_flags") != EXPECTED_NATIVE_C_FLAGS:
        raise SystemExit("native host release profile has unexpected compiler flags")
    if (
        c_compilation.get("hotloop_flags")
        or c_compilation.get("hotloop_defines")
        or c_compilation.get("engine_flags") != EXPECTED_NATIVE_C_FLAGS
        or c_compilation.get("engine_defines") != ["NORMALUNIX", "HEADLESS_DOOM"]
    ):
        raise SystemExit("native host release profile has unexpected C defines")


def build_records(repo: Path, source_commit: str, upstream_tree: Path, manifest: dict[str, object]) -> tuple[list[dict[str, object]], str]:
    expected = repo_inputs(repo, source_commit)
    patches = [(path, data) for path, data in expected.items() if path.startswith("repo/stunts/doom-host/patches/")]
    expected.update(apply_ordered_patches(upstream_tree.resolve(), UPSTREAM_COMMIT, patches))
    records = [file_record(path, expected[path]) for path in sorted(expected)]
    if manifest.get("inputs") != records:
        raise SystemExit("embedded manifest inputs differ from clean ordered source construction")
    root = input_root(records)
    if manifest.get("input_count") != len(records) or manifest.get("input_root") != root:
        raise SystemExit("embedded manifest input count/root is not independently reproducible")
    patch = next(record for record in records if record["path"] == PATCH_0014)
    z_zone = next(record for record in records if record["path"] == Z_ZONE)
    if patch["sha256"] != sha256(expected[PATCH_0014]) or z_zone["bytes"] == 0:
        raise SystemExit("ordered patch construction did not produce the pinned patch/z_zone inputs")
    return records, root


def load_json(path: Path, label: str) -> dict[str, object]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot load {label}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"{label} must be a JSON object")
    return value


def descriptor_pin(path: Path) -> dict[str, object]:
    return path_record(path)


def validate_external_descriptor(path: Path, expected_sha: str, expected_bytes: int) -> dict[str, object]:
    data = path.resolve().read_bytes()
    if len(data) != expected_bytes or sha256(data) != expected_sha:
        raise SystemExit("descriptor external SHA-256/length pin does not match the descriptor bytes")
    return load_json(path, "descriptor")


def generate_build_descriptor(args: argparse.Namespace) -> int:
    repo = args.repo.resolve()
    manifest_path = args.manifest.resolve()
    manifest_bytes = manifest_path.read_bytes()
    manifest = load_json(manifest_path, "source manifest")
    if manifest.get("schema") != SOURCE_MANIFEST_SCHEMA:
        raise SystemExit("build descriptor requires source manifest schema /3")
    if manifest.get("source_profile") != PROFILE:
        raise SystemExit("build descriptor requires the versioned host native profile")
    if manifest.get("compile_source_commit") != args.source_commit:
        raise SystemExit("manifest commit does not equal the named descriptor commit")
    if manifest.get("compile_source_dirty") is not False or manifest.get("sbf") is not False:
        raise SystemExit("build descriptor requires a clean host, non-SBF build")
    if manifest.get("binary_targets") != EXPECTED_BINARY_TARGETS:
        raise SystemExit("build descriptor requires both native binary targets")
    expected = load_json(args.expected_build_configuration.resolve(), "expected build configuration")
    validate_expected_configuration(manifest, expected)
    records, root = build_records(repo, args.source_commit, args.upstream_tree, manifest)

    binaries = {
        "sweep_native": path_record(args.sweep_binary),
        "fresh_resume_probe": path_record(args.probe_binary),
    }
    compile_identity = {
        "manifest_schema": manifest["schema"],
        "target": manifest["target"],
        "sbf": manifest["sbf"],
        "source_profile": manifest["source_profile"],
        "source_commit": manifest["compile_source_commit"],
        "source_tree": manifest["compile_source_tree"],
        "source_dirty": manifest["compile_source_dirty"],
        "upstream_commit": manifest["upstream_commit"],
        "source_manifest": path_record(manifest_path),
        "input_root": root,
        "input_count": len(records),
        "inputs": records,
        "patch_0014": next(record for record in records if record["path"] == PATCH_0014),
        "z_zone": next(record for record in records if record["path"] == Z_ZONE),
        "allowed_cargo_features": manifest["allowed_cargo_features"],
        "enabled_cargo_features": manifest["enabled_cargo_features"],
        "input_scope": manifest["input_scope"],
        "rust_module_closure": manifest["rust_module_closure"],
        "c_translation_units": manifest["c_translation_units"],
        "c_translation_units_scope": manifest["c_translation_units_scope"],
        "c_headers": manifest["c_headers"],
        "build_scripts_and_locks": manifest["build_scripts_and_locks"],
        "build_metadata": manifest["build_metadata"],
        "compiler_flags": manifest["compiler_flags"],
    }
    descriptor = {
        "schema": BUILD_DESCRIPTOR_SCHEMA,
        "version": 2,
        "stage": "source-build",
        "profile": PROFILE,
        "required_files": ["source-manifest", "sweep_native", "fresh_resume_probe"],
        "ordering": "canonical JSON keys sorted; input records lexicographic by UTF-8 path",
        "widths": {"sha256": 32, "bytes": 64, "commit": 20, "tree": 20},
        "domain": "DOOM-NATIVE-SOURCE-INPUT-ROOT/1",
        "compile": compile_identity,
        "binaries": binaries,
        "expected_build_configuration": descriptor_pin(args.expected_build_configuration),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(descriptor, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"stage": "source-build", **descriptor_pin(args.output)}))
    return 0


def generate_final_descriptor(args: argparse.Namespace) -> int:
    build_path = args.build_descriptor.resolve()
    build = validate_external_descriptor(build_path, args.build_descriptor_sha256, args.build_descriptor_bytes)
    if build.get("schema") != BUILD_DESCRIPTOR_SCHEMA or build.get("stage") != "source-build" or build.get("version") != 2:
        raise SystemExit("final descriptor requires a version-2 source-build descriptor")
    oracle = args.oracle_directory.resolve()
    oracle_records = directory_records(oracle, ORACLE_REQUIRED_FILES, "oracle")
    actual_oracle_files = {
        path.relative_to(oracle).as_posix()
        for path in oracle.rglob("*")
        if path.is_file() and not path.is_symlink()
    }
    if actual_oracle_files != {str(record["path"]) for record in oracle_records}:
        raise SystemExit("oracle directory contains missing or unexpected files")
    capture_records = directory_records(args.capture.resolve(), CAPTURE_REQUIRED_FILES, "capture")
    resource_path = args.resource.resolve()
    if resource_path.name != "resource-image.ri2":
        raise SystemExit("resource input must be named resource-image.ri2")
    if not resource_path.is_file() or resource_path.is_symlink():
        raise SystemExit("resource input is missing or is a symlink")
    resource_records = [file_record("resource-image.ri2", resource_path.read_bytes())]
    identity = load_json(oracle / "identity.json", "oracle identity")
    if identity.get("schema") != "DOOM-SIM/2-NATIVE-ORACLE-IDENTITY/4":
        raise SystemExit("oracle identity must be created by the source-build stage")
    build_pin = identity.get("build_descriptor")
    if build_pin != path_record(build_path):
        raise SystemExit("oracle identity does not retain the externally pinned source-build descriptor")
    commands = (oracle / "commands.bin").read_bytes()
    if len(commands) % COMMAND_WIDTH or len(commands) == 0:
        raise SystemExit("oracle command stream has a non-integral or empty record width")
    oracle_root = input_root(oracle_records, ORACLE_FILE_TABLE_DOMAIN)
    capture_root = input_root(capture_records, INPUT_FILE_TABLE_DOMAIN)
    resource_root = input_root(resource_records, INPUT_FILE_TABLE_DOMAIN)
    descriptor = {
        "schema": FINAL_DESCRIPTOR_SCHEMA,
        "version": 2,
        "stage": "acceptance",
        "profile": PROFILE,
        "required_files": {
            "oracle": sorted(ORACLE_REQUIRED_FILES),
            "capture": sorted(CAPTURE_REQUIRED_FILES),
            "resource": ["resource-image.ri2"],
        },
        "ordering": "lexicographic UTF-8 relative paths; file table must be complete",
        "widths": {"sha256": 32, "bytes": 64, "command_record": COMMAND_WIDTH},
        "domains": {
            "oracle_file_table": "DOOM-SIM/2-NATIVE-ORACLE-FILE-TABLE/1",
            "input_file_table": "DOOM-SIM/2-NATIVE-INPUT-FILE-TABLE/1",
            "commands": COMMAND_DOMAIN,
        },
        "eof": "exact: no missing or extra files, and commands.bin is exactly count*record_width",
        "build_descriptor": path_record(build_path),
        "compile": build["compile"],
        "binaries": {
            "sweep_native": path_record(args.sweep_binary),
            "fresh_resume_probe": path_record(args.probe_binary),
        },
        "oracle": {
            "directory": str(oracle),
            "file_table": oracle_records,
            "file_table_root": oracle_root,
        },
        "capture": {
            "directory": str(args.capture.resolve()),
            "file_table": capture_records,
            "file_table_root": capture_root,
        },
        "resource": {
            "path": str(resource_path),
            "file_table": resource_records,
            "file_table_root": resource_root,
        },
        "commands": {
            "path": "commands.bin",
            "bytes": len(commands),
            "sha256": sha256(commands),
            "count": len(commands) // COMMAND_WIDTH,
            "record_width": COMMAND_WIDTH,
            "ordering": "sequence 0..count-1, four-byte little-endian ticcmd records",
            "domain": COMMAND_DOMAIN,
            "eof": "exact",
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(descriptor, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"stage": "acceptance", **descriptor_pin(args.output)}))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stage", choices=("source-build", "acceptance"), required=True)
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--build-descriptor", type=Path)
    parser.add_argument("--build-descriptor-sha256")
    parser.add_argument("--build-descriptor-bytes", type=int)
    parser.add_argument("--source-commit")
    parser.add_argument("--upstream-tree", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--expected-build-configuration", type=Path)
    parser.add_argument("--oracle-directory", type=Path)
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--resource", type=Path)
    parser.add_argument("--sweep-binary", type=Path, required=True)
    parser.add_argument("--probe-binary", type=Path, required=True)
    args = parser.parse_args()
    if args.stage == "source-build":
        required = (args.source_commit, args.upstream_tree, args.manifest, args.expected_build_configuration)
        if any(value is None for value in required):
            parser.error("source-build requires --source-commit, --upstream-tree, --manifest, and --expected-build-configuration")
        return generate_build_descriptor(args)
    required = (
        args.build_descriptor,
        args.build_descriptor_sha256,
        args.build_descriptor_bytes,
        args.oracle_directory,
        args.capture,
        args.resource,
    )
    if any(value is None for value in required):
        parser.error("acceptance requires the build pin, oracle, capture, and resource arguments")
    return generate_final_descriptor(args)


if __name__ == "__main__":
    raise SystemExit(main())
