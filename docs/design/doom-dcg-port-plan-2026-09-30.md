# Doom on DCG: port design and staged plan — 2026-09-30

**Status: designed plan, not implementation evidence.** This note maps the
current Doom demo onto the DCG graph/sweep architecture and stages a later port.
It does not change revision 8, define frozen v2 bytes, or qualify a Doom kernel.
The standalone Doom demo remains the comparison until a Doom-owned DCG image
passes the local gates and matches the demo's testnet performance target.

Labels follow the repository convention: **measured** names a retained
artifact; **estimated** is planning arithmetic; **designed** is a proposal;
**open** is unresolved or unmeasured.

## Recommendation

Finish revision 8 and extract DCG first, as the owner has ordered. Then define
the versioned graph/sweep consensus runtime and link Doom kernels statically
into a Doom-owned image with DCG core. Keep the current program and Python
sender as the demo until the new image passes ProgramTest goldens and a separate
testnet comparison at a new program address.

Prepare the shared input oracle and ProgramTest-versus-SIM2 goldens while the
extraction proceeds. These are bounded tasks that do not need the extracted
crate. Do not start the stateful on-chain port before the v2 transition contract
and Doom's pointer-free state ABI agree.

## Current baseline and limits

The comparison is the normal four-buffer relayer path, with prediction off.
In the 2026-09-28 testnet run, settings `ADVANCE_CU_LIMIT=1,400,000`,
`SEND_RATE=60`, and `MIN_CURSOR_STEP=6` produced **3.354 active new pictures/s,
29.377 tics/s, and 3.533 frames/s** (**measured**, one 68.580-second active
window). It had no 286, 297, or 308 refusals. The [send-pipeline
experiment](../experiments/doom-send-pipeline-2026-09-28.md) identifies a
sender-side serial submit loop; it does not establish a chain-wide ceiling.
The later [predictor experiment](../experiments/doom-predictor-relayer-2-2026-09-28.md)
was slower, so prediction stays disabled.

The dispatch brief supplies **about 6.8M CU per picture** as the planning
comparison (**estimated** here; this note did not find one retained receipt
that measures that combined quantity). A retained testnet render census reports
**5.883M CU for the eight strips plus snapshot** (**measured**, [render-throughput
experiment](../experiments/doom-render-throughput-ceiling-2026-09-25.md)), before
simulation work is included. Use 6.8M only for the initial overhead budget;
measure a same-image, same-cursor end-to-end picture before making a final
comparison.

A rendered picture uses one committed-state snapshot and eight 40×200 strip
renders. Simulation advances and input deposits amortize across pictures; get
their actual count per picture from the retained run journal. For a simple
sensitivity case, count one eight-tic advance, one snapshot, and eight strips
(ten instructions, not counting input deposits). The caller-supplied target of
no more than 10% added compute then allows about **680k extra CU per picture**
(**derived** from the estimated 6.8M comparison). The eventual testnet session
must also report requested CU separately from consumed CU, since the
[render-throughput experiment](../experiments/doom-render-throughput-ceiling-2026-09-25.md)
shows Fogo schedules using requested compute limits.

## What DCG provides today

