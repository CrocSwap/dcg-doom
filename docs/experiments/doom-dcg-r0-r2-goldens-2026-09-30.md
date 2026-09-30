# Doom DCG rounds 0 and 2: demo packet and local ProgramTest goldens

**Status:** round 0 is a frozen recipe and historical baseline; round 2 is a
local SBF ProgramTest result. This is mechanics evidence, not a DCG port or a
claim about model or game quality. Numbers below are labeled **measured**,
**estimated**, **designed**, or **open**.

## Round 0: frozen demo identity

**Measured:** the current production image is `7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130`
(1,067,784 bytes), built from source commit
`6d601fa41971be9737349a4f8adcc690e4c8face`. Its source manifest SHA-256 is
`ef1ddbd8fb23acdbe79de82dd40c9c6266c40772e0c91e8c5e3eca9dfa5bb8c1`. These
identities are from the retained image build and source manifest described in
[`doom-predictor-relayer-2-2026-09-28.md`](doom-predictor-relayer-2-2026-09-28.md).
The image and manifest used by round 2 are retained under
`out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/image/`.

| Account | **Measured** shape |
| --- | ---: |
| Engine context and each RSN/3 scratch account | 9,734,160 bytes |
| Each of five WAD accounts | 1,048,576 bytes |
| ST1 state | 126,976 bytes |
| MP1 mirror | 65,536 bytes |
| CI2 controller | 64 bytes |
| RI2 resource account | 261,056 bytes |
| SIM/2 meta | 384 bytes |
| SIM/2 input buffer | 524,416 bytes: 128-byte header + 65,536 8-byte slots |
| RSN/3 snapshot | 1,048,768 bytes |
| Each of eight RSN/3 strips | 8,128 bytes: 128-byte header + 40×200 pixels |

The four `render_buffers` setting is the sender's pipeline setting. The RSN/3
ABI identities and per-account sizes above are the deployed account shapes;
do not multiply them by that sender setting when calculating account rent.
The five WAD pieces have payload SHA-256
`1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771` in the
round-2 capture.

## Historical 09-28 baseline

**Measured:** the 75.297-second `advance-cu-1m4` receipt completed 266 frames,
advanced 2,204 tics (29.377 tics/s), and produced 233 new pictures overall
(3.094/s overall; 3.354/s during the 68.580-second active-input window). The
page reported 37 key-to-next-picture samples, median 193.4 ms and p95 709.8 ms.
There were no input refusals, codes 286/297/308, compute-budget exhaustions,
HTTP 429s, frame stalls, or frame skips. The receipt reports actual payer
spend of 0.017758127 FOGO. Preflight estimated 0.1485 FOGO in fees; that is an
estimate, not the paid amount.

Full retained receipt directory:
`/Users/colkitt/sith/toys/crypto/basanos/out/runs/doom-send-pipeline-2026-09-28/advance-cu-1m4/`
(`relayer-session.json`, `frames.jsonl`, `render-transactions.jsonl`,
`tic-sender-result.json`, `input-batches.json`, `page-driver.stdout.json`, and
`getblock-compute.json`). Each completed frame row includes a
`frame_sha256`; `new_picture` marks the new-picture samples.

This reference is **not an image-matched comparison**. Its preflight receipt
records deployed image `4aef5a1ccca74aa38a8c9b04d66c145a8885b516054126e6b6ceed47a9966433`,
not current image `7d3b…`. It also used `advance_tics=4` and
`advance_pipeline_depth=4`; the current `scripts/doom_play.sh` pins
`ADV_TICS=6` and `ADV_PIPELINE_DEPTH=1`. Both receipts used four sender buffers,
60 sends/s, strip rate 64/s, minimum cursor step 6, 1,400,000-CU ADVANCE limit,
and batching off. Treat the 09-28 figures as a historical baseline only until
a new session confirms the current image and current settings in its receipt.

## 60–90-second comparison recipe

**Designed:** after a future run is explicitly scheduled, use a fresh session
name and a 75-second window with the normal sender pinned in
`scripts/doom_play.sh`:

```sh
DOOM_SESSION_NAME=doom-dcg-compare-YYYYMMDD DOOM_SPEND_CAP=2 scripts/doom_play.sh 75
```

