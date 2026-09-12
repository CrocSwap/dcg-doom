# Headless Doom host replay

Run `stunts/doom-host/build.sh`; it clones the pinned upstream source into the
mandated session scratchpad, applies the ordered patch set, fetches the NetBSD
`doom1-1.9.tgz` public mirror, and rejects any WAD except the
4,196,020-byte SHA-256-pinned v1.9 shareware asset.

The binary accepts `-headless -iwad PATH -demo DEMO1 -frames FILE`.  `DEMO1`
is the built-in shareware lump.  `--flip-forwardmove-tic 100` changes bit zero
of the first command byte in the private cached demo buffer; it never writes
the WAD.

`-state-hashes FILE` adds one `DOOM-ST1` v0 state digest per rendered tic;
`--s0 FILE` records the post-`load_level`, pre-first-tic state.  Use
`--save-at T --save FILE`, then a fresh invocation with `--load FILE` to
continue a replay; `--resave FILE` writes the decoded image without advancing.
`DOOM-ST1` is a fixed-header, little-endian image of mutable E1M1 state. Its
digest is `sha256("DOOM-ST1" || u32le(0) || wad_sha256 || image)`.

`frames.jsonl` has eight `strip_id` records (0–7) and one `strip_id:-1`
whole-frame record per gametic. Strip bytes are column-major: x advances over
the 40-column strip, y advances over each 200-pixel column. `DOOM-FB1` v0 is
`sha256("DOOM-FB1" || u32le(0) || wad_sha256 || u32le(gametic) ||
u32le(strip_id) || 8000 strip bytes)`. Whole records use the distinct
`DOOM-FB1-FRAME` domain, strip id 0, and the native row-major 64,000-byte
framebuffer.