| Needed by Doom | Present in the repository | Port status |
|---|---|---|
| **Kernel contract and dispatch** | `chain/dcg-program` has a `KernelImpl` interface, authenticated spans, and a compiled-in registry. Current registrations are closed and Basanos-oriented; the access-form rows for Doom are parser shapes, not a Doom implementation. The extraction census identifies `kernels/mod.rs` and `lib.rs` as mixed assembly boundaries. | The reusable interface and static application registry seam are **designed for extraction v1**. There is no DCG-owned Doom sim, snapshot, or strip kernel. A general stateful kernel ABI remains **missing** until v2. |
| **Graph and sweep plan** | Revision 8 has a fixed document/template lifecycle and closure trees. It has no app-neutral typed graph, arbitrary sweep plan, or consensus-only sweep lifecycle. | The three-layer graph, plan, and backend model is **designed**, with no accepted bytes or runtime. Doom uses v2, not a reinterpretation of revision 8. |
| **Input stream and session** | Doom's standalone `SIM/2` implements write-once input slots, cursor-bound advances, reset, and a session meta. DCG has no corresponding stream code. | DCG `DSM1`/`DSB1`-style stream/session rules are **designed only**. Keep the input buffer's own session lifetime; share one input layer with the oracle and typed-decision consumers. |
| **Engine state across steps** | Doom's own program has a 9,734,160-byte context, `ST1` state and `MP1` mirror. DCG `Execute` authenticates one entry at a time; it has no Doom state schema, input/output state transition, or persistent stateful kernel runtime. | Doom must declare a versioned, pointer-free state schema and import/export rule. Generic stateful transitions are **designed only**. Do not hash the full engine context per tic. |
| **Views and output accounts** | Doom has `RSN/3` render snapshots and strip accounts. DCG already has output regions and write-span mechanics, but no view/session lifecycle. | Keep a view as a declared output region/write span with the `view` flag and committed ABI ID. The owner decided **no new record type**. Generic admission and close rules are **designed only**. |
| **Rent reclaim** | DCG revision 8 closes its own document/template accounts, not every account a workload creates. Doom has no close instruction for its session and render accounts. | The owner decided revision 9 must reclaim every DCG-created account to its recorded authority and refuse close while its session is live. This general capability is **not implemented**. |
| **Client sequencer** | Several private Python senders and a TypeScript SDK transport exist. The Doom app has a Python sender pipeline behind a Node relayer; neither is a DCG sequencer. | A durable sequencer, recovery journal, and account-state reconciler are **designed only**. The dispatch records the current owner choice as **Python first**; Doom's sender is its last migration. The separate client proposal still says TypeScript first, so reconcile that note before implementation. |

The sibling repository `~/sith/toys/crypto/durable-compute-graph` exists at
local commit `3b2b50e`. Its README describes a first extraction slice: generic
SHA-256 and commit-Merkle code, portable revision-8 vectors, and a focused
PT1X create/unpublished-close processor with an offline lifecycle harness. It
is not a replacement for the complete revision-8 program: document, admission,
challenge, settlement, output-producer and other mixed unified handlers remain
in Basanos, and it contains no graph/sweep runtime or Doom kernel. The
[extraction census](dcg-extraction-census-2026-09-29.md) remains the dependency
plan; complete its current-byte adapters, tests, standalone build/CI and
manifest before treating extraction v1 as ready for the port.

## Target shape and instruction mapping

The target follows the graph note: a **Doom-owned SVM program image links DCG
core and the Doom kernels at build time**. The manifest binds the kernel
semantic versions and ABIs to that image. There is no runtime code loading or
general CPI to an arbitrary kernel. Doom's large engine state stays in Doom's
application image; DCG supplies graph, sweep, input/session, bounded account,
output, and close mechanics.

The port maps the present operations as follows. Existing instruction names
describe the source behavior, not a promise that their bytes carry over to the
new image.

