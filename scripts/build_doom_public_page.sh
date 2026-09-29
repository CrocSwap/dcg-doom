#!/usr/bin/env bash
# Assemble the public page and the shared input/frame decoder modules.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$REPO/out/public/doom-public-site}
mkdir -p "$OUT/doom-public" "$OUT/doom-play"
cp "$REPO/web/doom-public/index.html" "$REPO/web/doom-public/public.css" "$REPO/web/doom-public/public.js" "$OUT/doom-public/"
cp "$REPO/web/doom-play/burner.js" "$REPO/web/doom-play/input.js" \
  "$REPO/web/doom-play/push.js" "$REPO/web/doom-play/strip.js" "$OUT/doom-play/"
echo "Built public page at $OUT/doom-public/index.html"
