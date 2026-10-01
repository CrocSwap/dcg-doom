# Doom DCG round 3: extracted-v1 adapter seam

**Status:** a Doom-owned crate and SBF image link the extracted DCG v1 crate and
register three consensus-mode stateful kernel descriptors. **This is not a
working DCG execution adapter.** The registered kernel methods refuse, and the
SBF entrypoint still routes instructions through the existing DOOM-SIM/2 and
RSN/3 handlers. The ProgramTest result below is therefore evidence for the
composed image's retained Doom path and for the identified DCG v1 gaps; it is
not evidence that a DCG kernel ran.

Labels follow repository convention: **measured** names a retained artifact;
**designed** is a proposed interface; **estimated** is planning arithmetic;
**open** is unresolved or unmeasured.

## Image and source identity

**Measured:** the SBF image was built with Solana platform-tools v1.51 from
`stunts/doom-dcg` with feature `sbf`, against the read-only DCG checkout at
`5c2de34c12e2c766d3d38a5d8b893d35d1ed5695`. The image is 1,069,056 bytes with
SHA-256
`5350a9e7e756a8b47cb066673694261616b69f9985e6567a5a69637fb187012e`.
The image lives in the ignored run receipt's `image/` directory. The DCG clone
is local build input only and is not part of the Basanos branch.

**Open:** the existing Doom build script embeds its Doom source manifest, but
that manifest does not describe `stunts/doom-dcg`, the DCG path dependency, or
the DCG commit. The linked image hash identifies the bytes, but the inherited
manifest is not complete build provenance for this composed image. Add an
app-owned build manifest before treating the image identity as a reproducible
protocol commitment.

## ProgramTest result

The existing `sweep_gate` driver was built offline in `/private/tmp` and ran
the composed image under `ProgramTest::prefer_bpf(true)`, using the retained
WAD, resource, controller, oracle, and DEMO1 input tape from round 2. Run
receipts are in:

- `out/runs/doom-dcg-r3-adapter-2026-09-30/standalone-baseline/`
- `out/runs/doom-dcg-r3-adapter-2026-09-30/dcg-linked-image-full/`

Both receipts ran 1,000 tics across 126 running sweeps. **Measured:** all sweep
boundary records, final state and mirror hashes, snapshot output, eight strips,
and reconstructed frame in the linked-image result match the round-2 fixture
after projecting out exactly `sbf_image_sha256`. That field necessarily differs
because this is a new image. A full fixture comparison including image
identity consequently failed as expected; no tracked golden was edited. The
projected comparison matched every other field, including the input,
`source_commit`, `source_manifest_sha256`, simulation boundaries, snapshot,
strips, and frame hash. The frame hash is
`ca06a194b4853ccd2d298763d06e94b96a5246273c0829a401c07401d1c69504`.

**Measured:** the baseline run's maximum running-sweep CU was 649,541; the
linked-image run's was 648,041. Its reconstructed frame matched the host frame
and all eight strip pixels matched the host frame. The retained round-2
fixture is
[`dcg_r0_r2_programtest.json`](../../tests/golden/doom/dcg_r0_r2_programtest.json).

Both images also exercised the existing Doom refusal controls. The linked-image
receipt has 52 SIM/2 controls plus three RSN/3 controls. They include malformed
deposit and advance data, wrong authority and account roles, stale and
out-of-order cursors, duplicate deposits, malformed snapshot bytes, and stale
render cursors; the controls record refusals and durable-account preservation.
These are Doom-handler checks, not DCG-dispatch checks.

The round-2 limitations still apply: the initial state is the known rebased
capture (`retained_gate_a_roots_match=false`), and the unrelated legacy RSN/2
realloc probe was skipped because ProgramTest's account-growth limit rejects
it. No live chain, Tokyo, relayer, upgrade, or REBUILD path was used.

## Compute comparison

The measurements below compare the baseline run with one full linked-image run
on the same tape and render cursor. The runner generates fresh random test
account keys on each execution, so the observed deltas include differences in
account/PDA inputs and do not isolate a code-only adapter cost.

