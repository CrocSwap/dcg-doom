# Doom on DCG render lanes, Fogo testnet — 2026-10-04

**Status: measured (lanes slice 4, first pass).** Lanes work end to end on
testnet, and they take rendering off a frame's critical path. Throughput from
this host is bound by per-transaction confirmation latency on the serial
advance chain, not by compute or lanes. The 3.0 frames/s goal needs pipelined
sends of that chain (next step), not more lanes.

## Setup

- Owner approved a fresh Doom testnet program for this (2026-10-04), and a
  600 s stage-cap variance for the one-time deploy and provisioning.
- Image: Basanos main `5e1ea6ab0` (Doom pinned to DCG `6d8df99`, lanes slice
  1 + Doom lane hooks), `doom_dcg.so` SHA-256 `35d09650…`, program
  `9Uj6wmraGrWQuwjeirgveGX4HT2Kmfik3hwfin5G1kBR`.
- Session with 3 lanes; RPC `http://64.130.44.172:8899` only; client on the
  Mac (the Tokyo host stays stopped). Driver:
  `stunts/doom-dcg/lanes_testnet.py` (plan `lanes_plan.py`).
- Run directory: `out/runs/doom-lanes-testnet-2026-10-04/` (journals,
  `stage-ledger.jsonl`, logs). Keys under `out/chain/doom-lanes-testnet-2026-10-04/`.

## Provisioning (one-time)

- Deploy: 15 s.
- Provisioning as one serial chain was **killed at the 600 s cap** (751
  steps, ~0.8 s each). Rewritten as a dependency graph (resource, state and
  lane chains in parallel; initialization phases packed by measured CU into
  28 transactions; grows batched 30, lane grows 10 per transaction after
  "instruction trace" and compute failures): **552 steps, 212 s, pass**.

## Frames (measured, testnet, journal confirmation times)

| Lanes | Receipt hook | Frames | Seconds per frame | Frames/s |
|---:|---|---:|---:|---:|
| 1 | on | 5 | ~17 | 0.059 |
| 2 | on | 6 | 13.6 | 0.074 |
| 3 | on (fast mode) | 6 | 13.5 | 0.074 |
| 1 | off (fast mode) | 6 | 11.8 | 0.085 |
| 3 | off (fast mode) | 8 | 9.3 | 0.107 |

- The driver's per-commit receipt hook fetches every transaction's metadata
  inside the sequencer loop and stalls all chains ~4–5 s per frame; it is
  off for throughput runs.
- Every dependent step costs ~0.93 s from send to the next send (~0.37 s to
  confirm plus sequencer polling) from this host.
- A frame's serial chain is 10 transactions (input batch, 8 advances, one
  capture transaction): ~9.3 s. With 3 lanes, frames commit at exactly that
  cadence; the 11 render transactions per frame run on the lanes in
  parallel. With 1 lane the next capture waits for the previous commit,
  adding the render chain (11.8 s).
- Measured CU (ProgramTest, same image): capture ~350k CU in one transaction;
  strips 615k–776k each; commit 22k.

## What this establishes, and what it does not

- Established: lanes run on a live chain; a lane renders while the session
  advances and other lanes capture; commits are newest-only; frames commit at
  the serial-chain cadence with 3 lanes.
- Not established: 3.0 frames/s. The earlier 1.66 frames/s (Tokyo, 10-01)
  pipelined dependent transactions (OrderedLane) from a host 1.6 ms from the
  RPC node. This run waits for each confirmation from the Mac.
- Frame hashes were not checked on testnet in the throughput runs (the hook
  that reads them was off); the SBF ProgramTest checks the golden frame.

## Next

1. Pipeline the serial chain (send dependent same-lock transactions back to
   back, as OrderedLane does) and each lane's render chain.
2. Fewer serial transactions per frame: open the session with up to 3 tics
   per ADVANCE (~420k CU each) and fold the input batch into the first
   advance transaction.
3. Measure from a low-latency host if the owner wants (the Tokyo box is
   stopped by rule).

## Pipelined lanes from the Tokyo host (10-04 evening)

Driver `stunts/doom-dcg/lanes_ordered.py`: each frame's session chain (input
batch folded into the first of two 4-advance transactions, then the capture)
on one ordered, pipelined lane (DCG `OrderedLane`); each render lane's 12
strips pipelined on that lane's own ordered lane once its capture lands;
commits in frame order. Host `basanos-tokyo-2` (2.8 ms to RPC
`64.130.44.172`), same program `9Uj6wmra…`, 3 lanes.

| Host | Run | Frames | Frames/s | Notes |
|---|---|---:|---:|---|
| Mac | confirm-per-step sequencer (above) | 8 | 0.107 | serial chain ~9.3 s/frame |
| Mac | pipelined, 4 advances/tx | 3 | 0.053 | renders ~40 s/frame (see below) |
| Tokyo | pipelined, 4 advances/tx, priority step 1 | 3 | 0.489 | includes start-up |
| Tokyo | same | 9 | ~~0.627~~ **invalid** | only 2 of 9 frames happened on chain (cursor 488 -> 504); see the correction below |
| Tokyo | + `--overlap` (next frame sent once its lane is free) | 9 | ~~0.654~~ **invalid** | no frame happened on chain (cursor stayed 504) |
| Tokyo | + `--pipeline-commits` (one ordered commit lane) | 12 | ~~0.740~~ **invalid** | no frame happened on chain (cursor stayed 504) |

