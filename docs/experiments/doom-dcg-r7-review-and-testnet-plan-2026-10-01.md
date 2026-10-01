# Doom on DCG round 7: renderer review package and part-2 plan

> **Superseded:** This mutable-render proposal was rejected. The accepted
> workspace-first DCG implementation and its local results are recorded in
> [`doom-dcg-r7b-workspace-2026-10-01.md`](doom-dcg-r7b-workspace-2026-10-01.md).
> The testnet measurement plan below remains unauthorized by this dispatch.

## Mutable-render rationale and write contract

The pinned Doom renderer uses the fixed-address engine context for renderer
scratch state. DCG stateful v3 therefore needs an explicit mutable-render
opt-in for this application. Read-only rendering remains the default. The
opt-in is limited to the `RUN_PHASE` callback and to sessions whose declared
state accounts are all writable.

For Doom's RSN/3 renderer, the allowed writes are:

| Phase | Writable bytes and accounts |
|---|---|
| `BEGIN_PHASE` | DCG session phase/cursor fields, the workspace expected-state-cursor field at header bytes `[112,116)`, and the declared workspace payload, which DCG clears. Doom state, resources, output views, scratch, and renderer callback are not writable. |
| `RUN_PHASE` | The full, headerless 9,734,160-byte primary Doom context span `[0, ENGINE_CONTEXT_BYTES)`; the declared workspace payload `[0, V3_VIEW_WORKSPACE_BYTES)`; the expected-state-cursor field at publication-scratch header bytes `[112,116)`; and the current output chunk in publication-scratch payload. The renderer reads the resource, ST1, and MP1. It does not write ST1, MP1, resource bytes, other view-account header bytes, workspace header bytes, or view-output accounts. |
| `COMMIT_PHASE` | The nine declared view-output payloads, their expected-state-cursor header fields, and DCG publication/session metadata. The Doom callback is not invoked. Doom state, resources, workspace, and publication scratch are read-only. |

A refusal rolls back that transaction. Earlier successful render phases remain
committed in the primary context span, workspace, and staging scratch;
they are unpublished until `COMMIT_PHASE`. The application must discard or
resume the in-progress view session according to the v3 lifecycle.

This is an application write contract, with an important boundary: the current
DCG opt-in exposes every declared state span as a mutable slice. A first
tail-only audit found 25,126 changed primary-context bytes outside
`[7,637,008, 9,734,160)` in 1,583 disjoint ranges; Doom's fixed-address
renderer updates engine globals across its `doom_ctx_t`, not only the reserved
tail. The declared primary state span is therefore the whole exact-sized
context account. The ProgramTest checks that the other state, resource,
workspace header, non-cursor scratch-header bytes, and unpublished output
views stay unchanged; its negative control confirms the byte audit rejects a
write in a protected view-account header. DCG still does not enforce a
byte-level allowlist inside the primary context span. This is regression
evidence for the pinned compiled application, not isolation from a malicious
or newly changed kernel. Opus
review should decide whether this trusted-kernel contract is acceptable for
DCG or whether a generic enforcement mechanism is required before the change
can land.

The DCG review candidate is the nested local branch
`doom-dcg-r7-mutable-view-review` at `a56d58699f2f33e54b33de37707f65ae4d28c22e`,
based on the r6 opt-in code commit `a7e76f77640f782ec3bc8e34f2df100a57eb7347`.
It adds the explicit logical-span and transaction-boundary wording to
`dcg-repo/docs/stateful-workloads-v3.md` and a slice-preservation unit check.
The two-commit review patch is
`stunts/doom-dcg/review/dcg-stateful-v3-mutable-render.patch`. It has not been
merged or pushed to DCG. Send the candidate together with this application
test for Opus review after DCG's address rule lands.

## Part 1 measured local results

**Measured: production-image identity gate.** The pinned reproducible recipe
ran twice from clean source for current main `2a823df92876f4dff6b52950550e4d534ac15b45`
and twice for the r7 code commit `5eab3d32e01e4aca5f7b3dcc297b58cad10c47a9`,
using `sim-transport` and platform-tools v1.51. Both main builds and both r7
builds produced the same 1,067,784-byte standalone image,
`7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130`.
Each pair was byte-identical, with zero stack-frame errors and all 82
repository and 137 upstream source inputs verified. The source-manifest hashes
were `3e362e3d2c8cd7897d6bc0d712a6c7b5e8098a275f2ca95ac9f9d17c2d1c8b8d`
(main) and
`ada55e74eb6ad2580d7a38ce9a6c97bc1a0bb3e3be5076059abc243e0c3fbaea`
(r7). The input manifests have no mismatches. The production image was not
deployed or changed.

