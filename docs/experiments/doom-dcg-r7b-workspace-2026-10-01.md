# Doom on DCG r7b: workspace-first render results

## Scope

This records the local, offline DCG workspace-first implementation and the
Doom ProgramTest run from dispatch `doom-dcg-r7c`. It supersedes the mutable
render proposal in
[`doom-dcg-r7-review-and-testnet-plan-2026-10-01.md`](doom-dcg-r7-review-and-testnet-plan-2026-10-01.md).
The rejected mutable-render patch was deleted. This work performed no testnet
or mainnet operation, Doom publish, Tokyo relayer action, or R15 rebuild.

The DCG clone began at `057f088c5f31b9fd53750628403eac96f0d6e133` and the
accepted changes are commit
`711f2f18209f68658b5f1600df44fc248a416f29`. The Basanos application image was
built from source commit `7019279c79037ef2659a64394d9ae1265f81e339`; its SHA-256
is `6eb9e318a4aac35ddb6a4a5b5a9c785835e996b04f4b9d4849020c3b3871cead`.
The unchanged production image in the receipt has SHA-256
`7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130`.
The sealed Doom resource SHA-256 is
`d9cb710813d965c62eb919a477e3ea571d7ad986d82a07f611969bc8f09b9cf0`.

`stunts/doom-dcg/Cargo.toml` and `Cargo.lock` pin both DCG packages to revision
`0ec72af4a108e1457c5c601787e1a775aff78458` at the stable local repository URL.
The ignored `dcg-repo/` clone used to verify the not-yet-merged revision is
retained in this assigned worktree; it is not part of the Basanos source tree.

## Accepted DCG changes

- DCG no longer applies a generic writable-state-account refusal to view
  instructions. This matches the address/provenance rule. Doom still refuses a
  writable state span in its own kernel callback; the adversarial ProgramTest
  expects `REFUSAL_KERNEL` for that application-level refusal.
- Workspace payload sizing is consistent across create, grow, and view
  validation: it uses the workspace limit and `MAX_VIEW_BYTES`. Publication
  scratch retains its smaller `MAX_SCRATCH_BYTES` limit. The code comments
  explain that workspaces hold view payloads and scratch has a separate cap.
- Workspace-first `RUN_PHASE` ordering is normalized before the existing v3
  validators run. DCG snapshots the full 128-byte workspace account header
  around the callback and returns `REFUSAL_KERNEL` if the callback changes it.
- The SBF workload covers workspace-first ordering and malformed ordering,
  refusal paths, callback header restoration and mutation refusal, refusal to
  close a live workspace, and the opt-out from clearing workspace payloads at
  begin. Direct normalizer tests cover authority/session positions, account
  count, and an empty state-span list; the ordinary account-order path borrows
  the caller slice without copying.

## Measured local results

**Measured: DCG stateful-v3 SBF workload.** The workload passed 13/13 tests
against DCG commit `0ec72af4a108e1457c5c601787e1a775aff78458`. It covers live
workspace close refusal, malformed workspace-first account counts, direct
normalizer rejection of misplaced authority/session accounts and empty state
spans, callback-header protection, and preserving dirty workspace payload at
begin. The retained image SHA-256 is
`18bb4a45556b6a16ead200446b05e408f778bcc440a8dba4f92a83fd7c822ce3`.
`cargo metadata --locked` passed for the DCG workspace.
The SBF build log also contains 4,800-byte stack-frame diagnostics for the
existing v2 `checked_session` function; the build returned success and the
v3 workload passed, but this v3 run does not establish that v2 path's SBF
stack behavior.

**Measured: Doom ProgramTest.** The final run passed 1/1 in 1,358.96 seconds
against that DCG commit and the rebuilt Doom application image
(`178f358e47e2dc57a553eea3f709870a46a40995c307604b214a10183a49704e`). It
replayed 1,000 tics and 606 initialization phases, matched the retained SIM/2
roots and per-tic state hashes, and matched the retained frame golden at pixel
SHA-256 `ca06a194b4853ccd2d298763d06e94b96a5246273c0829a401c07401d1c69504`.
The one-shot golden-pixel render used 9,490,278 CU across 20 transactions and
138 phases. The ten-frame cadence trace used 198 transactions total (19.8 per
frame average) and 107,499,985 CU total, or 10,749,998.5 CU per frame including
the snapshot seed transaction. The required production image remained unchanged
at SHA-256
`7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130`.

The lower r7b CU totals came from a run against DCG `8dcc85f8`, where
`view_meta` and `state_meta` re-derived addresses on every instruction. The
provenance rework at `057f088` removed that repeated work. **Estimated
attribution:** about 37.2k CU per view instruction across 140 view instructions
accounts for roughly 5.21M CU per frame. This reconciles r7b's reported 14.85M
golden frame and 16.1M ten-frame average with the r7c measurements of 9.643M
for the golden frame and 10.902M per frame over ten cadence frames; those
earlier totals describe a different DCG implementation, not a different
workload. The r7d measurements are a further 152,351 CU/frame lower: 9.490278M
for the one-shot frame and 10.7499985M for the cadence average. **Estimated
attribution:** this is consistent with r7d's ordinary account-order path
borrowing the caller slice instead of copying `accounts.to_vec()`; no isolated
CU experiment decomposed the full difference. The r7c measurements remain in
`out/runs/doom-dcg-r7c-2026-10-01/doom-programtest-metrics.json`; the r7d
measurements are in
`out/runs/doom-dcg-r7d-2026-10-01/doom-programtest-metrics.json`.

