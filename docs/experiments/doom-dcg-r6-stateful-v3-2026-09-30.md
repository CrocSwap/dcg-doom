# Doom on DCG stateful v3, round 6 (2026-09-30)

**Result: measured local SBF ProgramTest.** The Doom-owned image completed a
real 1,000-tic run through DCG stateful v3. Its initial state, all 126 retained
SIM/2 sweep roots, all eight RSN/3 strip pixel hashes, and the reconstructed
frame hash matched the standalone golden. The full-frame SHA-256 was
`ca06a194b4853ccd2d298763d06e94b96a5246273c0829a401c07401d1c69504`.
Adversarial account/resource cases and an engine halt also reached their
expected results. This is a local SBF application demonstration, not testnet
throughput or a general protocol-capability claim.

The adapter is a v3-only image: the Doom context is the headerless primary
state at index 0, with `ST1` and `MP1` as additional state spans. This preserves
the pinned engine's existing fixed-address `doom_ctx_guard`. The RI2/WAD bundle
is copied into DCG's sealed resource account. Initialization runs in 16,384-byte
phases, and the RSN renderer uses the v3 resource-aware view callback and its
mutable-state opt-in. The DCG-side additive change is commit
`a7e76f77640f782ec3bc8e34f2df100a57eb7347` in the ignored `dcg-repo/` clone.

## Reproduction and artifacts

The app SBF image was built for source commit `9825d041ef4f33f4097b5bda2f8ba87eb5e06a37`
(tree `14f09907b3015b3a84ec0d029670cc5768d69191`) with pinned SBF platform-tools
v1.51 and `cargo-build-sbf --tools-version v1.51 --skip-tools-install`. The
measured image is 840 KiB, SHA-256
`62fe858c63a61db82fcd03acc01ae651f94fb0b78855cf1b536017c7052d9635`.
The packed resource is 4,457,108 bytes, SHA-256
`d9cb710813d965c62eb919a477e3ea571d7ad986d82a07f611969bc8f09b9cf0`; its
stateful-v3 65,536-byte-chunk Merkle root is
`26cc68c19943535ee09101690780ffb35692a5e4aef2c0e05467d02fff97a53e`.

The app image build used:

```sh
DOOM_SCRATCH=/private/tmp/doom-dcg-r6-scratch \
DOOM_SBF_SOURCE_TREE=14f09907b3015b3a84ec0d029670cc5768d69191 \
CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r6-target \
SBF_SDK_PATH=/private/tmp/basanos-sbf-sdk-v151-20260920 \
RUSTC_WRAPPER= \
/Users/colkitt/.local/share/solana/install/active_release/bin/cargo-build-sbf \
  --manifest-path stunts/doom-dcg/Cargo.toml --features sbf \
  --sbf-out-dir /private/tmp/basanos-doom-dcg-r6-sbf \
  --tools-version v1.51 --skip-tools-install
```

The focused test command was:

```sh
RUST_LOG=error \
DOOM_DCG_SBF_DIR=/private/tmp/basanos-doom-dcg-r6-sbf \
DOOM_DCG_RESOURCE_PATH=$PWD/out/runs/doom-dcg-r6-2026-09-30/input/doom-dcg-resource.bin \
DOOM_DCG_METRICS_PATH=$PWD/out/runs/doom-dcg-r6-2026-09-30/programtest-metrics.json \
CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r6-target \
cargo test --offline --manifest-path stunts/doom-dcg/Cargo.toml \
  --no-default-features --features sbf-programtest \
  --test programtest_dispatch -- --nocapture --test-threads=1
```

**Measured:** 1 test passed in 160.78 seconds. ProgramTest applied a 1,400,000
CU limit to each transaction. The full per-instruction and per-phase CU arrays,
transaction totals, and hashes are retained in
`out/runs/doom-dcg-r6-2026-09-30/programtest-metrics.json`; the final log, image,
resource, and earlier failed iteration logs are alongside it.

The setup instruction measurements were:

| Instruction | Count | Transactions | CU per instruction |
|---|---:|---:|---:|
| `OPEN_SESSION` | 1 | 1 | 20,955 |
| `CREATE_STREAM` / `GROW_STREAM` | 1 / 1 | 1 / 1 | 13,561 / 12,819 |
| `WRITE_INPUT` | 1,000 | 130 | 10,066–10,218 |
| `CREATE_STATE` / `GROW_STATE` | 1 / 1,210 | 1 / 152 | 28,643 / 8,830–51,947 |
| `RESOURCE_GROW` | 544 | 68 | 10,516–28,276 |
| `RESOURCE_CHUNK` | 69 | 18 | 9,933–42,727 |
| View/workspace/scratch create | 11 | 1 | 10,077–14,592 |
| `GROW_VIEW` | 399 | 50 | 7,066–11,512 |
| `BEGIN_INITIALIZE` | 1 | 1 | 8,754 |

