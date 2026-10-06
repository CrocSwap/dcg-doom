# Doom on the current DCG runtime, 1,000 frames on Fogo testnet (2026-10-05)

**Status: measured.** This is alpha exit criterion 7: Doom on DCG at
3.0 frames/s or better, sustained over at least 1,000 frames, on Doom's own
program built on the reviewed DCG runtime.

## What changed

- **DCG pin:** Doom is pinned to DCG main `4a7faf4`, runtime
  `dcg-runtime 0.1.0`. That runtime includes the sessions v3 review fixes,
  rejectable sessions, ring streams, the kernel kit and the runtime marker.
- **Rejecting instead of wedging.** The kernel declares `REJECTS_INPUT`.
  The engine's pre-tic guard refuses two kinds of command before any state
  changes, and the kernel now rejects both: a command with the special bit
  (232), and USE while a player is dead (295, a level restart SIM/2 does not
  run). Before, any refusal was fatal to the session; the 10-05 run stopped
  for good at tic 5,310 on USE-while-dead.
- **Session options:** sessions open with the ring stream and rejectable
  features, using the v3-r2 creator forms.
- **ProgramTests** (native ProgramTest, SBF image `b65854a3…`):
  - **Full lifecycle:** passes (654 s). Sim roots match the native oracle,
    renders match, and adversarial inputs are refused.
  - **Lanes:** passes. The golden frame at 1,000 matches. A special-bit
    command is rejected (11,022 CU): it is consumed, and the stream records
    its sequence and code. The session then advances.
- **Lanes driver, priority windows.** A step's priority is
  `step * (n - i) + step * n * (ceiling - epoch)`. The gap between a frame's
  steps is therefore about `1/(n*(ceiling - epoch))` of the price. Measured on
  Tokyo:
  - with ceiling = frames + 1, a 1,000-frame run's early frames had a gap
    near 0.03%. The leader reordered their second advance after the capture,
    so every frame paid a repair (0.69 frames/s) until the gap passed about
    0.04% near frame 200. This was seen on both 10-05 1,000-frame runs;
  - a 250-frame run (0.13% gap) had no repairs.

  Prices now restart every 250 frames, and the first commit of a window waits
  for the previous commit to land. Lowering the absolute price alone did not
  help (tried: the same slow start).

## Setup

- **Program:** `2uHBWbYEzGnjtik4fibo3eqipmgAY7pKUSL8Yp98j9vJ`, a fresh Doom
  testnet program (owner, 10-05). Image `b65854a375845e2c…`, 944,200 bytes;
  `dcg verify` matches the image and the runtime marker.
- **Session:** `AUHwkbkW…`, 3 lanes, features 3 (ring and rejectable).
- **Resource source:** the existing `G9rRwB…` account, byte-identical to the
  packed resource (sha256 `d9cb7108…`), which unblocked provisioning.
  Provisioning ran from Tokyo: 552 steps in 265 s, under a 600 s cap.
- **Measuring host:** `basanos-tokyo-2`. It was stopped afterwards; it had
  been left running about 28 days.
- **Receipts:** `out/runs/doom-current-dcg-testnet-2026-10-05/`, which holds
  the stage ledger, logs, run JSONs and receipt.

## Measured

| Run | Frames | Seconds | Frames/s | Repairs | Cursor |
|---|---:|---:|---:|---:|---|
| smoke | 30/30 | 9.1 | 3.30 | 0 | 0 → 240 |
| 1,000, ceiling = frames + 1 | 1,000/1,000 | 524.7 | 1.91 | 201 (frames 0–200) | 240 → 8,240 |
| 250 | 250/250 | 69.4 | 3.61 | 0 | 8,240 → 10,240 |
| **1,000, 250-frame price windows** | **1,000/1,000** | **262.4** | **3.81** | **0** | 10,656 → 18,656 |

Per 250-frame window, the last run held 3.71, 3.85, 4.01 and 3.71 frames/s.
Frames are counted from verified commits and cross-checked against the
session cursor on chain. Over its life the session rejected 83 inputs, all
USE-while-dead (code 295; the last at sequence 18,494), and never wedged.

## Not checked

- Live frame hashes against the oracle. The oracle does not model rejected
  inputs; the ProgramTest checks the golden frame at 1,000 instead.
- The relayer and viewer path. Respawn after death is not supported by
  SIM/2, so the player stays dead after a rejected USE.
