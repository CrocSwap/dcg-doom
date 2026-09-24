# Doom decoupled render snapshot v1 — 2026-09-24

## Status and boundary

**Implemented as a local SBF program-test instruction; render-only use remains
open.** This is the selected
snapshot format for the clipped mode-1 renderer. It leaves the existing
DOOM-SIM/2 DEPOSIT, ADVANCE/3, ADVANCE/4, and SNAPSHOT/4 wires unchanged.
SNAPSHOT/4 remains a read-side consistency check. A separately versioned
`DOOM-RSN/1` instruction writes the render snapshot.

The render-only transaction measurement remains a gate. The captured DEMO1
state has a measured ST1 length of 59,604 bytes. The maximum account allocation
is 192,704 bytes: a 192-byte header, the bounded 126,976-byte ST1 image, and
the 65,536-byte MP1 image. Both account images are copied into the snapshot;
the ST1 and MP1 unused tails retain their canonical zero padding. This gives
the future renderer the actor mirror bound by the committed SIM2 state root.

## Snapshot account

One program-owned PDA per game context is derived with seeds
`[b"DOOM-RENDER-SNAPSHOT/1", context_pubkey]`. Provision it at exactly 192,704
data bytes before use. The account is reused for later snapshots. All integers
are unsigned little-endian; all reserved bytes and the unused ST1 and MP1 tails
are zero.

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic `DOOM-RSN` |
| 8 | 4 | version `1` |
| 12 | 4 | header bytes `192` |
| 16 | 4 | total account bytes `192704` |
| 20 | 4 | SIM2 committed cursor; frame id |
| 24 | 4 | ST1 gametic copied from offset 20 of ST1 |
| 28 | 4 | used ST1 length, range 64–126,976 |
| 32 | 32 | SIM2 state root over ST1 and the used MP1 prefix |
| 64 | 32 | WAD SHA-256, checked against MP1 bytes 36–67 |
| 96 | 32 | render resource ABI id |
| 128 | 32 | snapshot hash |
| 160 | 32 | reserved zero |
| 192 | 126,976 | ST1 image followed by zero padding |
| 127,168 | 65,536 | complete MP1 account image, including zero padding |

The snapshot hash is
`SHA256("DOOM-RENDER-SNAPSHOT/1\\0" || header[0..128] || zero32 ||
header[160..192] || padded_ST1 || MP1)`. The writer validates the current SIM2
meta/state/mirror binding and cursor/root with the existing `snapshot_check`,
checks the render snapshot PDA, validates ST1 and MP1 shapes, and copies both
account images. The renderer must separately compare the snapshot WAD digest
with the static RI2 resource's pinned WAD before rendering.

`DOOM-RSN/1` takes read locks on meta, ST1, and MP1, and a write lock on the
snapshot. ADVANCE never reads or writes the snapshot. Snapshot and ADVANCE
therefore serialize only through the existing meta/state/mirror accounts; no
new dependency is added to the tic path. RENDER reads only the snapshot and
static resource/WAD accounts, so ADVANCE does not wait on rendering. The
program-test path initializes the PDA through a payer-funded system CPI and
then writes it from the committed SIM2 accounts; its measured transaction CU
and rent are not yet recorded.

## RENDER and strip publication contract

The planned `DOOM-RND/1 RENDER(strip_id)` reads the snapshot, the static RI2
resource/WAD, and a strip-specific scratch account. It writes only the
corresponding strip account and scratch account. The required scratch size is
still open; it must be measured from the selected SBF renderer state before
provisioning. Each strip account is exactly 8,128 bytes: 128-byte header plus
8,000 palette-index pixels in column-major strip order.

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic `DOOM-RS1` |
| 8 | 4 | version `1` |
| 12 | 4 | header bytes `128` |
| 16 | 4 | strip id, 0–7 |
| 20 | 4 | frame id / SIM2 cursor |
| 24 | 4 | ST1 gametic |
| 28 | 4 | pixel bytes `8000` |
| 32 | 32 | snapshot hash |
| 64 | 32 | SIM2 state root |
| 96 | 32 | strip hash |
| 128 | 8,000 | PLAYPAL-indexed pixels |

The strip hash is
`SHA256("DOOM-RENDER-STRIP/1\\0" || strip_id || frame_id || gametic ||
snapshot_hash || pixels)`. The renderer must match the native engine's 8,000
bytes for every strip at the captured tic before this ABI is deployable.
The browser groups strips by `(frame_id, snapshot_hash, state_root, gametic)`
and paints only when all eight headers match; this prevents a snapshot update
from creating a torn frame.

The target schedule is one snapshot write and eight independently submitted
strip renders, or **9 transactions per frame (estimated)**. This remains
conditional on every complete RENDER transaction, including snapshot hash
verification, scratch/account checks, and strip writes, measuring below the
configured 1,400,000-CU comparison meter. The diagnostic renderer phase alone
measured 752,001 CU for strip 4; it does not establish that bound. No live
transaction limit, rent, or fee is claimed here.