## Correctness results

The context, `ST1`, and `MP1` were grown through 1,210 bounded state-growth
instructions and initialized in 606 transactions/phases. The phase schedule
contained 466 WAD-copy phases, 115 Doom-engine phases (6 static, 71 prep, 37
new-game, and 1 finalize), and 25 tail phases. Every retained phase completed
under the 1,400,000-CU transaction limit. The aggregate measured initialization
cost was 26,323,696 CU:

| Initialization phase group | Phases | Total CU | CU per phase |
|---|---:|---:|---:|
| Resource/WAD copy | 466 | 10,072,152 | 21,508–21,701 |
| Doom engine | 115 | 15,718,066 | 21,447–738,687 |
| State tail | 25 | 533,478 | 21,339–21,342 |

The engine completed the requested 1,000 tics using one ADVANCE transaction per
tic. Each post-transaction `ST1`/`MP1` root and account digest at the 126 retained
sweep boundaries matched
`tests/golden/doom/dcg_r0_r2_programtest.json`; the final root was
`7fc42f4cd2811cef55ea3c880fead25f64416ed83e4cb29adfea44cc6d6840d5`. ADVANCE
used 258,060 CU on average (229,578–348,659; 258,060,210 CU total).

The v3 view ran 138 phases over a 1,113,792-byte view payload in 8,128-byte
chunks. Including begin and commit, one frame took 140 transactions and
11,453,593 CU (11,327,833 phase CU, 62,653 begin CU, 63,107 commit CU). Phase
costs ranged from 59,227 to 484,967 CU. The test reassembled the 40×200 strips
row by row, then checked each retained strip hash and the 320×200 frame hash.
The 140 frame transactions are one `BEGIN_PHASE`, 138 `RUN_PHASE` calls, and
one `COMMIT_PHASE`.

The adversarial checks passed:

| Case | Expected outcome | Measured CU |
|---|---|---:|
| Begin initialization before resource sealing | Refuse, 2326 | 7,745 |
| ADVANCE before initialization | Refuse, 2331 | 10,454 |
| Substitute stream account for primary state | Refuse, 2331 | 15,174 |
| Wrong WAD bytes under a valid resource Merkle commitment | Refuse, 2334 | 20,303 |
| Engine halt before the next tic | Commit zero-length prefix at cursor 1,000; halt reason 1 | 23,178 |
| ADVANCE after the halt | Refuse, 2322 | 5,365 |

The wrong-WAD case used a separately sealed copy whose DCG Merkle proof was
valid; Doom's RI2/WAD validation still rejected the changed WAD payload. The
halt case deliberately set the game state to the intermission enum through
ProgramTest account injection after the golden render, then confirmed the
stateful session recorded the engine halt without consuming an extra command.

## Cost and rough throughput comparison

These estimates use measured local SBF CU and a stated sender-rate assumption;
they are not a testnet result. The current demo's 3.354 active new pictures/s
was measured on testnet in a 68.580-second window. Its 6.8M CU per picture is
the planning estimate from
[`doom-dcg-port-plan-2026-09-30.md`](../design/doom-dcg-port-plan-2026-09-30.md),
not a same-image measurement.

**Estimated, CU-only sensitivity:** one R6 frame plus eight measured one-tic
ADVANCE operations costs about 13.518M CU. Scaling 3.354 pictures/s inversely
from the estimated 6.8M baseline gives about **1.69 pictures/s**. This ignores
transaction submission latency and uses eight tics per picture to match the
baseline planning case.

**Estimated, current transaction schedule:** the test uses 140 transactions per
frame and one transaction per tic. At the baseline sender setting of 60
transactions/s, with eight tics and the measured 0.13 input-write transactions
per tic, this schedule is about **0.40 pictures/s** (`60 / (140 + 8 + 1.04)`).
The baseline was 3.354 pictures/s under that sender setting. This lower estimate
reflects the current 138 dependent view-phase transactions; batching adjacent
phases was not tested. ProgramTest's 1.4M requested CU per transaction and
Fogo's scheduling by requested compute also mean consumed-CU scaling alone is
not a testnet forecast.

## Iterations and scope limits

Three failed ProgramTest iterations are retained in the receipt. They exposed
and corrected a writer that staged beyond the 512-slot stream before growth,
strip concatenation that did not interleave scanlines, and a final stream
capacity that did not leave a slot for the deliberate post-run halt command.
The final stream grows once from 512 to 1,024 slots at cursor 512.

No testnet/mainnet transaction, deploy, Doom publish, Tokyo relayer/session, or
R15 REBUILD was used. No evidence-register row was added. The estimates do not
establish chain throughput, CU scheduling, render latency, rent, or useful
picture quality. The path proves that this pinned Doom engine and renderer can
execute and match the retained golden through the local v3 SBF adapter.
