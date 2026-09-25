# Doom on Fogo

A public page that paints Doom frames that were rendered on chain, and signs
the player's inputs with a throwaway key so a relayer can attribute them.

Nothing in this directory sends a transaction. The page reads; that is all it
does on chain.

## Running it

From the repository root:

```sh
python -m http.server 8000
```

Then open <http://localhost:8000/web/doom-play/>.

Query parameters: `?net=testnet` picks a network from `network.js`, and
`?relayer=http://localhost:8787` prefills the relayer URL.

## The files

| File | What it is |
|---|---|
| `network.js` | The only place network-specific values live. Testnet is filled in; mainnet is placeholders that throw until filled. |
| `strip.js` | The strip account decoder. Layout transcribed from `stunts/doom-sbf/src/render_snapshot_v3.rs` and `doom_sbf_resource_ri2.c`. |
| `rpc.js` | Read-only JSON-RPC client, plus the WAD reader that pulls PLAYPAL out of the WAD accounts by offset. |
| `input.js` | The signed input message. Imported by the relayer too, so the two cannot drift. |
| `burner.js` | The throwaway ed25519 keypair, in the tab, in localStorage. |
| `app.js` | Page wiring: the read loop, the canvas, the input loop, the session. |
| `test/` | Offline tests, plus one live cross-check that is not part of the default run. |

## Tests

```sh
cd web/doom-play
node --test test/test_strip.mjs test/test_input.mjs test/test_palette.mjs
```

`test/fixtures/frame-cursor-6016.bin.gz` holds eight real strip accounts read
from testnet, so the decoder is tested against bytes the chain actually holds.
The expected frame hash is in `test/fixtures/frame-cursor-6016.json`.

The live cross-check is separate, because it reads the network:

```sh
node test/test_strip_live.mjs /path/to/out/runs/doom-r8i-20260925
```

It decodes the R8i strips from testnet and compares the result against the
readback receipts, including a byte-for-byte comparison with the framebuffer
file that `scripts/doom_r8i_readback.py`'s pipeline wrote.

## How a frame is put together

One frame is eight strip accounts, 8,128 bytes each: a 128-byte header and
8,000 palette indices. The frame is cut into eight vertical bands of 40
columns; within a band the pixels are row-major from the top left. So
`pixels[y * 40 + x]` in strip *k* is frame pixel `(40k + x, y)`, and eight
strips reassemble to 64,000 bytes, which is 320x200 exactly.

The parent snapshot account does not commit to the strips. Each strip commits
to the snapshot one way, in its own header at offset 32. The viewer therefore
groups strips by `(cursor, gametic, snapshot_hash, state_root)` and paints only
when all eight agree, which is what stops a snapshot landing between two strip
reads from producing a torn frame. A torn or partial read is never painted; the
last good frame stays on screen.

Each strip also carries its own SHA-256 over the strip id, cursor, gametic,
snapshot hash and pixels. The viewer recomputes it and refuses a strip whose
digest does not match.

## Where the palette comes from

Strip bytes are final PLAYPAL indices, not texture and light pairs: lighting
and the damage flash are already applied. The 256-colour palette is not in any
header, so the page reads it out of DOOM1.WAD, which the program keeps in five
1 MiB accounts. It reads three small slices by offset (the 12-byte WAD header,
the lump directory, then the 768 bytes of PLAYPAL) rather than pulling 4 MiB,
and caches the palette in localStorage against the WAD's pinned SHA-256.

Palette 0 is used because the on-chain profile is shareware, where vanilla
selects palette 0, and `I_SetPalette` is stubbed out, so nothing on chain can
select another.

## The burner key

The page makes an ed25519 keypair with WebCrypto on first load and keeps it in
localStorage. It signs input batches and nothing else. There is no wallet
extension and no connect button.

Anyone can read that key out of localStorage, and anyone who has it can sign
batches as that player. That is deliberate: the key signs inputs only, the
relayer pays every fee, and the amounts are tiny. Do not put anything of value
behind it.

## The input message

64-byte header plus four bytes per command. See `input.js` for the field
offsets and `docs/design/doom-relayer-api-2026-09-25.md` for the protocol.

The four command bytes are the engine's own demo `ticcmd`: forwardmove and
sidemove as signed bytes, angleturn as a signed int16 the program reads as
`(unsigned char) << 8`, and the `BT_*` button bits. A held key produces a
per-tic command stream, and turn is contributed on every fourth tic because
vanilla Doom accumulates angleturn over four.

## What this page is pointed at

The `testnet` entry points at the R8i deployment from 2026-09-25. Its context
spent all 6,016 of its input slots and the deployed image refuses
extend-capacity, so it is a spectator target: good for reading frames, not for
hosting a session. To play, point RELAYER at a relayer holding a fresh context.
