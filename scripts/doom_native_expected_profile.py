#!/usr/bin/env python3
"""Build the reviewed Doom host profile before any Cargo manifest exists.

This generator reads only the checked-in profile template, the named source
commit, the pinned upstream checkout, and successful tool probes.  It never
opens a produced Cargo/build manifest; the output is the independent profile
that the later descriptor stage compares with that manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "stunts" / "doom-sbf" / "doom-native-host-profile-v2.json"
UPSTREAM_COMMIT = "a77dfb96cb91780ca334d0d4cfd86957558007e0"
SCHEMA = "DOOM-NATIVE-HOST-RELEASE-CONFIG/2"
PORTABLE_ARCHIVER_PROBE = "DOOM-NATIVE-ARCHIVER-PROBE/1"
TOOL_PREFLIGHT_SCHEMA = "DOOM-NATIVE-TOOL-PREFLIGHT/1"


def fail(message: str) -> None:
    raise SystemExit(message)


def run(argv: list[str], cwd: Path | None = None) -> str:
    result = subprocess.run(argv, cwd=cwd, check=False, capture_output=True, text=True)
    if result.returncode:
        fail(f"command failed ({result.returncode}): {' '.join(argv)}\n{result.stderr.strip()}")
    return result.stdout.strip()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def resolve_executable(value: str, label: str) -> str:
    if not value or any(char.isspace() for char in value):
        fail(f"{label} must name one executable without arguments")
    selected = shutil.which(value) if not Path(value).is_absolute() else value
    if not selected:
        fail(f"{label} is not on PATH: {value}")
    try:
        # Keep the selected basename/symlink spelling (rustup dispatches based
        # on argv[0]) while making the recorded path absolute.
        path = Path(os.path.abspath(selected))
    except OSError as error:
        fail(f"{label} cannot be resolved: {error}")
    if not path.is_file() or not os.access(path, os.X_OK):
        fail(f"{label} is not an executable file: {path}")
    return str(path)


def executable_record(path: str) -> dict[str, object]:
    data = Path(path).read_bytes()
    return {"bytes": len(data), "sha256": sha256(data)}


def probe(name: str, default: str) -> dict[str, object]:
    value = resolve_executable(os.environ.get(name, default), name)
    if name == "AR":
        return probe_archiver(value)
    result = subprocess.run([value, "--version"], check=False, capture_output=True, text=True)
    if result.returncode or not (result.stdout + result.stderr).strip():
        fail(f"{name} --version must succeed before the expected profile is generated")
    return {"path": value, "version": (result.stdout + result.stderr).strip()}


def probe_archiver(value: str) -> dict[str, object]:
    """Use the same portable identity policy as the Rust build script.

    Apple's system ``ar`` has no successful ``--version`` option.  The
    fallback is deliberately a genuine archive create/list operation, not a
    mocked version string or a help-text check.
    """

    value = resolve_executable(value, "AR")
    identity = executable_record(value)
    version = subprocess.run([value, "--version"], check=False, capture_output=True, text=True)
    if version.returncode == 0 and (version.stdout + version.stderr).strip():
        return {
            "path": value,
            "version": (version.stdout + version.stderr).strip(),
            **identity,
        }
    try:
        with tempfile.TemporaryDirectory(prefix="basanos-doom-ar-probe-") as directory:
            root = Path(directory)
            member = root / "member.o"
            archive = root / "libprobe.a"
            member.write_bytes(b"basanos-doom-archiver-probe\n")
            create = subprocess.run(
                [value, "-rcS", str(archive), str(member)],
                check=False,
                capture_output=True,
                text=True,
            )
            listing = subprocess.run(
                [value, "-t", str(archive)],
                check=False,
                capture_output=True,
                text=True,
            )
            listed_member = any(line.strip() == member.name for line in listing.stdout.splitlines())
            if create.returncode == 0 and listing.returncode == 0 and archive.is_file() and listed_member:
                return {"path": value, "version": PORTABLE_ARCHIVER_PROBE, **identity}
    except OSError as error:
        fail(f"AR portable archive probe could not execute: {error}")
    fail("AR --version and the portable archive create/list probe both failed")


def probe_path(value: str, label: str) -> dict[str, str]:
    value = resolve_executable(value, label)
    result = subprocess.run([value, "--version"], check=False, capture_output=True, text=True)
    if result.returncode or not (result.stdout + result.stderr).strip():
        fail(f"{label} --version must succeed before the expected profile is generated")
    return {"path": value, "version": (result.stdout + result.stderr).strip()}


def host_target(path: str) -> str:
    result = subprocess.run([path, "-vV"], check=False, capture_output=True, text=True)
    if result.returncode:
        fail("rustc -vV must succeed before the expected profile is generated")
    for line in result.stdout.splitlines():
        if line.startswith("host: "):
            return line.split(": ", 1)[1].strip()
    fail("rustc -vV did not report a host target")


def normalized_env(name: str) -> str:
    return " ".join(os.environ.get(name, "").split())


def encoded_env(name: str) -> list[str]:
    value = os.environ.get(name, "")
    return value.split("\x1f") if value else []


def git(repo: Path, *args: str) -> str:
    return run(["git", "-C", str(repo), *args])


def source_tree(repo: Path, source_commit: str) -> str:
    resolved = git(repo, "rev-parse", f"{source_commit}^{{commit}}")
    if resolved != source_commit:
        fail(f"source commit is not the exact requested commit: {resolved}")
    return git(repo, "rev-parse", f"{source_commit}^{{tree}}")


def upstream_root(upstream_repo: Path) -> Path:
    root = upstream_repo / "linuxdoom-1.10"
    if not root.is_dir():
        fail(f"upstream checkout is missing linuxdoom-1.10: {root}")
    actual = git(upstream_repo, "rev-parse", "HEAD")
    if actual != UPSTREAM_COMMIT:
        fail(f"upstream checkout is not the literal pinned commit: {actual}")
    return root.resolve()


def source_names(repo: Path, source_commit: str, prefix: str, suffixes: set[str]) -> list[str]:
    names = git(repo, "ls-tree", "-r", "--name-only", source_commit, prefix).splitlines()
    return [name for name in names if Path(name).suffix in suffixes]


def file_record(path: str, absolute: Path) -> dict[str, object]:
    data = absolute.read_bytes()
    return {"path": path, "absolute_path": str(absolute), "bytes": len(data), "sha256": sha256(data)}


def profile(args: argparse.Namespace) -> dict[str, object]:
    template = json.loads(TEMPLATE.read_text())
    if template.get("schema") != SCHEMA or template.get("version") != 2:
        fail("reviewed host profile template has an unsupported schema/version")
    if os.environ.get("RUSTC_WRAPPER") or os.environ.get("RUSTC_WORKSPACE_WRAPPER"):
        fail("native host profile requires empty RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER")
    if normalized_env("RUSTFLAGS") or encoded_env("CARGO_ENCODED_RUSTFLAGS"):
        fail("native host profile requires empty Rust flags")
    if normalized_env("CFLAGS") or normalized_env("CPPFLAGS") or normalized_env("C_INCLUDE_PATH"):
        fail("native host profile requires empty C environment flags")

    repo = args.repo.resolve()
    upstream_repo = args.upstream_tree.resolve()
    compile_tree = source_tree(repo, args.source_commit)
    tree = upstream_root(upstream_repo)
    rustc = probe("RUSTC", "rustc")
    target = args.target or host_target(rustc["path"])
    cargo = probe("CARGO", "cargo")
    compiler = probe("CC", "cc")
    archiver = probe("AR", "ar")
    linker_name = os.environ.get(
        "RUSTC_LINKER",
        os.environ.get(
            f"CARGO_TARGET_{target.replace('-', '_').upper()}_LINKER",
            compiler["path"],
        ),
    )
    if any(char.isspace() for char in linker_name):
        fail("RUSTC_LINKER must name one executable without arguments")
    linker = linker_name
    linker_identity = probe_path(linker, "effective linker")

    repo_root = repo / "stunts" / "doom-sbf"
    root_paths = {
        "upstream": str(tree),
        "upstream/headless": str(tree / "headless"),
        "repo/sbfinc": str((repo_root / "sbfinc").resolve()),
    }
    groups: list[dict[str, object]] = []
    for group in template["host_groups"]:
        value = dict(group)
        value["include_roots"] = [root_paths[key] for key in group["include_root_keys"]]
        value["compiler"] = compiler
        value["archiver"] = archiver
        groups.append(value)
    translation_units = [path for group in groups for path in group["files"]]
    header_root = tree / "headless" / "doom_ctx.h"
    if not header_root.is_file():
        fail(f"reviewed header root is missing: {header_root}")
    header_paths = [
        f"repo/{name}"
        for prefix in ("stunts/doom-sbf/src", "stunts/doom-sbf/sbfinc")
        for name in source_names(repo, args.source_commit, prefix, {".h", ".inc"})
    ]
    header_paths.extend(
        f"external/{path.relative_to(tree).as_posix()}"
        for path in sorted(tree.rglob("*"))
        if path.is_file() and path.suffix in {".h", ".inc"}
    )
    header_paths.sort()
    metadata = {
        "target": target,
        "rustc": rustc,
        "cargo": cargo,
        "c_compiler": compiler,
        "linker": linker_identity,
        "archiver": archiver,
        "profile": template["profile"],
        "cargo_features": template["enabled_cargo_features"],
        "rust": {
            "rustflags": normalized_env("RUSTFLAGS"),
            "encoded_rustflags": encoded_env("CARGO_ENCODED_RUSTFLAGS"),
            "rustc_wrapper": "",
            "rustc_workspace_wrapper": "",
        },
        "c_environment": {
            "cflags": normalized_env("CFLAGS"),
            "cppflags": normalized_env("CPPFLAGS"),
            "c_include_path": normalized_env("C_INCLUDE_PATH"),
        },
        "c_compilation": {
            "hotloop_flags": [],
            "hotloop_defines": [],
            "engine_flags": template["compiler_flags"],
            "engine_defines": ["NORMALUNIX", "HEADLESS_DOOM"],
            "groups": groups,
            "group_scope": "exact-host-cc-build-contract/1",
            "header_root": file_record("external/headless/doom_ctx.h", header_root),
            "translation_units": translation_units,
            "translation_unit_scope": "exact-host-build-inputs/1",
        },
        "rust_module_closure": [
            f"{name}: {','.join(paths)}"
            for name, paths in template["rust_module_closure"].items()
        ],
        "effective_linker_source": "RUSTC_LINKER-or-target-linker-or-cc-rs-compiler",
    }
    return {
        "schema": SCHEMA,
        "version": 2,
        "target": target,
        "compile_source_commit": args.source_commit,
        "compile_source_tree": compile_tree,
        "upstream_commit": UPSTREAM_COMMIT,
        "allowed_cargo_features": template["allowed_cargo_features"],
        "enabled_cargo_features": template["enabled_cargo_features"],
        "input_scope": template["input_scope"],
        "rust_module_closure": template["rust_module_closure"],
        "c_translation_units": translation_units,
        "c_translation_units_scope": "conservative-source-envelope",
        "c_headers": {
            "scope": "conservative-source-envelope",
            "paths": header_paths,
            "header_root": template["header_root"],
        },
        "build_scripts_and_locks": template["build_scripts_and_locks"],
        "compiler_flags": template["compiler_flags"],
        "build_metadata": metadata,
        "source_checks": {
            "source_tree": compile_tree,
            "upstream_commit": UPSTREAM_COMMIT,
            "header_root": template["header_root"],
        },
    }


def tool_preflight(output: Path) -> int:
    tools = {
        name: probe(name, default)
        for name, default in (("RUSTC", "rustc"), ("CARGO", "cargo"), ("CC", "cc"), ("AR", "ar"))
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps({"schema": TOOL_PREFLIGHT_SCHEMA, "tools": tools}, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"schema": TOOL_PREFLIGHT_SCHEMA, "path": str(output.resolve()), "tools": tools}, sort_keys=True))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--source-commit")
    parser.add_argument("--upstream-tree", type=Path)
    parser.add_argument("--target")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--preflight-tools", action="store_true")
    args = parser.parse_args()
    if args.preflight_tools:
        return tool_preflight(args.output)
    if not args.source_commit or args.upstream_tree is None:
        parser.error("--source-commit and --upstream-tree are required unless --preflight-tools is set")
    output = profile(args)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n")
    data = args.output.read_bytes()
    print(json.dumps({"schema": SCHEMA, "path": str(args.output.resolve()), "bytes": len(data), "sha256": sha256(data)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
