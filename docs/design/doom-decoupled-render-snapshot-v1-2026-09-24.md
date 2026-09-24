# Doom decoupled render snapshot — 2026-09-24

## Status and boundary

**Implemented and measured locally; NO-GO at the configured per-transaction
comparison.** Standalone DOOM-RSN/2 RENDER of strip 4 succeeded and matched
the native pixels, but consumed 3,871,073 CU against a 1,400,000-CU
comparison. This is the selected snapshot format for the clipped mode-1
renderer. It leaves the existing
DOOM-SIM/2 DEPOSIT, ADVANCE/3, ADVANCE/4, and SNAPSHOT/4 wires unchanged.
SNAPSHOT/4 remains a read-side consistency check. A separately versioned
`DOOM-RSN/2` instruction writes the render snapshot and handles
`RENDER(strip_id)`. It supersedes the initial
`DOOM-RSN/1` design recorded in M1342 because the implementation copies MP1
into the image so later render calls do not need live state accounts.

The captured DEMO1 state has a measured ST1 length of 59,604 bytes. The maximum account allocation
is 192,704 bytes: a 192-byte header, the bounded 126,976-byte ST1 image, and
the 65,536-byte MP1 image. Both account images are copied into the snapshot;
the ST1 and MP1 unused tails retain their canonical zero padding. This gives
the future renderer the actor mirror bound by the committed SIM2 state root.

## Snapshot account

One program-owned PDA per game context is derived with seeds
`[b"DOOM-RENDER-SNAPSHOT/2", context_pubkey]`. Provision it at exactly 192,704
data bytes before use. The account is reused for later snapshots. All integers
are unsigned little-endian; all reserved bytes and the unused ST1 and MP1 tails
are zero.

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic `DOOM-RSN` |
| 8 | 4 | version `2` |
| 12 | 4 | header bytes `192` |
| 16 | 4 | total account bytes `192704` |
| 20 | 4 | SIM2 committed cursor; frame id |
| 24 | 4 | ST1 gametic copied from offset 20 of ST1 |
| 28 | 4 | used ST1 length, range 64–126,976 |
| 32 | 32 | SIM2 state root over ST1 and the used MP1 prefix |
| 64 | 32 | WAD SHA-256, copied from MP1 bytes 36–67 and checked against MP1 on read |
| 96 | 32 | render resource ABI id |
| 128 | 32 | snapshot hash |
| 160 | 32 | reserved zero |
| 192 | 126,976 | ST1 image followed by zero padding |
| 127,168 | 65,536 | complete MP1 account image, including zero padding |

The snapshot hash is
`SHA256("DOOM-RENDER-SNAPSHOT/2\\0" || header[0..128] || zero32 ||
header[160..192] || padded_ST1 || MP1)`. The writer validates the current SIM2
meta/state/mirror binding and cursor/root with the existing `snapshot_check`,
checks the render snapshot PDA, validates ST1 and MP1 shapes, and copies both
account images. ST1 has no WAD digest field: the snapshot copies that digest
from MP1 bytes 36–67, and the renderer compares it with the static RI2
resource's pinned WAD before rendering.

`DOOM-RSN/2` takes read locks on meta, ST1, and MP1, and a write lock on the
snapshot. ADVANCE never reads or writes the snapshot. Snapshot and ADVANCE
therefore serialize only through the existing meta/state/mirror accounts; no
new dependency is added to the tic path. RENDER reads only the snapshot and
static resource/WAD accounts, so ADVANCE does not wait on rendering. The
program-test path initializes the PDA through a payer-funded system CPI and
then writes it from the committed SIM2 accounts; its measured transaction CU
and rent are not yet recorded.

## RENDER and strip publication contract

`DOOM-RSN/2 RENDER(strip_id)` reads the snapshot, static RI2 resource and five
WAD accounts, and a strip-specific scratch account. Account order starts with
the scratch (the legacy engine requires it at the pinned first-account data
address), followed by RI2, WAD[0..4], snapshot, and strip. It writes only the
corresponding strip account and scratch account. The implementation's scratch
account is `resource_sbf::CONTEXT_BYTES` = 9,734,128 bytes. At the design rent
rate of 6,960 lamports per byte, eight scratch accounts are **estimated** at
541.996 FOGO, exceeding the 400 FOGO cap before other accounts. The rent rate
and total are not measured on Fogo; this is a funding go/no-go that requires a
smaller scratch representation before testnet provisioning.

Each strip account is exactly 8,128 bytes: 128-byte header plus 8,000
palette-index pixels in row-major strip order. The request is 52 bytes: the
`DOOM-RSN` magic, version 2, operation 2, one-byte strip id, two reserved zero
bytes, frame cursor, and expected snapshot hash. Scratch and strip PDAs are
derived from the snapshot PDA and strip id, so different strip calls write
disjoint accounts. The renderer reconstructs the legacy engine workspace for
each call, does not run `G_Ticker`, and returns only the selected 40×200
pixels.

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
`SHA256("DOOM-RENDER-STRIP/1\\0" || u32le(strip_id) || u32le(frame_id) ||
u32le(gametic) || snapshot_hash || pixels)`. The renderer must match the native engine's 8,000
bytes for every strip at the captured tic before this ABI is deployable.
The browser groups strips by `(frame_id, snapshot_hash, state_root, gametic)`
and paints only when all eight headers match; this prevents a snapshot update
from creating a torn frame.

The designed schedule is one snapshot write and eight independently submitted
strip renders, or **9 transactions per frame (designed)**. The measured strip
4 RENDER consumes 3,871,073 CU, 2.76× the configured 1,400,000-CU comparison,
so this schedule fails that comparator. Dividing the measured strip CU by the
comparison meter gives **25 transaction equivalents/frame (estimated)** for
eight strips plus one snapshot write, assuming ideal splitting and one-meter
snapshot cost; the current RENDER ABI has no split operation, and a split
renderer would add reconstruction and instruction overhead. The local bank's
30,000,000-CU cap allowed the measured instruction to succeed; no live Fogo
transaction limit, rent, or fee is claimed here.
