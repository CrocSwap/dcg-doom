# Doom DCG round 4: stateful adapter feasibility

**Result:** the real Doom adapter and its golden gate were not completed. The
pinned DCG stateful layer cannot currently carry the requested 1,000-tic,
snapshot, and eight-strip workload, and the composed image has no tag-230
dispatcher. The Doom kernels remain fail-closed. This record preserves the
source/API blockers and the local build evidence; it makes no DCG capability
claim.

Labels follow the repository convention: **measured** names a retained result,
**designed** describes a contract or proposal, **estimated** is planning
arithmetic, and **open** is unresolved or unmeasured.

## Source and image identity

**Measured:** DCG was cloned read-only at
`6731c76c3463335f08c9f165a237c21d3be729b4`. The checkout is local build input
under ignored `dcg-repo/`; it is not in the Basanos commit. The SBF image was
built with platform-tools v1.51 from `stunts/doom-dcg` with feature `sbf`. It is
1,069,056 bytes, SHA-256
`c0e7e4644af4e4a2ea1add1d3a1c73ac009b7a7cddd28a2e41702e6b7c8f2457`.

The only tracked source adjustment in the adapter crate adds the three
`ApplicationManifest` fields required by the pinned DCG API. The three
registered kernel methods still refuse, and their compute limits remain
`u64::MAX` sentinels. Replacing those sentinels with a fabricated ceiling would
make the descriptor appear valid while misdescribing an unmeasured workload.

## Pinned contract comparison

The pinned `dcg-repo/crates/dcg-program/src/stateful.rs` sets the following
**designed/implemented** bounds: input-stream capacity at most 64 commands,
eight steps per ADVANCE, at most eight state spans and 10,000,000 aggregate
state bytes, two view roles plus one scratch account, 4,096 bytes per view,
8,192 bytes of scratch, and 1,400,000 declared compute units per transaction.
ADVANCE multiplies the declared per-step limit by its requested step count and
refuses totals above 1,400,000.

| Requirement | Doom workload | Pinned v1 result |
|---|---:|---|
| One indexed stream covers the gate | 1,000 tics | Capacity is 64; state and cursor are bound to that session. No continuation mechanism transfers the same state to another session. |
| Persistent engine state | 9,734,160-byte context plus ST1 and MP1 (9,926,672 bytes in the scaffold schema) | Aggregate bytes fit under the 10,000,000-byte cap, but the engine cannot use these spans as currently compiled. |
| Engine context access | Existing C engine requires `DOOM_CTX` at the first account-data address and checks that address with `doom_ctx_guard` | DCG ADVANCE supplies state spans after actor, session, and stream accounts; each span's `data` starts after its 128-byte DSE1 header. No call-scoped context-pointer binding exists in the existing Doom engine interface. |
| Snapshot and strip publication | One 1,048,768-byte snapshot and eight 8,128-byte strips | A view is at most 4,096 bytes. PUBLISH_VIEWS accepts two declared views and scratch, then copies ranges from state; it does not call a renderer or publish eight outputs. |
| Full render compute | Existing local Doom SBF snapshot and strips each exceed 1,400,000 CU | The DCG adapter cannot declare an operation above that bound. The retained measurements used a 30,000,000-CU local ProgramTest request; they do not establish that the workload fits the DCG call. |
| SVM dispatch | Tags 230–239 must reach `process_with_kernel` | The current `doom-dcg` entrypoint directly calls `doom_sbf::transport_process_instruction`. `doom-sbf` exports its entrypoint from `stunts/doom-sbf/src/lib.rs`; replacing that dispatcher requires a no-entrypoint/processor seam there or an equivalent engine-crate split. The task forbids editing that source tree. |

The DCG stateful callbacks receive state spans, but `initial_state_spans` has no
authenticated WAD/resource input parameters. A separate initial-state and
resource-binding contract is also needed for a reproducible Doom session.

## ProgramTest and compute evidence

**Measured:** the Doom host source builder completed. The SBF image built
successfully. The focused adapter suite passed (2 tests); the manifest test
asserts that validation returns `InvalidComputeLimit(SIM_KERNEL_ID)`, because
DCG v1 rejects the scaffold's `u64::MAX` compute sentinel. This preserves the
contract result instead of working around it with an invented number.
`cargo fmt --check` passed.

**Measured:** a ProgramTest smoke probe loaded the SBF image and submitted a
105-byte OPEN_SESSION/tag-230 instruction. The image returned `invalid
instruction data` after 189 CU. The probe passed by observing that refusal; it
shows the linked image does not route tag 230 into DCG. It does not exercise a
DCG session or Doom kernel. The probe source and logs are retained under
`out/runs/doom-dcg-r4-adapter-2026-09-30/`.

The retained round-2 standalone golden remains the comparator, not a new
round-4 measurement. Its **measured** result is 1,000 tics over 126 running
sweep boundaries, maximum sweep use 649,541 CU, snapshot use 9,648,124 CU, and
strip use of 3,930,939; 4,033,703; 3,979,403; 4,048,694; 4,004,479; 3,964,760;
3,949,530; and 3,957,178 CU. These render calls used a 30,000,000-CU local
ProgramTest limit. The source record and byte-level results are in
[`doom-dcg-r0-r2-goldens-2026-09-30.md`](doom-dcg-r0-r2-goldens-2026-09-30.md)
and [`dcg_r0_r2_programtest.json`](../../tests/golden/doom/dcg_r0_r2_programtest.json).

**Open:** no DCG Doom tic or sweep ran, so there is no round-4 boundary,
snapshot, strip, or same-case CU comparison. The ≤10% added-compute gate was
not evaluated. I stopped before the one-tic adapter step because the built
image rejects the DCG instruction and the contract limits above prevent the
requested full lifecycle. No stale-cursor, duplicate-input, wrong-state-view,
live-close, or wrong-authority DCG control was run. No live chain, Tokyo,
relayer, upgrade, or REBUILD operation was used.

## DCG contract proposals

These are **designed proposals**, not implementation claims:

1. Add an app-owned SVM dispatch seam that can compose the Doom transport
   handlers with `process_with_kernel` without exporting a second entrypoint.
2. Add an authenticated invocation-local engine-context binding. It must define
   how a Doom engine receives the actual span address, how account headers are
   excluded, and how pointers are rebound and kept out of committed state.
3. Add a bounded continuation/input-window design that can advance more than
   64 commands while preserving one session's state schema, cursor, and input
   commitment across windows.
4. Add a render callback and publication model for one large snapshot and at
   least eight outputs larger than 4,096 bytes. Specify output ordering,
   cursor binding, scratch needs, and atomic failure behavior.
5. Define a resource/input binding for initial state and a resource envelope
   that can represent a phased workload whose retained standalone snapshot and
   strip calls exceed the current per-transaction ceiling. Do not raise the
   compute declaration beyond a runtime limit; split work into explicit,
   deterministic phases if that remains the target.

The worktree was checked with
`git diff main -- stunts/doom-sbf/src`; it is empty. The 2.9-GB Rust target,
host scratch, and SBF image were moved to `/private/tmp/trash-doom-dcg-r4` after
the checks, below the dispatch's 5-GB cap. No evidence-register row was added:
the requested capability was not measured.
