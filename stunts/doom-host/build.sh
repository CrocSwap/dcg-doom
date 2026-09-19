#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
# The scratch area holds the fetched upstream source and the WAD; neither may
# enter Git.  DOOM_SCRATCH lets a later session point at its own scratchpad
# instead of the one that first recorded this path.
scratch=${DOOM_SCRATCH:-/private/tmp/claude-501/-Users-colkitt-sith-toys-crypto-basanos/e8f2923b-4704-46b4-a218-5d8564c5ea3f/scratchpad}
src="$scratch/doom-src"
wad_dir="$scratch/doom-wad"
wad="$wad_dir/share/doom/doom1.wad"
wad_url=https://cdn.netbsd.org/pub/pkgsrc/packages/NetBSD/aarch64/10.0/All/doom1-1.9.tgz
wad_hash=1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771

if [ ! -d "$src/.git" ]; then
  git clone https://github.com/id-Software/DOOM "$src"
fi
git -C "$src" fetch --quiet origin a77dfb96cb91780ca334d0d4cfd86957558007e0
git -C "$src" checkout --detach --force a77dfb96cb91780ca334d0d4cfd86957558007e0
# The patch creates this host-only directory; remove only that prior generated
# path so repeat builds apply the exact same patch to the pinned checkout.
git -C "$src" clean -fdx -- linuxdoom-1.10/headless linuxdoom-1.10/doom_wrap.h
for patch in "$root"/stunts/doom-host/patches/*.patch; do
  case "$patch" in
    *0005-doom-reentrant-strips.patch|*0008-doom-captured-controller-tick.patch)
      git -C "$src" apply --check --unidiff-zero --recount "$patch"
      git -C "$src" apply --unidiff-zero --recount "$patch"
      ;;
    *)
      git -C "$src" apply --check "$patch"
      git -C "$src" apply "$patch"
      ;;
  esac
done

mkdir -p "$wad_dir"
if [ ! -f "$wad" ]; then
  curl --fail --location --retry 2 --output "$scratch/doom1-1.9.tgz" "$wad_url"
  tar -xzf "$scratch/doom1-1.9.tgz" -C "$wad_dir"
fi
actual_hash=$(sha256sum "$wad" | awk '{print $1}')
actual_size=$(wc -c < "$wad" | tr -d ' ')
if [ "$actual_hash" != "$wad_hash" ] || [ "$actual_size" != 4196020 ]; then
  echo "refusing WAD: sha256=$actual_hash bytes=$actual_size" >&2
  exit 1
fi

make -C "$src/linuxdoom-1.10/headless" clean all
printf '%s\n' "$src/linuxdoom-1.10/doom-headless"
