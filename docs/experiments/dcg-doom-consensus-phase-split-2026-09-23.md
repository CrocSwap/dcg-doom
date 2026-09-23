# DCG consensus fast path: SIM2 phase split — 2026-09-23

Design: [`docs/design/dcg-consensus-mixed-mode-2026-09-23.md`](../design/dcg-consensus-mixed-mode-2026-09-23.md).
Evidence: M1328.
Receipts: `/Users/colkitt/sith/toys/crypto/basanos/out/runs/dcg-doom-consensus-design-20260923/`
(`phase_split.py`, `phase-split.json`, `SHA256SUMS`).

## Result

**Measured (retained receipt), split derived.** This is a re-analysis of the
per-tic markers in the metered SIM2 SBF ProgramTest profile
`out/runs/doom-staged-input-sweep-20260922/sbf-profile-a3/sweeps.jsonl`
(sha256 `6e3f7eb5…`). It covers 1,000 DEMO1 tics and 295 running sweeps, and
every sweep was oracle-equal. Markers add about 200 CU each; they are not
subtracted.

| Phase | Median | Max | Mean |
|---|---:|---:|---:|
| Entry validation | 3,752 | 3,752 | 3,752 |
| Pre-sweep live ST1+MP1 root | 50,338 | 51,583 | 50,355 |
| Per tic: slot checks | 346 | 346 | 346 |
| Per tic: engine step (ArmV2 + `G_Ticker`) | 45,253 | 222,408 | 50,170 |
| Per tic: input-chain hash + loop | 584 | 584 | 582 |
| Per tic, whole loop | 46,180 | 223,333 | 51,098 |
| Publication | 348,033 | 354,398 | 348,093 |
| Post-publication root + meta | 49,746 | 50,991 | 49,763 |

Derived from the means:

- Fixed non-tic work is 452,805 per running sweep, of which 448,210 is
  publication plus root hashing.
- At the deployed 8-tic cap, publication and hashing are **52.6%** of a
  sweep.

Worst sliding-window sum of the per-tic loop, with no publication:

| Tics per sweep (k) | Worst window (CU) |
|---:|---:|
| 8 | 675,785 |
| 12 | 979,744 |
| 16 | 1,288,147 |
| 17 | 1,349,633 |
| 18 | 1,413,150 |

A DCG pre-kernel bound comes from an existing testnet receipt,
`out/runs/dcg-attention-2576-testnet-20260922/cases.jsonl`:

- An image-binding refusal consumed 4,942 program CU.
- A 5-slot entry refused at shared-window admission (357) consumed 70,623.
- The honest entry consumed 1,108,396.

## Boundary

- Analysis only. No build, no ProgramTest, no chain transaction.
- The profile is the metered diagnostic image on the legacy 5,120-slot
  buffer ABI. The per-tic and publication code is the same as the deployed
  SIM2 path.
- The window table covers the DEMO1 1,000-tic prefix only. The 5,026-tic
  lifetime maximum for an 8-tic sweep is 3.6% above the prefix maximum.
- The lean-path costs in the design are estimates.