The script performs preflight, opens the browser page, and records a receipt
under `out/runs/doom-demo-reliable-2026-09-28/doom-dcg-compare-YYYYMMDD/`.
For a valid pair, confirm the receipt's production image SHA-256 is the frozen
`7d3b…` value and retain the complete `config` object. Keep the same map/reset
state, 75-second duration, sender settings, and key-action schedule across
both variants. The page starts with a short automatic move, then accepts live
arrow-key input. There is no checked-in deterministic keyboard replay for the
historical human-controlled route; the run's `input-batches.json` and
`page-driver.stdout.json` are therefore part of the workload record. Until the
same action trace is replayed, call the result a controlled operational
comparison, not a byte-for-byte matched input trial.

For each run, retain and report:

1. **Picture identity and count:** `frames.jsonl`, selecting
   `new_picture == true`; record `frame_sha256`, cursor, and frame index. Report
   new-picture hashes separately from duplicate pictures.
2. **Tics and frames:** `relayer-session.json` fields `start_cursor`,
   `final_cursor`, `tics_advanced`, `tics_per_second`, `frames_completed`, and
   `frames_per_second`.
3. **Requested and consumed CU:** requested limits from each signed
   transaction's compute-budget instruction (`render-transactions.jsonl`
   records render `cu_limit`; retain the sender/deposit request records too).
   For every landed transaction signature, query its transaction metadata and
   record `meta.computeUnitsConsumed` in a companion `transaction-cu.jsonl`,
   along with operation, signature, requested limit, and any refusal code.
   Report per-operation values and totals; block-level totals in
   `getblock-compute.json` are not per-picture consumed CU.
4. **Latency and refusals:** use the page's `stamp_samples` in
   `page-driver.stdout.json` to report key-to-next-picture median/p95 and sample
   count. Include `input_refusals`, sender refusal counts, render transaction
   errors, HTTP 429s, and compute exhaustion from the session and transaction
   receipts.
5. **Fees:** report `payer_start_fogo`, `payer_end_fogo`, and `spend_fogo` from
   `relayer-session.json`; list `preflight.spend_estimate.estimated_fee_fogo`
   separately as an estimate. Also retain the session duration and exact
   image/source identity.

No live session was run for this dispatch. The command above is a future-run
recipe, not a report that it was executed.

## Round 2: SIM/2 and RSN/3 ProgramTest goldens

**Measured:** the runner executed `stunts/doom-sbf`'s actual SBF handler under
`ProgramTest::prefer_bpf(true)`. It loaded the captured WAD and typed DEMO1
tape, compared the tape byte-for-byte with retained SIM/2 `commands.bin`, and
ran 1,000 tics over 126 running sweep boundaries. At every running boundary,
cursor, canonical ST1, full ST1 account bytes, MP1, and state root matched a
same-process sequential host oracle. Requested ADVANCE limit was 1,400,000 CU;
maximum measured running-sweep consumption was 649,541 CU.

The retained command tape is 4,000 bytes with SHA-256
`76d93e9f4668b7bd493a24663a150dd4e2f0c8d6fde67ae0ce9d248f31c039fd`.
The fixture records every running sweep boundary's cursor, ST1/MP1 hashes,
and state root. It is
[`tests/golden/doom/dcg_r0_r2_programtest.json`](../../tests/golden/doom/dcg_r0_r2_programtest.json)
(61,646 bytes; SHA-256
`7f2c653faa5956849bb3a0d07945dc2df0a99953ead472ef6d1d30786cc39f92`).

**Measured:** RSN/3 snapshot cursor 1,000 was rendered by SBF and consumed
9,648,124 CU with a 30,000,000-CU local ProgramTest request. Eight SBF strips
each used a 30,000,000-CU request and consumed 3,930,939; 4,033,703; 3,979,403;
4,048,694; 4,004,479; 3,964,760; 3,949,530; and 3,957,178 CU respectively.
Reconstruction from all eight strip outputs matched the host frame exactly,
SHA-256 `ca06a194b4853ccd2d298763d06e94b96a5246273c0829a401c07401d1c69504`.
The 30M request is a local ProgramTest ceiling for this standalone render; it is
not a live transaction setting or a network compute claim.

### Reproduce the local gate

With the retained inputs and image under `out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/`,
prepare the host engine and build the standalone driver in `/private/tmp`:

