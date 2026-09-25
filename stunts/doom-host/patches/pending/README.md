# Pending render patches

Not applied by `build.sh` (it only reads `patches/*.patch`). They belong to the strip-bounded render work in
`docs/design/doom-render-cost-2026-09-25.md`, which does not yet pass the 8/8 pixel gate: 0018 permutes pixels in
`R_DrawPlanes`, and 0019 changes the fuzz pattern. Move a patch back up a level only with a passing gate, and renumber
it after 0017 (live-input controller cells, branch `fadeno/doom-r8l-live-input`).