Measured (testnet, journal and driver timestamps):

- The session chain lands its capture ~1.2–1.3 s after send; most of that is
  one repair pass per frame (an advance batch or the capture lands out of
  order within a block). Render chains land in 0.13–0.22 s; out-of-order strips
  are refused by the render-cursor guard (2338/2339) and resolved by repair.
- Found: `OrderedLane`'s default priority step (1000) with the epoch offset
  priced each 840k-CU strip near 1 FOGO; the node refused the pipelined
  strips at preflight (each render chain then waited 20 s and was repaired
  one transaction at a time). Priority step 1 fixed it (as in the 10-01 driver).
- Mac vs Tokyo: from the Mac the same session chain took ~6 s per frame;
  delivery measurements belong on the Tokyo host (owner, 10-04).

Next for 3.0 frames/s: the per-lane cycle (~3.4 s: capture lands, strips
render, commit confirms, lane frees) over the lane count bounds throughput,
so more lanes (this program has 3; the plan allows 4, DCG's cap to check)
and a shorter cycle (commit without waiting for the confirmation of the
previous frame's capture chain; fewer confirmation round trips per lane).
The on-chain floor of the session chain is ~3 transactions of ~1.3M
requested CU against the 2.5M per-account block cap, about 0.12 s per frame.

## Correction and 3 frames/s from Tokyo (2026-10-05)

**The 0.627, 0.654 and 0.740 rows above are invalid.** The session's input
stream had capacity 512 tics. The driver wrote each frame's inputs one
sequence ahead (cursor+1..cursor+8; advancing from cursor c reads input c),
so the frame ending at 512 asked for input 512 and the stream refused it
(2328, backpressure). From then on every frame's steps were refused on chain,
and `OrderedLane`'s repair counted each refused simulation as "already
landed", so the driver logged frames as committed. The session cursor (bytes
112..116) shows what happened: 424 -> 448 -> 464 -> 488 over the first runs
(the 0.114 and 0.489 rows are real), 488 -> 504 during the 9-frame run, then
504 for every later run. Found on 10-05 by dumping each lane's failed
statuses (receipts `out/runs/doom-lanes-testnet-2026-10-04/tokyo-2026-10-05/`,
`tokyo-3f-diag-bad.json`).

Fixes:

- DCG `OrderedLane` (Python client, `dcg/sequencer/ordered_lane.py`): repair
  counts a refused step as landed only if its original signature succeeded;
  otherwise it raises. The pre-repair pause is a parameter (`repair_delay`,
  default 1.0 s as before). Offline tests: 17 pass, two new (a step refused by
  another guard raises; a dropped step is repaired with no delay).
- Driver `lanes_ordered.py`:
  - writes exactly the inputs a frame's advances read;
  - grows the stream by 512 slots (DCG's per-instruction maximum, allowed only
    at cursor == capacity) in the first transaction of the frame that
    starts at capacity;
  - takes the frame count from verified commits and cross-checks the session
    cursor on chain;
  - prices each step with an epoch ceiling of frames + 1. With the default
    ceiling of 100,000, the priority gap between a lane's steps was ~1 part
    in 300,000. That is smaller than the scheduler's cost difference (the
    first session transaction carries the inputs), so the second transaction
    landed first and every frame paid a repair. The gap is now ~1%;
  - requests 1.2M CU per strip (two strips exceeded 840k; 1.2M still fits
    two strips under the 2.5M per-account block cap);
  - waits for the previous frame's capture before resolving a frame;
  - sends the next frame only after the previous frame's capture is seen
    (no `--overlap`). With overlap, one capture in 58 landed after the next
    frame's advances, which moved the cursor past it and lost the frame.
    (Folding the capture into the last advance's transaction is refused by
    DCG, 2334.)
- `lanes_heal.py` finishes or aborts lanes left mid-render.

**Measured** (testnet, program `9Uj6wmra…`, host `basanos-tokyo-2`, 3 lanes,
4 advances per transaction, `--pipeline-commits`, no overlap; frames counted
from verified commits, each run cross-checked against the session cursor):

| Run | Frames | Seconds | Frames/s | Repairs | Session cursor |
|---|---:|---:|---:|---:|---|
| `tokyo-60f-3l-noov` (repair delay 1.0 s) | 60/60 | 15.22 | **3.94** | 0 | 1152 -> 1632 |
| `tokyo-60f-3l-noov-r2` (1.0 s) | 60/60 | 13.78 | **4.35** | 0 | 1632 -> 2112 |
| `tokyo-60f-3l-noov-r3-d0` (0 s) | 60/60 | 19.66 | **3.05** | 0 | 2112 -> 2592 |
| `tokyo-60f-3l` (with `--overlap`) | 57/60 | 15.53 | 3.67 | 0 | one capture overtaken; stopped |

The spread (3.05–4.35) comes from the network and the leader; the repair
delay does not matter when nothing needs repair. With repairs gone, each
stage costs its landing time (session chain ~0.1–0.5 s, 12 strips
~0.2–0.3 s, commit ~0.1 s) instead of landing time plus 1 s.

Not established: frame hashes were not compared with the golden frames in
these runs (as before), and the publication path (relayer, viewers) was not
exercised. These numbers are one session, one host, testnet; the fourth lane
was not needed and was not provisioned.