| Current Doom operation and accounts | DCG role | Doom-specific behavior that stays in the app image |
|---|---|---|
| `RSR/2 BUILD`, `INIT`, and `CLEAR`; static-resource account, program data, WAD slices, and payer | One-time graph resource setup and sealed boundary inputs. | RI2 layout, WAD chunk order/hash, pinned context base, renderer tables, resumable build phases, and build authority. Preserve the actual WAD bytes and account readbacks as inputs. |
| `DOOM-STG/1` seals RI2 resource, WAD (`WC1`), start-state (`ST1`), mirror (`MP1`), and controller (`CI2`) accounts in chunks, writing the magic header last | Generic bounded upload/supply and session input setup. | Doom account magics, WAD/resource schema, controller fields, capacity, and when setup is complete. The WAD stays private and supplied for the first profile; shared-owner provenance waits. The E2R continuation is a separate account, initialized and advanced by `E2R/5`, not staged by `DOOM-STG/1`. |
| `E2R/5` PREPARE/STEP reconstructs the context in eight phases; context, WAD/resource, `ST1`, `MP1`, controller, frame, continuation, authority | A bounded state initialization transition with explicit progress and an image-bound completion condition. | The eight reconstruction phases and C engine import/rebind logic. Keep internal pointers invocation-local or at the app's explicitly pinned account mapping; never serialize runtime pointers as portable state. |
| `SIM/2 INIT`, `DEPOSIT`, `ADVANCE/4` (or the current cursor-bound variants), `RESET/9`; context, `ST1`/`MP1`, session meta, input buffer, authority | Session creation, indexed input stream, deterministic bounded state transition, cursor progression, reset, and shared sequencer boundary. | Doom button validation, tic meaning, halt conditions, `G_Ticker`, input consumption and game state. The v2 transition must name state before/after versions and its bounded tic range. |
| `SIM/2 SNAPSHOT` and `RSN/3 SNAPSHOT-LIVE`; SIM2 state/meta plus snapshot buffer | A typed view-producing transition that reads a committed state version and writes a declared output region. | Doom render projection, RSN header, WAD/resource binding, snapshot identity and rendering semantics. Keep any Doom-required view hash as app work, not a DCG default. |
| `RSR/2 RENDER-BUFFERED` and `RSN/3 RENDER`; resource, snapshot buffer, scratch, strip output | Eight dependent view-output writes, parallel only when each uses disjoint scratch/output accounts and the same committed state version. | Doom's renderer, strip geometry, pixels, strip headers and exact frame assembly. Browser rendering stays off chain. |
| No current Doom close; accounts include context, buffers, session records, snapshots, strips and scratch | Revision-9 general close path for every DCG-created account. Halt first; refuse close while a session is live; return rent to the recorded authority. | Doom decides the session end and which app resources are eligible to close. No resource is reclaimed while a render or transition can still read it. |

The logical graph is `resource setup → session init → input deposits → bounded
tic sweep → view snapshot → 8 strip outputs`. Each strip names the snapshot's
state cursor and WAD/resource identity. The next tic sweep depends on the
previous state transition; independent strips may run together. The first v2
profile should be a static DAG with explicit bounded state transitions, not an
implicit feedback cycle.

Keep the existing renderer snapshot/hash behavior during the first port if it
is required for safe buffer reuse and strip binding. Do not add generic
write-record/provenance hashing to consensus sweeps by default. Commit graph,
plan, kernel/image and boundary schema identities at setup. Any optional
per-session input chain or full-state anchor is explicit app-level work and
must have its own CU result.

### What stays in Doom

Doom retains the C engine, WAD and map identity, `ST1`/`MP1` schemas, controller
bytes, button and halt policy, context import/rebind/export, tic and render
semantics, view hashes needed by its current renderer, strip layout, and browser
presentation. DCG owns the generic typed ports, graph/plan identity, stream
mechanics, sweep ordering, mode finality, bounded authenticated account access,
and account close/reclaim mechanism. The Python app adapter owns Doom account
construction, session policy, signer selection, cost caps, payment, user-facing
progress, and the mapping from RPC/account state to a resumed run.

## Compute and rate risk

### Measured anchors

- **Measured:** one five-account DCG `Execute` refusal used at most 70,623 CU
  before kernel dispatch in the retained authentication census. This is not a
  Doom-shaped upper bound. The honest Fly entry total in that source profile
  was 1,108,396 CU.
- **Designed/estimated:** the [earlier DCG stream proposal](dcg-consensus-mixed-mode-2026-09-23.md)
  estimates about 4–6k CU for a bound `ADVANCE` after one full bind. It keeps
  owner, key, length,
  writable-bit, authority, cursor, and slot checks, and omits repeated route/PDA
  authentication. This estimate has not run in DCG code or in Doom.