```sh
DOOM_SCRATCH=/private/tmp/basanos-doom-dcg-r0-r2-scratch stunts/doom-host/build.sh
scripts/warm_target.sh stunts/doom-sbf/driver /private/tmp/basanos-doom-dcg-r0-goldens-target
cd stunts/doom-sbf/driver
RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r0-goldens-target \
  DOOM_SCRATCH=/private/tmp/basanos-doom-dcg-r0-r2-scratch \
  OPENSSL_NO_VENDOR=1 OPENSSL_DIR=/opt/homebrew/opt/openssl@3 \
  cargo build --release --features ctx-engine --bin sweep_gate
cd ../../..
```

Then run:

```sh
/private/tmp/basanos-doom-dcg-r0-goldens-target/release/sweep_gate \
  out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/capture-current \
  out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/capture/resource-image.ri2 \
  out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/capture/controller.bin \
  out/runs/doom-dcg-r0-goldens-2026-09-30/programtest-recheck \
  out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/image/doom_sbf.so \
  7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130 \
  out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/image/doom_ex2_source_manifest.json \
  ef1ddbd8fb23acdbe79de82dd40c9c6266c40772e0c91e8c5e3eca9dfa5bb8c1 \
  out/runs/doom-dcg-r0-goldens-2026-09-30/inputs/oracle \
  --tics 1000 --seed 5eedd00d20260922 \
  --allow-rebased-start --skip-legacy-rsn2 \
  --expected-golden tests/golden/doom/dcg_r0_r2_programtest.json
```

The runner verifies the complete retained DEMO1 command tape against the
generated tape before starting ProgramTest. `--allow-rebased-start` records
the known Gate-A root mismatch; `--skip-legacy-rsn2` skips only the unrelated
legacy realloc probe. **Measured:** an initial RSN/3 attempt at 1,400,000 CU
was refused by ProgramTest's compute budget; the snapshot gate was then rerun
at its explicit 30,000,000 local limit. The fixture compares state/render
hashes and excludes variable ProgramTest compute measurements.

The run also exercised malformed deposit and advance payloads, wrong authority
and account roles, out-of-order/stale cursors, duplicate deposits, wrong
snapshot roots, malformed RSN/3 snapshot bytes, stale render cursors, and
wrong strip account roles. The controls confirmed durable state preservation
on refusal and atomic rollback when a later instruction failed. Their codes,
CU use, and preservation checks are in the ignored local receipt's
`controls.json` and `render-rsn3-controls.json`.

There are two explicit limits to this result:

- The original 09-22 Gate-A ST1 capture cannot start in the current host engine
  (`NativeSimSession::open rc=232`). The usable 09-27 capture has the same WAD
  and controller and accepts the exact retained command tape, but its initial
  state root differs from Gate A beginning at tic 0. Thus the golden's
  `retained_gate_a_roots_match` is `false`. SBF sweep boundaries match the
  oracle computed from this 09-27 start, not the old Gate-A per-tic roots.
- The separate legacy RSN/2 one-shot ProgramTest is skipped. Its 192-KiB
  realloc exceeds ProgramTest's 10-KiB account-growth limit (`InvalidRealloc`);
  the requested RSN/3 SBF snapshot and all eight strips ran and passed.

The local run receipt with generated comparison reports, boundary journal,
snapshot, strips, and reconstructed frame is under
`out/runs/doom-dcg-r0-goldens-2026-09-30/programtest-r3b/`; the captured
inputs and image are in the sibling `inputs/` directory. The clean
expected-golden recheck is under
`out/runs/doom-dcg-r0-goldens-2026-09-30/programtest-golden-recheck/`.
These large artifacts are intentionally kept under ignored `out/`; the compact
hash fixture is tracked.

## Verification and scope

**Measured:** the standalone driver compiled in `/private/tmp`. The SBF
ProgramTest gate passed twice; the second run matched the tracked fixture
exactly. Focused `rustfmt --check` on both modified Rust files and
`git diff --check` passed. The broader `cargo fmt --check` reports pre-existing
formatting differences in untouched driver files. No changes were made under
`stunts/doom-sbf/src`, no live chain or Tokyo relay was touched, and no program
upgrade or R15 REBUILD was run.

This only freezes the demo comparison and supplies a local behavior golden.
It does not qualify a DCG-hosted Doom image or establish matched live
performance.
