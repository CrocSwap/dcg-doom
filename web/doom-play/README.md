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

## Playing

With no relayer the page spectates: it reads frames and shows them, which is
what the R8i deployment supports today. To play, start a relayer and press
JOIN:

```sh
node stunts/doom-relayer/server.mjs --mode mock --port 8787 --slots 4
```

In mock mode the relayer has no chain accounts, so the page falls back to the
relayer's copy of the same eight strip accounts when the network read fails.
The bytes go through the same decoder either way.

A page reload re-attaches to the session it already had rather than taking a
second slot, because the burner key in localStorage is the same one.

Keys: arrows or WASD to move and turn, Q and E to strafe, Space or F to fire,
R to use, Shift to run. On a phone there is a pad. Turn is contributed on every
fourth tic, as vanilla Doom accumulates it, so a held key turns at the rate a
real keyboard would.


## The files

| File | What it is |
|---|---|
| `network.js` | The only place network-specific values live. Testnet is filled in; mainnet is placeholders that throw until filled. |
| `strip.js` | The strip account decoder. Layout transcribed from `stunts/doom-sbf/src/render_snapshot_v3.rs` and `doom_sbf_resource_ri2.c`. |
| `rpc.js` | Read-only JSON-RPC client, plus the WAD reader that pulls PLAYPAL out of the WAD accounts by offset. |
| `input.js` | The signed input message. Imported by the relayer too, so the two cannot drift. |
| `burner.js` | The throwaway ed25519 keypair, in the tab, in localStorage. |
| `push.js` | The frame stream: SSE subscription, in-order painting, the chain cross-check. |
| `proof-frame-push.mjs` | The proof driver: painted rate, torn reads, per-press latency, over CDP. |
| `app.js` | Page wiring: the read loop, the canvas, the input loop, the session. |
| `test/` | Offline tests, plus one live cross-check that is not part of the default run. |

## Tests