- **Measured:** Doom's [SIM/2 experiment](../experiments/doom-staged-sweep-2026-09-22.md)
  used up to 1,123,639 CU for an eight-tic sweep; the retained testnet sweep
  used up to 1,079,580 CU. The exact values depend on image and workload.
- **Measured:** the testnet strip consumed 708,437–731,388 CU in the [render
  throughput experiment](../experiments/doom-render-throughput-ceiling-2026-09-25.md);
  the snapshot and eight strips totaled 5.883M CU.

### Scenarios and guardrail

Applying 70,623 CU to all ten current per-picture instructions gives about
706k CU, or **10.4%** of the 6.8M planning comparison (**derived sensitivity
case**). It would miss the 10% target. It is not a prediction: the measured
case used five accounts, the Doom render entries touch a different account set,
and an account count alone does not scale CU linearly. It shows why full
descriptor and route authentication on every tic and strip needs an early CU
probe.

If all ten instructions instead stayed within the stream proposal's 4–6k
bound-check estimate, they would add 40–60k CU, or **0.6–0.9%** per picture
(**estimated sensitivity case**). The estimate was made for a bound stateful
advance, not for eight render calls, so the render share remains **open**.
Amortize `STREAM_BIND` over the session and report its CU separately.

The plan's hard gate is measured overhead at or below **680k CU per picture**
against a same-cursor baseline measured on the same image and runner. At the
estimated 6.8M baseline, a compute-bound rate would fall from 3.354 to about
3.05 active new pictures/s at the full 10% overhead allowance (**derived**,
assuming throughput scales inversely with CU). The corresponding measured
baseline tics rate is 29.377 tics/s; its separate `ADVANCE` CU must be reported
so render cost is not mislabeled as a tic cost.

The hash budget is the main controllable risk. Consensus mode re-executes the
accepted transition on every validator, so it does not need optimistic dispute
replay or default provenance hashes. Keep full-context hashes, per-tic input
chains and write records out of the hot path. Preserve only hashes the Doom
snapshot/render ABI actually needs, and attribute those to the app baseline.
Record CU for bind, advance, snapshot and each strip separately. A native host
or ProgramTest pass does not supply SBF CU or testnet throughput evidence.

This compute model does not guarantee the picture-rate result. The retained
[buffer-depth census](../experiments/doom-render-buffer-depth-2026-09-26.md)
found a 0.244-second serial submit cycle and concluded that the sender, rather
than buffer depth, bound that workload's measured frame rate; it estimated
sustainable render capacity at 5.3–6.0 fps beside a sweep. Preserve the current pipelined sender behavior, confirm each
dependency at the boundary it needs, and compare both CU and wall-clock rates.

## Staged plan

Each row is a **designed 4–8-hour work round**; elapsed time and final totals
are not measured. The v2 runtime itself remains the graph note's low-confidence
**estimated 4–8 engineer-weeks**, excluding the Doom adapter, client sequencer,
and deployment. Split that runtime into the small acceptance rounds below.

