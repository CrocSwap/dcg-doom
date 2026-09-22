#!/usr/bin/env bash
set -euo pipefail

# Operator lane for the bounded Doom native provenance gate. The source
# commit/tree are supplied by the later qualification note after the code
# commit; this script never follows a moving branch tip.

die() {
  echo "doom native provenance: $*" >&2
  exit 2
}

: "${SOURCE_COMMIT:?set SOURCE_COMMIT to the reviewed code commit}"
: "${SOURCE_TREE:?set SOURCE_TREE to SOURCE_COMMIT^{tree}}"
: "${RECEIPTS:?set RECEIPTS to a new final-native receipt root}"
: "${CAPTURE:?set CAPTURE to the retained Doom account-image capture}"
: "${RESOURCE:?set RESOURCE to the retained RI2 resource image}"

test -d "$CAPTURE" && test ! -L "$CAPTURE" || die "CAPTURE must be a real directory: $CAPTURE"
test -f "$RESOURCE" && test ! -L "$RESOURCE" || die "RESOURCE must be a real file: $RESOURCE"

ROOT=$(git rev-parse --show-toplevel)
SOURCE=${SOURCE:-/private/tmp/basanos-doom-native-tool-preflight-repair-20260922-source}
SCRATCH=${SCRATCH:-/private/tmp/basanos-doom-native-tool-preflight-repair-20260922-scratch}
TARGET=${TARGET:-/private/tmp/basanos-doom-native-tool-preflight-repair-20260922-target}
PYTHON_BIN=${PYTHON_BIN:-$ROOT/.venv/bin/python}
test -x "$PYTHON_BIN" || die "PYTHON_BIN is not executable: $PYTHON_BIN"

sha256_file() {
  "$PYTHON_BIN" - "$1" <<'PY'
import hashlib
import sys

digest = hashlib.sha256()
with open(sys.argv[1], "rb") as stream:
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
print(digest.hexdigest())
PY
}

sha256_stdin() {
  "$PYTHON_BIN" -c '
import hashlib
import sys

digest = hashlib.sha256()
for chunk in iter(lambda: sys.stdin.buffer.read(1024 * 1024), b""):
    digest.update(chunk)
print(digest.hexdigest())
'
}

[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || die "SOURCE_COMMIT must be a 40-byte hexadecimal commit"
[[ "$SOURCE_TREE" =~ ^[0-9a-f]{40}$ ]] || die "SOURCE_TREE must be a 40-byte hexadecimal tree"
[[ ! -e "$SOURCE" ]] || die "source root is not fresh: $SOURCE"
[[ ! -e "$SCRATCH" ]] || die "scratch root is not fresh: $SCRATCH"
[[ ! -e "$TARGET" ]] || die "Cargo target root is not fresh: $TARGET"
[[ ! -e "$RECEIPTS" ]] || die "receipt root is not unique: $RECEIPTS"
[[ -z "${RUSTC_WRAPPER-}" && -z "${RUSTC_WORKSPACE_WRAPPER-}" ]] || die "Rust wrappers must be empty"

case "$SOURCE" in /private/tmp/basanos-doom-native-tool-preflight-repair-20260922-*) ;; *) die "SOURCE is outside the fresh-root namespace" ;; esac
case "$SCRATCH" in /private/tmp/basanos-doom-native-tool-preflight-repair-20260922-*) ;; *) die "SCRATCH is outside the fresh-root namespace" ;; esac
case "$TARGET" in /private/tmp/basanos-doom-native-tool-preflight-repair-20260922-*) ;; *) die "TARGET is outside the fresh-root namespace" ;; esac

mkdir -p "$RECEIPTS"
mkdir "$TARGET" "$SCRATCH"
git worktree add --detach "$SOURCE" "$SOURCE_COMMIT" >"$RECEIPTS/worktree.log" 2>&1
test "$(git -C "$SOURCE" rev-parse HEAD)" = "$SOURCE_COMMIT"
test "$(git -C "$SOURCE" rev-parse HEAD^{tree})" = "$SOURCE_TREE"
test -z "$(git -C "$SOURCE" status --porcelain --untracked-files=all)"

export CARGO_TARGET_DIR="$TARGET"
export DOOM_SCRATCH="$SCRATCH"
export DOOM_SBF_SOURCE_TREE="$SOURCE_TREE"