| Operation | Standalone measured CU | Linked image measured CU | Observed delta |
|---|---:|---:|---:|
| Running sim sweeps | 363,741–649,541; 525,956 mean over 126 | 362,241–648,041; 524,456 mean over 126 | −1,500 each; −0.285% of standalone sweep total |
| Snapshot at cursor 1,000 | 9,640,624 | 9,639,124 | −1,500 (−0.016%) |
| Strip 0 | 3,933,939 | 3,932,439 | −1,500 (−0.038%) |
| Strip 1 | 4,033,703 | 4,032,203 | −1,500 (−0.037%) |
| Strip 2 | 3,979,403 | 3,976,403 | −3,000 (−0.075%) |
| Strip 3 | 4,045,694 | 4,048,694 | +3,000 (+0.074%) |
| Strip 4 | 4,004,479 | 4,005,979 | +1,500 (+0.037%) |
| Strip 5 | 3,967,760 | 3,966,260 | −1,500 (−0.038%) |
| Strip 6 | 3,954,030 | 3,952,530 | −1,500 (−0.038%) |
| Strip 7 | 3,955,678 | 3,955,678 | 0 (0%) |

The strip-3 and strip-4 linked-image values above are from the full control run;
the first comparison attempt had different CU for some operations while
producing the same bytes, consistent with run-to-run variance. A one-average-
sweep plus snapshot plus eight strips totals 42,041,266 CU standalone and
42,033,766 CU linked-image (**measured**, −7,500 or −0.018%). This local
ProgramTest total is not comparable to the estimated 6.8M testnet planning
picture in the port plan.

The current entrypoint does not invoke any of the registered DCG kernels, so
these values cannot establish the ≤10% added-compute gate for a DCG adapter.
They measure only the existing Doom handlers in the newly linked image. The
actual compute overhead of account authentication, DCG dispatch, state import
and export, and view binding is **open**.

## DCG v1 gaps and proposed changes

The following are **designed** contract proposals, not implemented behavior.

1. **SVM application dispatch.** v1 has a compiled-in manifest and a byte
   `ApplicationManifest::execute`, but no SVM entrypoint that resolves a
   manifest kernel and invokes its stateful lifecycle. Add an app-owned dispatch
   seam that selects exact kernel semantic/ABI versions and an operation
   (`initial_state` or `transition`) before calling the registered handler.
2. **Authenticated account spans.** Kernel methods receive byte slices, not
   account identities or rights. Define a bounded account-span view carrying
   key, owner, signer, writable role, length, and a schema-bound region. The
   SVM adapter must authenticate all descriptors and bounds before forming
   slices; specify overlap and aliasing rules.
3. **Doom input and session lifetime.** Define a session-bound input-stream
   descriptor containing stream root, input index, command width, cursor, and
   sequence. Specify write-once slots, permitted gaps, bounded multi-tic
   advancement, refusal atomicity, reset, and close eligibility.
4. **Persistent engine state.** Define versioned Doom state import/export for
   the 9,734,160-byte context, ST1, and MP1. Bind the state schema to separate
   account spans, establish the transition's before/after cursor, and make
   durable writes atomic. Runtime C pointers must be rebound per invocation;
   they must not become serialized state.
5. **Views and strip outputs.** Bind a snapshot to a committed state cursor and
   resource identity, then bind each of eight strip outputs to that snapshot.
   Define scratch/output account roles, independent strip ordering, and
   all-or-nothing publication rules.
6. **Close and rent.** Add lifecycle rules for DCG-created session and view
   accounts: refuse close while any session, transition, or view is live, and
   return rent to the recorded authority.
7. **Image identity and resources.** Bind the complete app manifest and DCG
   source identity into build provenance. Replace the current `u64::MAX`
   compute placeholders with measured, workload-appropriate resource limits
   before advertising a deployable contract.

## Verification

**Measured:** `cargo test --profile fasttest --offline --locked`, `cargo
check`, and `cargo fmt --check` passed for `stunts/doom-dcg`. The SBF image and
the existing ProgramTest driver built successfully. The full linked-image
ProgramTest run passed its Doom gate; the exact golden comparison including
image identity did not pass, and the projected behavior comparison did.

**Not verified:** execution of a DCG kernel through the SVM entrypoint,
complete app build provenance, live-chain CU, Tokyo behavior, relay behavior,
program upgrade, or DCG-specific malformed-account and atomicity controls.