| Round | Work and gate | Depends on extraction? |
|---|---|---|
| **0. Baseline packet** | Freeze the exact demo image/source identity, normal sender settings, current account shapes, and a reproducible 60–90 second comparison recipe. Include new-picture hashes, tics, frames, requested/consumed CU, latency, refusals, and fees. Use the 2026-09-28 3.354/29.377 run as the initial baseline, but retain the full receipt path. | No. Read-only prep; may proceed while revision 8 and extraction are active. |
| **1. Shared typed input** | Build one versioned input decoder and fixture path used by the Doom oracle, the later on-chain harness, and typed-decision/oracle consumers. Pin session/cursor, player order, fixed-width command, reserved zeros, and input range. Keep Doom indexed input and oracle append policy as policies over the same layer. | No. Q6 is already decided. No DCG handler changes. |
| **2. Local behavior goldens** | Run the real standalone Doom SBF program in local ProgramTest against the same captured WAD and typed input sequence as SIM/2. Compare canonical `ST1`/`MP1`, cursor and state bytes at each sweep boundary; compare snapshot and all eight strips. Include malformed inputs, wrong cursors/account roles, duplicate deposits, and atomic refusal controls. ProgramTest executes the actual handler; a Python handler mirror is not a substitute. | No. Q5 says this is the primitive gate. Freeze EX2/state ABI first. |
| **3. Kernel adapter contract** | Write the Doom stateful-kernel adapter against the extracted kernel contract: stable kernel IDs and versions, port layouts, state schema, image identity, resource limits, and compile-time registry. Keep this as an app adapter; do not add generic CPI or alter revision-8 dispatch. | Design can start now. Integration waits for extraction v1's kernel seam. |
| **4. Freeze extraction v1** | Complete the standalone DCG repository, build/tests, current-byte adapters, and static app registry seam; record the frozen v8 source/image identities. No graph or sweep bytes enter revision 8. | Yes. Extraction census steps 1–9; v8 deployment/freeze precedes the code move. |
| **5. Python sequencer vertical slice** | Implement the durable Python plan/journal/reconcile core and one independent-write vertical slice. Persist exact signed bytes before send; recover ambiguous fate from signature plus app-supplied chain-state postconditions; keep endpoint pacing shared and bounded. | Yes. After the extracted package stabilizes. The client note's TypeScript-first recommendation must be updated to the owner's Python-first choice before work starts. |
| **6. V2 graph and consensus backend** | Freeze graph root, sweep plan root, port layouts, bounded state transition and output commitment rules. Implement the backend first against a small pure kernel, with exact-once sweep coverage and cross-language golden bytes. Test honest transaction orders and malformed/cheating plan inputs. Keep dependent sweeps behind upstream finality. | Yes. After extraction. This is multiple 4–8-hour rounds; the graph note estimates 4–8 engineer-weeks overall. |
| **7. Doom simulation adapter** | Link Doom sim kernel(s) into the Doom-owned DCG app image. Run one tic, then bounded multi-tic sweeps, through the real DCG instruction handler. Compare every boundary to the SIM/2 goldens; check an altered command changes the actual engine state. Exercise bad schema, stale cursor, bad account order/rights, unknown action IDs, and rollback atomicity. | Yes. Rounds 1–3, 4 and 6. No sender migration yet. |
| **8. Doom view adapter** | Add a typed snapshot output and strip output kernels. Run the actual renderer from the committed state version; compare header and pixels for all eight strips. Test disjoint scratch/output accounts, out-of-order independent strips, wrong snapshot/state version, and torn-frame refusal. | Yes. Round 7 and the v2 output/view contract. |
| **9. Close and rent gate** | Close every account created for a DCG Doom session only after halt and after no live transition/view can reference it. Refuse close during a live session, bind the refund to the recorded authority, and read back zeroed/closed state and rent destination. | Yes. Revision-9 account close/reclaim semantics and rounds 7–8. |
| **10. Local throughput parity** | Build a reproducible production-shaped SBF image, then run matched SIM/2 and DCG ProgramTest/SBF cases with the same image settings and input tape. Attribute bind, sweep, snapshot, strip and close CU. Require measured added compute ≤10% per full picture, no unexpected refusal, and the eight pixel goldens. | Yes. Rounds 7–9. Local ProgramTest/SBF is the gate before testnet. |
| **11. New-address testnet comparison** | Use a fresh program address and fresh session accounts; do not replace the demo. Exercise the shared Python sequencer through a Doom test adapter and compare against the retained demo recipe for active new pictures/s, tics/s, frame rate, end-to-end latency, requested/consumed CU, fees, and account rent. Require at least 90% of the demo's measured 3.354 active new pictures/s and 29.377 tics/s under the matched workload, with byte-exact frames and no unresolved fates. | Yes. Only after rounds 0–10. Testnet result is live performance evidence; it does not replace the local correctness gate. |
| **12. Last sender migration** | Move the Doom sender onto the accepted Python sequencer after its journal/resume gates pass. Keep the current relayer and demo available until the new address passes the comparative session. | Yes. After round 11 and the Python client contract. |