resolve_tool() {
  local selected="$1"
  local label="$2"
  if [[ "$selected" != /* ]]; then
    selected=$(command -v "$selected") || die "$label is not on PATH: $1"
  fi
  [[ "$selected" == /* ]] || die "$label did not resolve to an absolute executable: $selected"
  test -x "$selected" || die "$label is not executable: $selected"
  printf '%s\n' "$selected"
}

RUSTC=$(resolve_tool "${RUSTC:-rustc}" RUSTC)
CARGO=$(resolve_tool "${CARGO:-cargo}" CARGO)
CC=$(resolve_tool "${CC:-cc}" CC)
AR=$(resolve_tool "${AR:-ar}" AR)
export RUSTC CARGO CC AR

# Use the same selected-tool implementation as the independent expected
# profile before the host build or Cargo can consume significant resources.
TOOL_PREFLIGHT="$RECEIPTS/tool-preflight.json"
PYTHONPATH="$SOURCE:$SOURCE/src" "$PYTHON_BIN" \
  "$SOURCE/scripts/doom_native_expected_profile.py" --preflight-tools --output "$TOOL_PREFLIGHT" \
  >"$RECEIPTS/tool-preflight.log" 2>&1

{
  for name in \
    SOURCE_COMMIT SOURCE_TREE SOURCE SCRATCH TARGET RECEIPTS CAPTURE RESOURCE FINAL_DOCS_COMMIT FINAL_DOCS_TREE PYTHON_BIN CARGO_TARGET_DIR \
    DOOM_SCRATCH DOOM_SBF_SOURCE_TREE RUSTC CARGO CC AR PROFILE OPT_LEVEL DEBUG \
    DEBUG_ASSERTIONS CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS CARGO_PROFILE_RELEASE_LTO \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CFLAGS \
    CPPFLAGS C_INCLUDE_PATH RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTC_LINKER \
    CARGO_FEATURE_CTX_ENGINE CARGO_FEATURE_STATIC_WRITE_PROBE \
    CARGO_FEATURE_ACCOUNT_ADDRESS_PROBE CARGO_FEATURE_ZERO_BSS_PROBE \
    CARGO_FEATURE_DIAGNOSTIC_PROFILE CARGO_FEATURE_SIM_TRANSPORT \
    CARGO_FEATURE_METERED_SWEEP; do
    printf '%s=%q\n' "$name" "${!name-}"
  done
} >"$RECEIPTS/provenance-environment.txt"

printf '%q ' DOOM_SCRATCH="$SCRATCH" bash "$SOURCE/stunts/doom-host/build.sh" >"$RECEIPTS/host-build.argv"
DOOM_SCRATCH="$SCRATCH" bash "$SOURCE/stunts/doom-host/build.sh" >"$RECEIPTS/host-build.log" 2>&1

# This is intentionally before Cargo: the expected profile is assembled from
# the reviewed template and successful probes, never from a produced manifest.
EXPECTED="$RECEIPTS/expected-build-configuration.json"
PYTHONPATH="$SOURCE:$SOURCE/src" "$PYTHON_BIN" \
  "$SOURCE/scripts/doom_native_expected_profile.py" \
  --repo "$SOURCE" --source-commit "$SOURCE_COMMIT" \
  --upstream-tree "$SCRATCH/doom-src" --output "$EXPECTED" \
  >"$RECEIPTS/expected-profile.log" 2>&1
sha256_file "$EXPECTED" >"$RECEIPTS/expected-build-configuration.sha256"
wc -c <"$EXPECTED" | tr -d ' ' >"$RECEIPTS/expected-build-configuration.bytes"
EXPECTED_SHA256=$(<"$RECEIPTS/expected-build-configuration.sha256")
EXPECTED_BYTES=$(<"$RECEIPTS/expected-build-configuration.bytes")

printf '%q ' "$CARGO" build --locked --manifest-path "$SOURCE/stunts/doom-sbf/driver/Cargo.toml" \
  --release --features ctx-engine --bin sweep_native --bin fresh_resume_probe >"$RECEIPTS/cargo-build.argv"
"$CARGO" build --locked --manifest-path "$SOURCE/stunts/doom-sbf/driver/Cargo.toml" \
  --release --features ctx-engine --bin sweep_native --bin fresh_resume_probe \
  >"$RECEIPTS/cargo-build.log" 2>&1

MANIFESTS=()
while IFS= read -r -d '' candidate; do
  MANIFESTS+=("$candidate")
done < <(find "$TARGET/release/build" -type f -name doom_ex2_source_manifest.json -print0)
test "${#MANIFESTS[@]}" -eq 1 || die "expected exactly one fresh source manifest"
MANIFEST=${MANIFESTS[0]}
SWEEP="$RECEIPTS/sweep_native"
PROBE="$RECEIPTS/fresh_resume_probe"
cp "$TARGET/release/sweep_native" "$SWEEP"
cp "$TARGET/release/fresh_resume_probe" "$PROBE"

BUILD_DESCRIPTOR="$RECEIPTS/source-build-descriptor.json"
PYTHONPATH="$SOURCE:$SOURCE/src" "$PYTHON_BIN" \
  "$SOURCE/scripts/doom_native_provenance_descriptor.py" --stage source-build \
  --repo "$SOURCE" --source-commit "$SOURCE_COMMIT" \
  --upstream-tree "$SCRATCH/doom-src" --manifest "$MANIFEST" \
  --expected-build-configuration "$EXPECTED" \
  --sweep-binary "$SWEEP" --probe-binary "$PROBE" --output "$BUILD_DESCRIPTOR" \
  >"$RECEIPTS/source-build-descriptor.log" 2>&1
BUILD_SHA256=$(sha256_file "$BUILD_DESCRIPTOR")
BUILD_BYTES=$(wc -c <"$BUILD_DESCRIPTOR" | tr -d ' ')
printf '%s\n' "$BUILD_SHA256" >"$RECEIPTS/source-build-descriptor.sha256"
printf '%s\n' "$BUILD_BYTES" >"$RECEIPTS/source-build-descriptor.bytes"

ORACLE="$RECEIPTS/oracle"
printf '%q ' "$SWEEP" "$CAPTURE" "$RESOURCE" "$ORACLE" --tics 1000 \
  --build-descriptor "$BUILD_DESCRIPTOR" --build-descriptor-sha256 "$BUILD_SHA256" \
  --build-descriptor-bytes "$BUILD_BYTES" >"$RECEIPTS/sweep_native.argv"
"$SWEEP" "$CAPTURE" "$RESOURCE" "$ORACLE" --tics 1000 \
  --build-descriptor "$BUILD_DESCRIPTOR" --build-descriptor-sha256 "$BUILD_SHA256" \
  --build-descriptor-bytes "$BUILD_BYTES" >"$RECEIPTS/sweep_native.log" 2>&1

FINAL_DESCRIPTOR="$RECEIPTS/final-acceptance-descriptor.json"
PYTHONPATH="$SOURCE:$SOURCE/src" "$PYTHON_BIN" \
  "$SOURCE/scripts/doom_native_provenance_descriptor.py" --stage acceptance \
  --build-descriptor "$BUILD_DESCRIPTOR" --build-descriptor-sha256 "$BUILD_SHA256" \
  --build-descriptor-bytes "$BUILD_BYTES" --oracle-directory "$ORACLE" \
  --capture "$CAPTURE" --resource "$RESOURCE" --sweep-binary "$SWEEP" \
  --probe-binary "$PROBE" --output "$FINAL_DESCRIPTOR" \
  >"$RECEIPTS/final-descriptor.log" 2>&1
FINAL_SHA256=$(sha256_file "$FINAL_DESCRIPTOR")
FINAL_BYTES=$(wc -c <"$FINAL_DESCRIPTOR" | tr -d ' ')
printf '%s\n' "$FINAL_SHA256" >"$RECEIPTS/final-descriptor.sha256"
printf '%s\n' "$FINAL_BYTES" >"$RECEIPTS/final-descriptor.bytes"

printf '%q ' "$PROBE" "$CAPTURE" "$RESOURCE" "$ORACLE" "$RECEIPTS/fresh-resume-probe" \
  --source-root "$SOURCE" --final-descriptor "$FINAL_DESCRIPTOR" \
  --final-descriptor-sha256 "$FINAL_SHA256" --final-descriptor-bytes "$FINAL_BYTES" \
  >"$RECEIPTS/fresh_resume_probe.argv"
"$PROBE" "$CAPTURE" "$RESOURCE" "$ORACLE" "$RECEIPTS/fresh-resume-probe" \
  --source-root "$SOURCE" --final-descriptor "$FINAL_DESCRIPTOR" \
  --final-descriptor-sha256 "$FINAL_SHA256" --final-descriptor-bytes "$FINAL_BYTES" \
  >"$RECEIPTS/fresh_resume_probe.log" 2>&1

export BASANOS_DOOM_FRESH_RESUME_ACCEPTANCE=1
export BASANOS_DOOM_FRESH_RESUME_PROBE="$PROBE"
export BASANOS_DOOM_CAPTURE="$CAPTURE"
export BASANOS_DOOM_RESOURCE="$RESOURCE"
export BASANOS_DOOM_RESUME_ORACLE="$ORACLE"
export BASANOS_DOOM_FINAL_DESCRIPTOR="$FINAL_DESCRIPTOR"
export BASANOS_DOOM_FINAL_DESCRIPTOR_SHA256="$FINAL_SHA256"
export BASANOS_DOOM_FINAL_DESCRIPTOR_BYTES="$FINAL_BYTES"

FOCUSED_XML="$RECEIPTS/focused.xml"
set +e
PYTHONPATH="$SOURCE:$SOURCE/src" "$PYTHON_BIN" -m pytest -q \
  --junitxml="$FOCUSED_XML" "$SOURCE/tests/test_doom_review_fixes.py" -k fresh_resume \
  >"$RECEIPTS/focused.log" 2>&1
PYTEST_STATUS=$?
set -e
test "$PYTEST_STATUS" -eq 0 || die "focused pytest failed with exit $PYTEST_STATUS"
test "$("$PYTHON_BIN" -c 'import sys,xml.etree.ElementTree as E; print(sum(int(x.attrib.get("skipped","0")) for x in E.parse(sys.argv[1]).getroot().iter("testsuite")))' "$FOCUSED_XML")" -eq 0 || die "focused pytest reported skips"

git -C "$SOURCE" rev-parse HEAD >"$RECEIPTS/code-commit.txt"
git -C "$SOURCE" rev-parse HEAD^{tree} >"$RECEIPTS/code-tree.txt"
cp "$RECEIPTS/code-commit.txt" "$RECEIPTS/tested-code-head.txt"
cp "$RECEIPTS/code-tree.txt" "$RECEIPTS/tested-code-tree.txt"
if [[ -n "${FINAL_DOCS_COMMIT-}" || -n "${FINAL_DOCS_TREE-}" ]]; then
  [[ "${FINAL_DOCS_COMMIT-}" =~ ^[0-9a-f]{40}$ ]] || die "FINAL_DOCS_COMMIT must be a 40-byte hexadecimal commit"
  [[ "${FINAL_DOCS_TREE-}" =~ ^[0-9a-f]{40}$ ]] || die "FINAL_DOCS_TREE must be a 40-byte hexadecimal tree"
  printf '%s\n' "$FINAL_DOCS_COMMIT" >"$RECEIPTS/final-docs-head.txt"
  printf '%s\n' "$FINAL_DOCS_TREE" >"$RECEIPTS/final-docs-tree.txt"
fi
git -C "$SOURCE" ls-files -s -- \
  stunts/doom-sbf/build.rs stunts/doom-sbf/driver/src/bin/fresh_resume_probe.rs \
  stunts/doom-sbf/driver/src/bin/sweep_native.rs scripts/doom_native_provenance_descriptor.py \
  scripts/doom_native_expected_profile.py scripts/doom_native_provenance_acceptance.sh \
  stunts/doom-sbf/doom-native-host-profile-v2.json stunts/doom-sbf/src/native_archiver.rs \
  tests/test_doom_review_fixes.py \
  | sha256_stdin >"$RECEIPTS/owned-files.sha256"
date -u +%Y-%m-%dT%H:%M:%SZ >"$RECEIPTS/finished-at.txt"
"$PYTHON_BIN" - "$RECEIPTS" >"$RECEIPTS/checksums.sha256" <<'PY'
import hashlib
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve()
for path in sorted(path for path in root.rglob("*") if path.is_file() and path.name != "checksums.sha256"):
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    print(f"{digest}  {path.relative_to(root)}")
PY

# All retained binaries and descriptors are in RECEIPTS before disposable
# roots are removed. The explicit namespace checks above keep cleanup narrow.
rm -rf "$TARGET" "$SCRATCH"
git worktree remove --force "$SOURCE"
echo "Doom native provenance acceptance passed; receipts=$RECEIPTS expected_profile_sha256=$EXPECTED_SHA256 expected_profile_bytes=$EXPECTED_BYTES"
