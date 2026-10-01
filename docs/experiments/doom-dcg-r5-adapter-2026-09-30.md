# Doom DCG round 5: stateful v2 adapter boundary

**Result:** the composed v2 dispatcher and its lifecycle refusals run in local
ProgramTest. The real Doom one-tic, sweep, and render golden gate remains open.
The local adapter now records the exact fixed-address engine mismatch and
refuses before it can claim or commit a Doom result.

Labels follow the repository convention: **measured** names a retained
artifact or experiment result, **estimated** is planning arithmetic,
**designed** describes an interface or proposal, and **open** is unverified.

## Pinned inputs and committed adapter

**Measured:** this work starts from the round-4 adapter merge and consumes
DCG read-only at `4d1446f67a483ca3d5c05b7deea00fb8f5554168`. The checkout lives
under ignored `dcg-repo/`; it is not committed to Basanos.

**Designed/implemented:** `doom-dcg` now declares one composite
`basanos-doom-v2` kernel, application version 2, Doom state schema 1, a
four-byte input/output port, and nine render roles (one snapshot and eight
strips). The state layout declares a 9,734,160-byte engine context,
126,976-byte ST1, and 65,536-byte MP1, for 9,926,672 bytes total. Its v2
indexed stream is configured for up to eight transitions per ADVANCE; each
step declares 175,000 CU, or 1,400,000 CU for eight steps. These are manifest
limits, not measurements of Doom execution.

The adapter accepts exactly four input bytes and rejects the Doom
`BT_SPECIAL` bit. It binds the context span through an invocation-local
`AtomicPtr` on a stack-owned kernel value and clears the pointer after each
callback. The app manifest kernel is immutable. This is designed to keep an
address out of committed state and mutable global storage.

The fixed-address refusal shim compares against the same `0x400000060` value
as the pinned Doom C guard; the focused manifest test asserts that literal.

**Measured:** `pack-dcg-resource.py` validated the retained RI2 image and five
WAD account payloads and produced a 4,457,108-byte `DOOMDGR1` resource
envelope. Its SHA-256 is
`d9cb710813d965c62eb919a477e3ea571d7ad986d82a07f611969bc8f09b9cf0`; the
packed WAD digest matches the retained golden. The file is retained with the
dispatch artifacts. The SBF initialization callback checks the supplied
resource key, schema, commitment, envelope shape, and WAD digest before it
examines state spans. The ProgramTest dispatcher case does not call that
callback, so this validation path has not been exercised in SBF.

## Engine and v2 boundary

The pinned Doom C code still requires `DOOM_CTX` at `0x400000060` and checks
that address with `doom_ctx_guard`. DCG v2 passes the engine through a mutable
state span after its session and stream accounts, so the span's invocation
pointer does not satisfy the existing address guard. The adapter binds and
clears that actual pointer, then deliberately refuses when it differs from
the legacy address. Initialization also lacks a Doom new-game import routine
that accepts an arbitrary context span. The local shim therefore stops
before touching Doom state; it does not fake an initialized context.

The DCG v2 transition API returns success or refusal and does not expose the
Doom engine's `Halt(code)` result as a committed prefix. Doom can halt before a
tic; the standalone runner records the halt at its current cursor. In v2, a
multi-step ADVANCE cannot report that result while committing only the prefix
of tics that ran. `HALT_SESSION` exists as a separate instruction, but the
current callback cannot pass its halt reason and consumed cursor to it.

The view callback receives state spans and an output chunk. It does not
receive the authenticated RI2/WAD resource or writable render workspace
needed by the pinned RSN renderer. The manifest describes a 65,536-byte phase
and a 1,400,000-CU phase ceiling; neither is a measurement of a Doom render
phase. The existing snapshot and strip standalone calls exceed that ceiling
and were measured under a 30,000,000-CU local ProgramTest limit in round 2.

## ProgramTest evidence

**Measured:** the composed SBF app image loaded by the retained test is
1,276,200 bytes, SHA-256
`dd284c27ca9df22f46771e218f194fb9f90b3b8966f206ff37b886718a9e759f`.
The ProgramTest case opens a v2 session and stream, then checks these
successful or adversarial transactions:

| Case | Result | CU |
| --- | --- | ---: |
| Open session | success | 6,832 |
| Create indexed stream | success | 9,861 |
| Write input as wrong authority | refused with 2322 | 2,473 |
| Write input at index 0 | success | 3,422 |
| Write the same index twice | refused with 2327 | 2,783 |
| ADVANCE at stale cursor 1 | refused with 2329 | 2,554 |
| Close stream while session is live | refused with 2325 | 2,516 |
| Create a one-byte state span | success | 8,583 |
| Initialize after corrupting its schema version | refused with 2331 | 3,099 |