The extraction dependency is explicit: rounds 0–3 are preparation and local
baseline work; round 4 is the extraction; round 5 is the Python sequencer; round
6 is DCG v2; rounds 7–9 are the Doom workload gates; round 10 qualifies local
SBF performance/correctness; round 11 is the new-address testnet result; round
12 is the final client migration. A program/session address changes with the
new app image, and all of its PDAs and rent accounts must be recreated.

## Owner choices and current decisions

The following decisions are already recorded and this plan uses them:

- The engine application imports DCG core; the Doom-owned image statically
  links the Doom kernels. Keep the full 9.7 MB engine path out of CPI. A small
  bounded CPI for unrelated kernels is not needed for this Doom port.
- The input buffer has its own session lifetime; indexed Doom input and append
  oracle input use one shared layer.
- A view is a declared output span with a view flag and ABI ID; no new view
  record type.
- Revision 9 supplies close-and-rent-reclaim for every DCG-created account,
  with close refused while a session is live.
- Local ProgramTest-versus-SIM2 goldens are the primitive gate. A testnet run
  adds live evidence, not a substitute for that gate.
- The graph/runtime does not require a PT2P compiler change. Any PT2P compiler
  selection remains explicitly `compiler_version=1`.
- The current sequencer direction is Python first, despite the TypeScript-first
  recommendation in the 2026-09-30 client transport note. Update that note
  before sequencer implementation; do not build duplicate retry engines.

Before freezing v2 bytes, the graph note still calls for these owner choices.
Recommendations for the Doom profile are:

1. **Graph language:** static DAG plus bounded state transitions. This is
   enough for Doom's repeated tics and avoids an implicit unbounded cycle.
2. **Sweep cover:** exact-once coverage of required nodes in the first profile.
   Refuse overlap/repeated execution until ownership of shared outputs is
   specified.
3. **Dependencies:** dependent sweeps consume only finalized upstream output;
   no rollback of downstream consensus state. Independent strip outputs may
   proceed in parallel.
4. **Commitments:** SHA-256 for graph/plan identities and optional explicit
   state anchors. Consensus execution itself stays hash-free by default; no
   per-tic WRP1 or full-context hashing. Defer commitment translation and ZK
   schemes until an actual later backend needs them.
5. **Application hook:** Doom's first profile has no bond settlement hook.
   Keep the computation result under DCG and add no game payout policy to the
   generic backend. Bind an app hook only when a concrete Doom policy needs it.
6. **Vocabulary:** add “resolution mode” for consensus/optimistic/proof
   selection per sweep while preserving “execution profile” for the existing
   native/recompute/leaf distinction. Update terminology before freezing the
   plan bytes.

The earlier stateful-workload draft suggests a narrow registered CPI for small
calls, while the 2026-09-30 graph target is static build-time application
linking. This port follows the newer static image target. Keep a future small
CPI interface as separate work if an application demonstrates a need.

## Sources and evidence boundary

Read for this plan: the graph/sweep and client/transport designs, the 2026-09-29
extraction census, the stateful-workload decisions Q1–Q7, mixed-mode design,
Doom/DCG integration map, staged sweep and experiment, render snapshot/scene
cost/relayer notes, the 2026-09-26–28 Doom buffer/send/predictor experiments,
the current Doom SBF source and `scripts/doom_play.sh`, terminology, decisions,
evidence register, operations guide, and engineering learning log. The
durable-compute-graph repository was not present at its expected path.

No code, test, build, audit-box run, Tokyo access, RPC, or chain transaction was
performed for this note. No new evidence row is warranted. In particular,
DCG v2 bytes, Doom's DCG handler behavior, per-strip DCG CU, close/refund CU,
and matched DCG testnet picture/tic rates remain **open**.

**Recommendation: keep this plan, finish revision 8 and DCG extraction first,
then port Doom as the first fully on-chain consensus-mode workload; retain the
current demo until the new-address comparison passes.**
