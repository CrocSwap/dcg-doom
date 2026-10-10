# Doom on DCG: public mode on the DCG pipeline (2026-10-09)

**Status: measured.** Repo split step 3 (`docs/plans/dcg-doom-split.md` on
branch `plan/dcg-doom-split`) ports public mode (one shared game, spectators,
idle handoff) from the R-series relayer to the DCG pipeline before Doom moves
to its own repository. Owner answers 2026-10-09: idle pause, and a 1,000
testnet FOGO cap for a public run.

## What changed

- **Pause by request** (`stunts/doom-dcg/relay_pipeline.py`, op `pause`;
  `stunts/doom-relayer/sender.mjs`). An idle lobby asks the pipeline to stop
  building frames; the frames it already sent land. The R-series SIGSTOPped
  its sender instead, which would leave DCG lane transactions to resume on
  expired blockhashes (about 6 s at 40 ms slots).
- **Run-wide spend.** Spend is the drop in the summed balances of the payer
  and the three lane payers since `--payer-baseline-lamports`, the run's first
  start, which the relayer keeps in the public state file. A restart continues
  the same cap. Funding a payer mid-run lowers the measured spend.
- **Events.** `failed` when the pipeline gives up; `death` when the render
  snapshot's player state (R3-DATA offset 44) turns to Doom's PST_DEAD. The
  page's death banner reads `death`.
- **No rollover.** Session `G913rZjZ…` uses a ring input stream, so the
  R-series rollover (RESET/9 near a full input buffer) is not needed.
- **Stop.** A stop during a failed frame no longer starts a heal, so the
  pipeline writes its receipt inside the relayer's 20 s close window.
- **Launcher** `scripts/doom_dcg_public.sh deploy|start|stop|status`: deploys
  committed code only, shares Tokyo's Doom lock, default cap 1,000 (range
  (0, 1,000]). **Probe** `scripts/doom_dcg_public_probe.mjs` drives the
  public API as the page does (join, signed key-state input, heartbeats, the
  frame stream) through play, the 60 s idle pause, and a resume.

## Measured

| Run | Play (frames) | Paused (frames) | Resume (frames) | Spend while paused | Result |
|---|---:|---:|---:|---|---|
| Mac relayer, on an airplane link (30 s / 90 s / 30 s) | 3 | 0 | 1 | flat at 0.083197 | pass; 2 post-resume frames hit RPC socket timeouts and were retried |
| Tokyo relayer and probe (60 s / 90 s / 60 s) | 251 | 0 | 252 | flat at 1.245025 | pass, no failures |

The Tokyo run (deployed commit `3fe2297f`, program `2uHBWbYE…`, session
`G913rZjZ…`, cap 5 on its own state file): about 4.1 frames/s in both play
windows; 749 frames and 1.879 testnet FOGO over the 300 s process (about 0.6
FOGO per active minute; the game keeps running for the first 60 s of an idle
lobby, until the pause); clean stop with receipt, lock released, box stopped.
At that rate the 1,000 FOGO cap is about 27 hours of active play.

The Mac run's frame rate reflects the in-flight link, not the pipeline.

Receipts: `out/runs/doom-dcg-public-tokyo-requal-2026-10-09/`,
`out/runs/doom-dcg-public-local-2026-10-09/`.

## Soak (measured 2026-10-09 to 10-10, Tokyo)

`scripts/doom_dcg_public.sh` with its own state file and an 18 FOGO cap; the
probe looped cycles of 20 s play, 280 s idle (the lobby pauses after 60 s),
20 s resume, joining and leaving each cycle.

- 16 of 16 cycles passed: 81–96 frames in each 20 s play window (4.05–4.8
  frames/s), 74–95 in each resume, 0 frames and flat spend in every paused
  window.
- 5,157 s of relayer process, 7,174 frames, cursor 71,008 (the 512-slot
  input ring wrapped well over a hundred times with no input fault), 2,643
  key events, no pipeline failure until the cap.
- The cap ended the run as designed: the pipeline stopped itself at 18.03
  testnet FOGO ("spend cap reached"), the lobby showed "Public run spend cap
  reached. The game is paused.", and later inputs got 402
  `spend_cap_reached`. The only probe errors were those 97 refusals and the
  16 stream closes at each leave.
- About 1.1 FOGO per cycle (100 s of running game per cycle).

Receipts: Basanos `out/runs/doom-dcg-public-soak-2026-10-09/`.

## Not shown

- A `death` event live (covered by an offline test of the snapshot parse and
  the transition).
- The public page in a real browser against the DCG relayer, spectators and
  the offer handoff (covered by the existing offline public-mode tests, which
  do not depend on the pipeline).
- A week-long run, or a restart mid-run on the same state file.
- A stop while lanes are mid-frame can still take longer than the relayer's
  20 s close window (each lane wait is up to 20 s plus repair).
