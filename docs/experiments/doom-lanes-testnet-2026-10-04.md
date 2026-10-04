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