CU values are from the single retained local run, not a cross-machine or
standalone-Doom comparison. The wrong-state-version case uses
`ProgramTestContext::set_account` to alter the state header after creating the
span. It tests the SBF schema-version refusal; it does not emulate a normal
protocol transaction. The test does not initialize the resource bundle or
execute a Doom tic.

The targeted host adapter suite passed all 3 tests. They validate the
composite manifest, reject malformed resource-envelope headers, and check
that the invocation-local binding is cleared and the fixed-address mismatch
refuses. The composed ProgramTest case passed all 9 listed operations.

## Production image identity gate

**Measured:** the documented reproducible production build ran twice from
committed source with `sim-transport`, pinned platform-tools v1.51, and the
pinned upstream scratch. The pre-change and post-change images are both
1,067,784 bytes with SHA-256
`7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130`.
The post-change build matched the expected hash, both builds matched each
other, there were zero stack-frame errors, and all 82 repository and 137
upstream manifest inputs verified without mismatch. The recorded source
manifest hash changed from
`ef1ddbd8fb23acdbe79de82dd40c9c6266c40772e0c91e8c5e3eca9dfa5bb8c1` to
`e87b15a0eb5fdd70d34778b39e2575b3c2be1887c46315485d73baf1767babe9`; the
production SBF bytes remained identical.

The only `stunts/doom-sbf` source edit is the permitted cfg guard around its
existing entrypoint macros. The other edit there adds the packaging-only
`no-entrypoint` feature; the production `sim-transport` build does not enable
it.

## Gate status and proposals

**Open:** no full WAD-to-context initialization was measured. The composed
ProgramTest did not reach `initial_state_spans_with_resources`, and the
fixed-address engine mismatch prevents a valid initialization shortcut. The
round-2 byte goldens still have no round-5 boundary, snapshot, or strip
comparison. No Doom sweep, render phase, or same-case compute comparison ran;
the 10% full-picture overhead gate was not evaluated. The one-byte state
control above is only an account-header refusal test.

**Estimated:** DCG v2 provisions child state in 8,192-byte growth operations.
After state account creation, the three declared Doom spans would require
about 1,210 growth operations to reach their full declared lengths. This
count is derived from the pinned span lengths and growth step; it is not a
measured transaction-time or cost result.

These are **designed proposals** for the DCG/Doom boundary:

1. Give Doom an explicit invocation-context ABI and pass the bound span base
   through every engine subsystem that currently expands `DOOM_CTX` to a
   fixed address. Require bind/clear symmetry and keep pointers out of
   committed bytes.
2. Add bounded, resumable resource verification and Doom initialization. Bind
   every chunk to the session resource commitment, specify the final WAD/RI2
   roots, persist an initialization cursor, and cap each phase under the
   runtime compute ceiling. Measure a full init only after this interface is
   available.
3. Let phased render callbacks read authenticated resources and write an
   explicit bounded workspace. Specify deterministic snapshot/strip chunk
   order, frame/cursor binding, and atomic final publication.
4. Add a transition stop result that carries the Doom halt code and consumed
   prefix so ADVANCE can commit a bounded prefix and halt status atomically.
5. Consider larger bounded state-growth steps or another rent-funded
   provisioning path to avoid roughly 1,210 separate growth operations for
   this state shape.

No evidence-register row was added because no Doom-on-DCG capability result
was measured. No live chain, Tokyo, relayer, program upgrade, or R15 REBUILD
operation was used.

## Retained artifacts and commands

Dispatch artifacts are under
`out/runs/doom-dcg-r5-2026-09-30/`, including the pre-change receipt, two
post-change reproducibility receipts (the last is from final source commit
`5faf22d6`), the composed `doom_dcg.so`, its ProgramTest log, and the packed
resource envelope.

The focused commands were:

```sh
CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r5-target \
  cargo test --offline --manifest-path stunts/doom-dcg/Cargo.toml \
  --no-default-features --tests

DOOM_DCG_SBF_DIR=/private/tmp/doom-dcg-r5-sbf-out \
CARGO_TARGET_DIR=/private/tmp/basanos-doom-dcg-r5-target \
  cargo test --offline --manifest-path stunts/doom-dcg/Cargo.toml \
  --no-default-features --features sbf-programtest \
  --test programtest_dispatch -- --nocapture

DOOM_UPSTREAM_SEED=/private/tmp/doom-dcg-r5-upstream-seed \
SBF_SDK_PATH=/private/tmp/basanos-sbf-sdk-v151-20260920 \
  sh stunts/doom-sbf/scripts/build-sbf-reproducible.sh "$PWD" HEAD \
  "$PWD/out/runs/doom-dcg-r5-2026-09-30/production-after-replay" \
  sim-transport /private/tmp/basanos-doom-dcg-r5-production-target \
  --expect-sha256 7d3b63efee0bbbb9926c45dac4db51aaf033fd7f3d61ef33162de6f8c4d1b130
```
