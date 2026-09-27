#!/bin/sh
# Reproducible build of the Doom SBF image from a named commit.
#
#   build-sbf-reproducible.sh <repo> <commit> <out-dir> <features> <target-root>
#
# Review F2 (2026-09-21): the DOOM-SIM/1 image was built from uncommitted
# (pre-rustfmt) source.  This wrapper only builds committed source:
#
# 1. checks out <commit> into a fixed detached worktree (the source path is
#    identical for every build, so no path-dependent identity can differ);
# 2. rebuilds the patched upstream scratch at a fixed path from the
#    committed stunts/doom-host/patches (pinned upstream a77dfb96, the same
#    ordered apply rules as stunts/doom-host/build.sh; no network: the
#    upstream checkout is copied from DOOM_UPSTREAM_SEED, a clone that
#    contains a77dfb96);
# 3. exports DOOM_SBF_SOURCE_TREE = git tree of `stunts/` at <commit>, which
#    build.rs records in the embedded source manifest;
# 4. builds twice in two fresh target directories and requires identical
#    image and manifest hashes;
# 5. scans both build logs for the SBF stack-frame errors cargo-build-sbf
#    prints and then ignores (exit 3 if any is present);
# 6. verifies every `repo/` manifest input against `git show <commit>:path`
#    and every `external/` input against the rebuilt scratch.
#
# Writes <out-dir>/{doom_sbf.so,doom_ex2_source_manifest.json,build-1.log,
# build-2.log,build-receipt.json}.  <target-root> holds the two target
# directories and is deleted on success.
set -eu