```sh
cd web/doom-play
node --test test/test_strip.mjs test/test_input.mjs test/test_palette.mjs test/test_push.mjs
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

## Where the frames come from

A page with a relayer to push from is sent each complete frame and never looks
for one. `?frame-source=push` (the default) subscribes to
`GET /v1/sessions/:id/frames`, which is server-sent events: the relayer already
read every frame's eight strip accounts and checked every digest, and it forwards
those exact bytes. `?frame-source=read` is the old path, this page polling the
chain itself, and it is still the default for a page with no session.

The push is not a shortcut past the checks. A pushed frame goes through the same
`decodeFrame` as a chain read: each strip's SHA-256 over its header and pixels is
recomputed, and the eight are required to agree on cursor, gametic, snapshot hash
and state root. Every twentieth frame the page also reads the same accounts from
the chain and compares, and a disagreement is raised rather than counted — the
one way this path can be wrong is quietly, so the check is not optional. It
reads the eight 128-byte strip headers rather than the whole accounts, because
the digests cover the pixels and 1,728 bytes finishes inside a frame interval
where 1,072,384 bytes does not; and it compares the chain's frame against the
page's own record of that frame, because the public node the page reads is about
three frames behind the relayer.

Measured through this page on 2026-09-26 (see
`docs/experiments/doom-frame-push-2026-09-26.md`): **1.78 frames/s painted over
60 s with 0 torn reads**, key-to-picture median **390 ms** over 12 presses with
the picture moving on all 12, and 9 of 9 chain cross-checks matching. The direct
read on the same page over the same period: 0.22 fps and 151 torn reads.

## The input panel

The panel under the chain readout is the answer to "I pressed a key and nothing
happened". It keeps one row per key press, newest first: the key, when it went
down and up, the tic the relayer stamped for that key state, and the fate of
that tic. A fate is named, not summarised, and a landed tic links the transaction
that carried it on the Fogo explorer.

The fates and where each comes from are in `inputlog.js`. Two of them are worth
knowing about:

* **missed the slot** — the producer deposited that tic *before* this write
  reached it, so the chain ran it with no keys in it. This is the input drop, and
  it is measured rather than inferred from a picture that did not change.
* **queued, never deposited** — offered above the producer's staging limit, and
  the pipeline stopped before the chain reached it.

The page sends ordered key-state transitions without a tic number. The relayer
assigns the earliest free tic it can deposit and returns that stamp. The
one-line summary above the list is pressed / landed / failed and the landed
ratio over resolved tics; the page does not estimate a delivery lead.

When the context is spent, the pipeline has stopped, or the session is gone, a
red banner replaces the numbers. A page whose job is to say whether keys are
landing cannot also be the page that quietly swallows them.

### Proven on chain, 2026-09-26 04:16-04:18 UTC

`node proof-input-panel.mjs <out-dir>` against context
`2mDH886tnFU6gNDLkZHSm29TE8UAZ6QQPWzXMAPoeXLN` on the R12 keys, through Chrome's
own key path, with the panel's `input_fate` ledger live (receipts in
`out/runs/doom-panel-proof-2026-09-26/`). Six presses of 1.4 s each, one row
per key, every one of them with at least one landed tic and a transaction link:

| key | tics offered | landed | missed the slot | links | angleturn the chain was given | button |
|---|---:|---:|---:|---:|---|---|
| forward | 42 | 42 | 0 | 11 | — | — |
| back | 28 | 28 | 0 | 8 | — | — |
| left | 41 | 38 | 3 | 10 | `0x0400` (+1024) | — |
| right | 51 | 50 | 1 | 13 | `0xfc00` (−1024) | — |
| fire | 51 | 47 | 4 | 13 | — | `0x01` BT_ATTACK |
| use | 53 | 53 | 0 | 14 | — | `0x02` BT_USE |

* **258 of 266 tics landed, 0 pending, landed ratio 0.970** over resolved tics.
  The relayer's own receipt for the same session says
  `input_command_tics_deposited: 233` of `input_tics_staged: 552` and
  `input_refusals: {}`; that is a different denominator (every tic the page
  offered, against every tic deposited) and both are reported rather than
  reconciled.
* **Left turns left, read off the chain rather than off a picture.** The panel
  holds the exact `commands` bytes the producer was given, and `left` deposits
  angleturn `0x0400` where `right` deposits `0xfc00`. Doom's `angleturn` is
  counter-clockwise positive, which is the whole of commit `49c282ebc`.
* Key-to-picture latency: **2 samples, 517 ms and 7,740 ms** — the panel only
  takes a sample when a frame paints while a key is down, and the page painted
  16 frames in the 74 s window (151 torn reads, counted, not raised). Thin.
* Input lead 17 tics = 616 ms at 27.6 tic/s, over a measured 361 ms POST round
  trip. The banner was correctly absent.

Two defects had to be fixed before that run could mean anything, and both were
in this panel rather than on the chain:

1. **`input_fate` was never published.** The ledger existed only in the
   end-of-session result, and `status` is answered from the progress dict, so a
   page was told `zero_deposited_through` with no runs and read every keystroke
   as missed. That is the defect the first proof run failed on, and the fix was
   committed without being re-proved — and, as it turned out, never even reached
   the relayer tree on Tokyo, which ran the unfixed `doom_r7_render_loop.py`.
   The relayer tree is rsynced from a worktree, so a fix in a commit is not a fix
   on the host; check the sha256 before believing a runbook.
2. **`render()` raised on its first line.** `bannerNode` was a free variable
   rather than a parameter, so every call threw
   `ReferenceError: bannerNode is not defined`: no row was ever drawn, the
   summary line never moved off "no keys pressed", the banner never appeared, and
   — because the read loop calls `render` after a paint — the same throw is what
   froze the picture at 10 painted frames. The eleven offline tests passed because
   the test file said, correctly as far as it went, that the drawing is
   `document` calls over the answers worth testing, and then never called it.
   There are now two tests that do, against a small DOM stub.

### What the relayer has to expose for it

Two fields on the session status, both of which ride on an answer the relayer
was already fetching, so neither costs a round trip:

| field | what it is |
|---|---|
| `input_fate.runs[]` | one row per DEPOSIT that carried a non-zero command: `seq`, `count`, the exact `commands` bytes, and the transaction `signature`. Only command-bearing runs are listed, and the list is bounded. |
| `input_fate.zero_deposited_through` | every sequence below this is deposited. One number instead of a run of zeros, and it is what turns a tic with no run into "missed the slot" rather than "queued". |

Plus `producer_next_cursor`, which is not the same number as
`producer_generated_cursor` and is the one a writer can honestly aim ahead of:
`generated_cursor` is the *end* of the batch the producer has already published,
so it runs `round(35 * 0.1)` = 4 tics ahead of the producer's real position.
And `pipeline`, which says whether keystrokes can still reach the engine at all.

`pipeline.stalled_seconds` is the **greater** of two clocks, and it has to be.
The producer publishes its own, which is the better number while the producer is
alive, but it is only refreshed when the producer calls `publish_progress`, so a
child that wedges completely freezes it — usually at 0, because the cursor had
just moved, and a frozen 0 reads as "not stalled". The relayer's own 32-byte
cursor probe is the second clock: it kept running at its full rate (4,174 samples
over 458 s, the last at 458.0 s) through the play session that stalled, so it can
see a cursor that has stopped without asking the child anything.
`pipeline.stalled_observed_by` says which side saw it.

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
