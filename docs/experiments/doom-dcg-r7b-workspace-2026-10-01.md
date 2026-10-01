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

`stunts/doom-dcg/Cargo.toml` and `Cargo.lock` pin both DCG packages to the
local clone's Git commit, rather than resolving an unpinned sibling directory.
The README records the baseline, accepted patch/fixes, clone location, and
recreation steps. Because Cargo's source is a `file://` URL, another checkout
must recreate the clone at the configured worktree path before running Cargo.
The ignored `dcg-repo/` clone was retained locally at the pinned commit; it was
not pushed.

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
  refusal paths, callback header restoration and mutation refusal, and the
  opt-out from clearing workspace payloads at begin.

## Measured local results

**Measured: DCG stateful-v3 SBF workload.** The full workload passed 13/13
tests (12 prior cases plus the new workspace cases) using the rebuilt DCG SBF
image, SHA-256
`75fdf26cdfb5e98c47dfba66233486e4e2276e7d82e9fd9a01c1636c8f78e4fb`.
The test was run with `--features sbf-real-lifecycle-test` and the rebuilt
image at `/private/tmp/doom-dcg-r7c-dcg-sbf/dcg_program.so`.

**Measured: Doom ProgramTest.** The final run passed 1/1 in 2,300.54 seconds
against that patched DCG clone and the rebuilt Doom application image. It
replayed 1,000 tics and 606 initialization phases, matched the retained SIM/2
roots and per-tic state hashes, and matched the retained frame golden at pixel
SHA-256 `ca06a194b4853ccd2d298763d06e94b96a5246273c0829a401c07401d1c69504`.
The single golden-pixel render used 9,642,629 CU across 20 transactions and
138 phases. The ordinary ten-frame cadence trace used 198 transactions total
(19.8 per frame average) and 109,023,495 CU total, or 10,902,349.5 CU per
frame including the snapshot seed transaction.

The brief supplied 16.1M CU as the ten-frame average and 14.85M CU as the
golden-frame figure. Those values are **not reproduced** by this final
ProgramTest artifact: its recorded values are 10.902M and 9.643M respectively.
Treat the brief-supplied figures as open pending reconciliation with their
source run; do not cite them as measurements from this build. The retained
artifact is `out/runs/doom-dcg-r7c-2026-10-01/doom-programtest-metrics.json`.

**Measured: cache and rerender checks.** The snapshot cache is 655,360 bytes
(640 KiB). Each of the ten observed frames used an 18,652-byte snapshot
payload, or 18,780 bytes including its 128-byte header; none exceeded the
cache. The arena high-water observation was 613,664 bytes on each frame. This
sample does not prove a bound for every reachable game state. If a snapshot
callback reports a projection larger than its cache, the designed behavior is
`InvalidInput`, mapped by DCG to `REFUSAL_KERNEL`, and the `RUN_PHASE`
transaction rolls back; no over-cap reachable state was measured here.

Each of ten deliberate aborts followed caching two rendered strip payloads.
Both cached cursor checks refused on each frame. The test then reset the
workspace and rerendered; all ten resulting frame hashes matched the
abort/rerender trace, and frames 2–10 matched renders from a fresh workspace.
No render blockhash retry was needed in the final cadence trace. The final
log contains an expected caught panic from the byte-audit negative control,
which deliberately flips a protected workspace-header byte; the test passed.

**Measured: halt and close.** The workspace account was 10,462,736 bytes
including its header. After halting and closing the session, the account was
closed and the authority received exactly 72,821,533,440 lamports, equal to
the workspace's pre-close balance. This is a ProgramTest rent result, not a
testnet rent measurement.

## Estimated throughput and demo comparison

**Estimated: local sender-limited picture rate.** At the test's measured
average of 19.8 render transactions per picture, plus eight advance
transactions and one batched input transaction per eight-tic picture,
60 transactions/second gives `60 / (19.8 + 9) = 2.083` pictures/second. The
brief's conservative rounded estimate is 2.07 pictures/second. At eight tics
per picture this is about 16.6 tics/second.

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
DOOM_DCG_SBF_DIR=/private/tmp/doom-dcg-r7c-app-sbf \
DOOM_DCG_RESOURCE_PATH=$PWD/out/runs/doom-dcg-r7c-2026-10-01/input/doom-dcg-resource.bin \
DOOM_DCG_METRICS_PATH=$PWD/out/runs/doom-dcg-r7c-2026-10-01/doom-programtest-metrics.json \
CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r7c-doom-target \
cargo test --offline --manifest-path stunts/doom-dcg/Cargo.toml \
  --no-default-features --features sbf-programtest \
  --test programtest_dispatch -- --nocapture --test-threads=1
```

The final successful log is
`out/runs/doom-dcg-r7c-2026-10-01/doom-programtest-final.log`; earlier
blockhash failures and the harness compile correction remain as separate
receipt logs. The full DCG workload log, input image/resource, metrics JSON,
and source-tree fingerprint are also retained under
`out/runs/doom-dcg-r7c-2026-10-01/`. No evidence-register row was added because
this dispatch did not request one.