if [ $# -ne 5 ]; then
    echo "usage: $0 <repo> <commit> <out-dir> <features> <target-root>" >&2
    exit 2
fi
REPO=$1
COMMIT=$(git -C "$REPO" rev-parse --verify "$2^{commit}")
OUT=$3
FEATURES=$4
TARGET_ROOT=$5
GIT=${GIT:-git}
SDK=${SBF_SDK_PATH:-/private/tmp/basanos-sbf-sdk-v151-20260920}
SEED=${DOOM_UPSTREAM_SEED:?set DOOM_UPSTREAM_SEED to a clone containing a77dfb96}
UPSTREAM=a77dfb96cb91780ca334d0d4cfd86957558007e0
SRC=/private/tmp/doom-sbf-repro-src
SCRATCH=/private/tmp/doom-sbf-repro-scratch
LOCK=/private/tmp/doom-sbf-repro.lock

for path in "$OUT" "$TARGET_ROOT"; do
    case "$path" in /*) ;; *) echo "paths must be absolute: $path" >&2; exit 2 ;; esac
done
if [ -e "$OUT" ] && [ -n "$(ls -A "$OUT" 2>/dev/null)" ]; then
    echo "output directory is not empty: $OUT" >&2
    exit 2
fi
if ! mkdir "$LOCK" 2>/dev/null; then
    echo "reproducible Doom build already running: $LOCK" >&2
    exit 75
fi
cleanup() {
    "$GIT" -C "$REPO" worktree remove --force "$SRC" 2>/dev/null || true
    rm -rf "$SCRATCH" 2>/dev/null || true
    rmdir "$LOCK" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM
for path in "$SRC" "$SCRATCH"; do
    if [ -e "$path" ]; then
        echo "fixed build path already exists: $path" >&2
        exit 2
    fi
done

mkdir -p "$OUT" "$TARGET_ROOT"
"$GIT" -C "$REPO" worktree add --detach "$SRC" "$COMMIT" >/dev/null
TREE=$("$GIT" -C "$REPO" rev-parse "$COMMIT^{tree}")
STUNTS_TREE=$("$GIT" -C "$REPO" rev-parse "$COMMIT:stunts")

mkdir -p "$SCRATCH"
"$GIT" clone --quiet --no-checkout "$SEED" "$SCRATCH/doom-src"
"$GIT" -C "$SCRATCH/doom-src" checkout --quiet --detach "$UPSTREAM"
for patch in "$SRC"/stunts/doom-host/patches/*.patch; do
    case "$patch" in
        *0005-doom-reentrant-strips.patch|*0008-doom-captured-controller-tick.patch|*0010-doom-controller-ingress-causality.patch|*0011-doom-controlled-reentry-pair-ingress.patch|*0033-doom-frame-scoped-cache.patch)
            "$GIT" -C "$SCRATCH/doom-src" apply --unidiff-zero --recount "$patch" ;;
        *)
            "$GIT" -C "$SCRATCH/doom-src" apply "$patch" ;;
    esac
done 2>/dev/null

build() {
    n=$1
    target="$TARGET_ROOT/target-$n"
    rm -rf "$target"
    (
        cd "$SRC/stunts/doom-sbf"
        CARGO_TARGET_DIR="$target" DOOM_SCRATCH="$SCRATCH" SBF_SDK_PATH="$SDK" \
            DOOM_SBF_SOURCE_TREE="$STUNTS_TREE" RUSTC_WRAPPER= \
            cargo-build-sbf --tools-version v1.51 --skip-tools-install --features "$FEATURES"
    ) >"$OUT/build-$n.log" 2>&1 || { echo "build $n failed; see $OUT/build-$n.log" >&2; exit 1; }
    if grep -E "Stack offset of|overwrites values in the frame|overflows the maximum allowed frame space" "$OUT/build-$n.log" >/dev/null; then
        echo "build $n reported an SBF stack-frame overflow" >&2
        exit 3
    fi
    manifest=$(find "$target/sbpf-solana-solana/release/build" -name doom_ex2_source_manifest.json -path '*doom-sbf-*' | head -1)
    cp "$target/deploy/doom_sbf.so" "$OUT/doom_sbf.$n.so"
    cp "$manifest" "$OUT/doom_ex2_source_manifest.$n.json"
}
build 1
build 2
IMAGE1=$(shasum -a 256 "$OUT/doom_sbf.1.so" | awk '{print $1}')
IMAGE2=$(shasum -a 256 "$OUT/doom_sbf.2.so" | awk '{print $1}')
MANIFEST1=$(shasum -a 256 "$OUT/doom_ex2_source_manifest.1.json" | awk '{print $1}')
MANIFEST2=$(shasum -a 256 "$OUT/doom_ex2_source_manifest.2.json" | awk '{print $1}')
if [ "$IMAGE1" != "$IMAGE2" ] || [ "$MANIFEST1" != "$MANIFEST2" ]; then
    echo "non-reproducible: image $IMAGE1 vs $IMAGE2, manifest $MANIFEST1 vs $MANIFEST2" >&2
    exit 4
fi
mv "$OUT/doom_sbf.1.so" "$OUT/doom_sbf.so"
mv "$OUT/doom_ex2_source_manifest.1.json" "$OUT/doom_ex2_source_manifest.json"
rm -f "$OUT/doom_sbf.2.so" "$OUT/doom_ex2_source_manifest.2.json"

# Every manifest input must be committed (repo/) or come from the scratch
# rebuilt from committed patches (external/).
python3 - "$OUT/doom_ex2_source_manifest.json" "$REPO" "$COMMIT" "$SCRATCH/doom-src/linuxdoom-1.10" "$OUT/input-verification.json" <<'PY'
import hashlib, json, subprocess, sys
manifest, repo, commit, scratch, out = sys.argv[1:]
data = json.load(open(manifest))
bad, repo_count, external_count = [], 0, 0
for item in data["inputs"]:
    path = item["path"]
    if path.startswith("repo/"):
        blob = subprocess.run(["git", "-C", repo, "show", f"{commit}:{path[5:]}"], capture_output=True).stdout
        repo_count += 1
    elif path.startswith("external/"):
        blob = open(f"{scratch}/{path[9:]}", "rb").read()
        external_count += 1
    else:
        bad.append({"path": path, "reason": "unknown prefix"}); continue
    if hashlib.sha256(blob).hexdigest() != item["sha256"] or len(blob) != item["bytes"]:
        bad.append({"path": path, "reason": "sha/bytes mismatch"})
json.dump({"repo_inputs": repo_count, "external_inputs": external_count, "mismatches": bad}, open(out, "w"), indent=2)
sys.exit(1 if bad else 0)
PY

SIZE=$(wc -c < "$OUT/doom_sbf.so" | tr -d ' ')
cat >"$OUT/build-receipt.json" <<EOF
{
  "commit": "$COMMIT",
  "tree": "$TREE",
  "stunts_tree": "$STUNTS_TREE",
  "features": "$FEATURES",
  "image_sha256": "$IMAGE1",
  "image_bytes": $SIZE,
  "manifest_sha256": "$MANIFEST1",
  "double_build_equal": true,
  "stack_frame_errors": 0,
  "sbf_sdk": "$SDK",
  "upstream": "$UPSTREAM",
  "source_path": "$SRC",
  "scratch_path": "$SCRATCH"
}
EOF
rm -rf "$TARGET_ROOT"
echo "reproducible Doom SBF image $IMAGE1 ($SIZE bytes) manifest $MANIFEST1 from $COMMIT"