Compute units do not set the estimated local sender-limited picture rate here;
transaction count does. The measured cadence uses 19.8 render transactions per
picture plus nine input/advance transactions. At 60 transactions/second, the
estimate is `60 / (19.8 + 9) = 2.083` pictures/second (about 2.08), or 16.7
tics/second at eight tics per picture. The r7d metrics and complete final log
are retained under `out/runs/doom-dcg-r7d-2026-10-01/`.

**Measured: cache and rerender checks.** The snapshot cache is 655,360 bytes
(640 KiB). Each of the ten observed frames used an 18,652-byte snapshot
payload, or 18,844 bytes including the 192-byte RSN/3 header. The SBF encoder
enforces a 1,024-mobj cap. The ProgramTest derives a resource-specific maximum
from the committed resource's counts (85 sectors, 648 sides, 126 textures,
57 flats) plus the encoder's 1,024-mobj cap. With the 128-byte RSN/3 data
header, two 16-byte player sprites, 16-byte sector and side records, 4-byte
texture and flat translations, and 52-byte mobj records, the measured
resource-specific maximum is 65,868 payload bytes or 66,060 bytes with the
192-byte wire header. ProgramTest asserts that this fits the 655,360-byte
cache and that each observed projection stays below the bound; the manifest
test checks the C encoder's record sizes and guards. This bounds every
projection the encoder can accept for this committed resource, not just the
ten measured frames. If a future resource or encoder change exceeds the cache,
Doom returns `KernelError::Refused`, which DCG maps to `REFUSAL_KERNEL`; the
`RUN_PHASE` transaction rolls back. The arena high-water observation was
613,664 bytes on each frame.

Each of ten deliberate aborts followed caching two rendered strip payloads.
Both cached cursor checks refused on each frame. The test then retried against
the same dirty workspace without zeroing it; the begin-phase payload comparison
confirmed it stayed byte-identical, and all ten resulting frame hashes matched
the abort/retry trace. Frames 2–10 also matched the cadence render.
The first full ProgramTest attempt reached a `BlockhashNotFound` during the
final view-batch transaction after 1,512.74 seconds. The harness now retries a
view-batch with a refreshed blockhash up to three times at the same cursor.
The final run passed with zero view-batch blockhash retries; the retry path was
not needed on that run. The final log also contains an expected caught panic
from the byte-audit negative control, which deliberately flips a protected
workspace-header byte; the test passed. The failed run is retained as
`out/runs/doom-dcg-r7d-2026-10-01/doom-programtest-blockhash-failure.log`.

**Measured: manifest checks.** All five Doom manifest tests passed after the
budget assertions were tied to DCG's declared kernel ceiling of 1,329,427 CU;
the 1.4M transaction cap reserves DCG replay overhead and margin. The first
test run exposed stale assertions equating those two values and is retained as
`out/runs/doom-dcg-r7d-2026-10-01/doom-manifest-tests-assertion-failure.log`.

**Measured: halt and close.** The workspace account was 10,462,736 bytes
including its header. After halting and closing the session, the account was
closed and the authority received exactly 72,821,533,440 lamports, equal to
the workspace's pre-close balance. This is a ProgramTest rent result, not a
testnet rent measurement.

## Estimated throughput and demo comparison

**Estimated: local sender-limited picture rate.** At the test's measured
average of 19.8 render transactions per picture, plus eight advance
transactions and one batched input transaction per eight-tic picture,
60 transactions/second gives `60 / (19.8 + 9) = 2.083` pictures/second
(about 2.08). At eight tics per picture this is about 16.7 tics/second.

**Measured: existing testnet demo comparison.** The existing demo recorded
3.354 active new pictures/second and 29.377 tics/second (about 29.4), over a
68.580-second active interval; see
[`doom-send-pipeline-2026-09-28.md`](doom-send-pipeline-2026-09-28.md).
The 90% port gate is about 3.02 pictures/second (`0.9 * 3.354`). The local
estimate is below that gate. The testnet result belongs to the existing demo
and is not a result for this workspace-first image. Neither local ProgramTest
throughput nor a single-machine golden proves on-chain performance or useful
model quality.

## Reproduction and receipts

The final Doom command was:

```sh
RUST_LOG=error \
DOOM_DCG_SBF_DIR=/private/tmp/doom-dcg-r7d-app-sbf \
DOOM_DCG_RESOURCE_PATH=$PWD/out/runs/doom-dcg-r7d-2026-10-01/input/doom-dcg-resource.bin \
DOOM_DCG_METRICS_PATH=$PWD/out/runs/doom-dcg-r7d-2026-10-01/doom-programtest-metrics.json \
CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r7d-app-target \
cargo test --offline --manifest-path stunts/doom-dcg/Cargo.toml \
  --no-default-features --features sbf-programtest \
  --test programtest_dispatch -- --nocapture --test-threads=1
```

The attempted temporary Cargo path override triggered Doom's clean-source
provenance guard, so that build stopped before compilation. The tracked Cargo
manifest and lockfile were restored to the stable repository URL and exact
revision; a process-local Git URL rewrite then resolved that URL from the local
clone at the pinned commit for the successful clean-tree build and ProgramTest.
The failed override log is retained as `doom-app-sbf-build.log`. The successful
test log, metrics JSON, app/DCG images, build logs, input resource, failed
blockhash run, and source fingerprint are retained under
`out/runs/doom-dcg-r7d-2026-10-01/`. No evidence-register row was added because
this dispatch did not request one.