**Measured: SBF ProgramTest.** The application image built from committed r7
source is 860,400 bytes, SHA-256
`04a89084c425bb6ea23520c6d13fea12a7abebd8c6c84eaa298342b5e4623e39`, using
the local DCG candidate branch based on r6's opt-in mutable-v3 code. The
focused ProgramTest passed 1/1 in 258.53 seconds. It replayed 1,000 tics with
606 initialization phases, matched all 126 retained SIM/2 checkpoints, and
matched the frame golden at pixel SHA-256
`ca06a194b4853ccd2d298763d06e94b96a5246273c0829a401c07401d1c69504`.
Initialization, render, refusal, and per-transaction metrics are retained in
`out/runs/doom-dcg-r7-2026-10-01/programtest-metrics.json`.

Render batching used 138 phases in 13 transactions: ten batches of 13 phases,
then batches of 3, 3, and 2. Including begin and commit, the frame took 15
transactions versus r6's 140, an 89.3% reduction. It consumed 11,434,843 CU
total, averaging 762,323 CU per transaction; the largest transaction used
1,346,996 CU. Serialized render transactions ranged from 876 to 1,217 bytes,
under the 1,232-byte packet limit. The requested limit was 1,400,000 CU.

The byte audit observed changes only within the declared primary context,
workspace payload, and publication-scratch payload, plus the scratch
expected-state-cursor field `[112,116)`. It counted 97,967 changed primary
context bytes, 76,163 workspace payload bytes, 68,113 scratch payload bytes,
and the four cursor-field bytes. ST1, MP1, resource bytes, workspace header,
other scratch-header bytes, and view outputs before commit stayed unchanged.
The read-only-state adversarial attempt refused as expected. The account
boundary check is specific to this compiled application; DCG does not enforce
a byte-level allowlist within the full primary context span.

**Estimated: sender-limited active picture rate.** Reusing the measured demo
sender setting of 60 transactions/second, an eight-tic frame schedule, and
130 input-write transactions over 125 pictures gives
`60 / (15 render + 8 advance + 1.04 input-write) = 2.50 pictures/second`.
This excludes one-time setup and network scheduling, so it is not a testnet
measurement. The corresponding r6 schedule estimates 0.40 pictures/second;
the existing demo's measured active rate is 3.354 pictures/second.

## Part 2: fresh-address testnet measurement plan

This is a plan only; round 7 part 1 performs no chain operation.

1. After the DCG address rule and app seam land and the mutable-render change
   passes review, freeze the accepted DCG and Basanos commits, Doom resource
   SHA-256 and Merkle root, source manifest, command tape, image features, and
   SBF toolchain. Build the production-shaped Doom/DCG image and retain its
   SHA-256.
2. Generate a fresh program keypair and new testnet program address. Deploy
   there; leave the existing Doom demo and its program address untouched. Record
   the address, image hash/readback, setup account identities, rent, and fees.
   Apply the project stage cap to each chain-touching operation.
3. Run the same 1,000-command tape as r6, with one committed frame every eight
   tics (125 frames). Reuse the r6 resource and SIM/2 goldens. Batch consecutive
   render phases only when the packet-size and transaction CU budget permit.
4. Measure setup time separately from the active run. For the active interval,
   report committed frames divided by elapsed seconds, tics/s, frame hashes,
   transactions/frame, requested and consumed CU per transaction, bytes per
   transaction, fees, refusals, and unresolved transaction fates. Verify all
   eight strip hashes and the reconstructed frame hash against the retained
   golden. Compare the measured picture rate with the existing demo's measured
   3.354 active new pictures/s under the same sender setting.
5. Do not call the result a capability or product-quality claim: it is a
   matched testnet performance/correctness measurement for this image, address,
   command tape, and workload.

No testnet or mainnet transaction, Doom publish, Tokyo session, or R15 rebuild
is authorized by this part.
