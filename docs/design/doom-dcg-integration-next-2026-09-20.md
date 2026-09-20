# Doom → DCG integration map — 2026-09-20

Status: **bounded read-only architecture/code audit; design decision, not an
implementation or execution result.** This note recommends an incremental
migration boundary. It does not change Doom source, the DCG program, the Tier C
path, the evidence register, or `DIRECTOR_HANDOFF.md`.

## Decision

**Migrate incrementally now; defer claiming Doom execution in DCG.** The next
work should close the EX2 state/input ABI and the smallest qualified DCG kernel
bridge, then measure one real Doom tic and one real render strip through
`Execute`. It should not attempt a whole-frame or one-transaction migration,
shared WAD ownership, or a generic plugin mechanism before those gates.

The current DCG descriptor builders are useful shape declarations, not a live
Doom profile. Form IDs 40–42 are present in the access-form table, but the
compiled DCG implementation registry contains only the Fly tile and the
machine-neutral leaf hash. A Doom descriptor therefore reaches
`EXEC_NO_IMPLEMENTATION(284)` today; any pass that only hashes a wrapper or
parses the descriptor would not be Doom execution.

The recommended first profile is:

1. one private, supplied WAD resource set;
2. a versioned pointer-free EX2 state import/export boundary;
3. a deterministic typed controller input for one tic;
4. one real `Execute` tic producing next-state bytes; and
5. one subsequent real `Execute` render-strip instruction producing
   `DOOM-FB1`-typed output bytes.

Only after those operations pass through the actual qualified image should the
work decide whether to add the remaining strips, a successor descriptor, or a
shared WAD owner. This is a **designed** sequence. No Doom `Execute` result is
measured in this checkout.

## Evidence boundary and labels

This audit inspected the current local source and retained notes only. No
build, model load, network call, validator, deployment, key operation, or live
write was performed. Existing numbers below are attributed to retained
artifacts; they are not new measurements.

| label | meaning in this note |
|---|---|
| **measured** | explicitly reported by a retained source/experiment/receipt |
| **estimated** | planning arithmetic or an estimate in an existing note |
| **designed** | proposed integration behavior |
| **open** | not answered by source inspection or an existing result |

The current DCG local-SBF boundary is the retained main-image gate, not the
historical alpha deployment: two wrapper builds produced an identical
553,920-byte image, the packet census reached 1,133 B of a 1,232 B envelope,
and the honest fixture ran **1,660/1,660 Execute entries** with **82/82**
terminal comparisons; maximum Execute compute was **998,296 CU** (**measured**,
M1093). That is a Fly/local-ProgramTest boundary, not Doom capability, not
validator behavior, and not deployment. The historical 132/132 composition
and recorded deployment must not be used as a Doom acceptance assumption.

The Doom-side retained boundary is narrower: the host V2 controlled re-entry
gate passes two tics with exact state/frame equality (**measured**), while
`DOOM-EX1` still refuses at `Custom(230)` and SBF `init`, `load_level`, `tic`,
and `render` remain unimplemented/unmeasured (**measured/open** in the retained
notes). The 1,025-step `DOOM-BU1` WAD hash continuation is a real SBF
continuation (**measured**), but it is not an engine tic or render.

## What DCG actually supports today

### Descriptor, regions, and state

The normative descriptor is the byte grammar in
[`dcg-descriptor-v1.md`](../spec/dcg-descriptor-v1.md). It commits machine,
kernels, regions, placement, routes, schedule, verification policy, closure,
supply, and successor rows. The descriptor has no dynamic kernel/plugin table;
the kernel table declares which compiled implementation a document asks for.

The actual lifecycle is split across these records and instructions:

| concern | current implementation | consequence for Doom |
|---|---|---|
| descriptor bytes | `desc_upload::{process_open,process_alloc,process_upload,process_finish}`; DCD1 and chunk PDAs | Doom bytes must be uploaded and sealed through the real descriptor lifecycle; a caller-supplied descriptor digest is not enough |
| descriptor seal state | `seal::{process_begin,process_step,process_finish}`; BDS2 at `dcg-state` | BDS2 binds descriptor digest, authority, stage/cursor, route-root fold, terminal fact, and the image hash recorded at seal |
| supplied window | `supply::{process_chunk,process_seal}`; DSW1 tail | WAD bytes must be written through supply instructions and read back as sealed account bytes |
| region root | `region_seal::{process_seal,process_freeze}`; DRS1 | a private WAD can be sealed without external owner provenance; a shared WAD cannot |
| execution | `execute::execute` | reads DCD1/chunks/BDS2, authenticates the entry, resolves placement, borrows only declared spans, and dispatches a compiled kernel |
| successor | `successor::{process_export,process_successor}`; DXP1/DSC1 | a next-tic state relation needs explicit exported/imported regions; `successor_imports=()` is not a state chain |

`Execute` accounts are DCD1, descriptor chunks, BDS2, the entry's ordinal
accounts, and one owner DRS1 for each shared-window slot. It requires the
descriptor and BDS2 to be read-only, checks the image hash carried in the
instruction against BDS2, authenticates routes with `access::authenticate`,
requires `consensus` mode, and then calls `kernels::dispatch`. It does not
write a result PDA, closure, witness, or run-global state.

The kernel sees `SpanMemory`, not a logical Doom address space. The current
interface provides copying `read`, checked `write`, `hash`, and a read-only
`slice` when one authenticated span is one contiguous run. It has no public
mutable contiguous-slice/raw-pointer operation for handing a live engine a
context pointer. Its current hard bounds are **16 execution spans**, **8
placement cells per span**, and **32 entry account ordinals** (**measured/source
constants**, `chain/dcg-program/src/kernels/mod.rs`). These bounds are
compatible with a small Doom part only if its routes are explicit and its
engine adapter is designed around them; they do not make the existing context
engine callable.

### Dispatch and call mechanisms

The supported call path is currently exactly:

```text
Execute instruction
  -> access::authenticate(descriptor, entry, payload)
  -> require_consensus
  -> placement/account/owner-seal checks
  -> kernels::dispatch(machine_name, form_id, params, SpanMemory)
  -> one entry in kernels::IMPLEMENTATIONS
```

`kernels::IMPLEMENTATIONS` currently has two entries: the Fly tile and the
machine-neutral window leaf. `resolve` keys on `(machine.machine_name,
kernel.form_id)`, never on a kind number. The access registry's Doom rows
(`form_id` 40 WAD hash/init, 41 tic, 42 render-strip) are parser shapes only;
there is no corresponding `KernelImpl`.

There is no current plugin ABI, dynamic code loading, or generic kernel CPI.
The DCG image uses CPI/invocation in account-provisioning/lifecycle code where
needed, but `Execute` does not invoke a downstream Doom program. A separate
Doom program could theoretically CPI into the public `process_instruction`
entry if a future consumer ABI deliberately passed all DCG accounts and
instruction bytes, but that would still execute DCG's compiled image; it would
not cause DCG to call Doom code. No such consumer instruction, account policy,
or proof boundary exists today.

Therefore there are only two honest future choices:

* **Qualified built-in extension (recommended first).** A Doom-qualified DCG
  image carries the EX2 engine adapter and registers forms 40–42 under an
  explicit machine/ABI name. The generic DCG image and the standalone DCG
  boundary stay unchanged. The descriptor's BDS2 image binding and the
  machine/profile identity select the qualified image.
* **Explicit external extension/CPI ABI (later, open).** Specify how a
  consumer program registers or calls a Doom handler, how DCG authenticates
  its read/write set, and how the handler's output is bound to the same image
  and state records. This is new protocol work; it must not be inferred from
  the existing CPI calls or from the word “plugin”.

The first choice is a workload-specific extension, not permission for
arbitrary kernel loading. It must be an explicit qualified image policy and
must preserve the direction “consumers import DCG, never the reverse” in
[`dcg-spinout-plan-2026-09-18.md`](dcg-spinout-plan-2026-09-18.md).

## What Doom has, and what is still only a proposal

### EX1, host V2, and EX2

`stunts/doom-sbf/src/engine.rs` defines `DOOM-EX1`, version 1, a fixed twelve-
role vector: context, config, five WAD accounts, level, mobj pool, controller,
scratch, and frame. It validates account order/size/owner/writability and then
calls `doom_engine_entry_gate` in
`stunts/doom-sbf/src/doom_sbf_engine.c`; that function returns
`ENGINE_ACTION_POINTERS_UNRESOLVED` (230). This is an intentional fail-closed
plan boundary, not a callable engine.

`stunts/doom-sbf/src/doom_sbf_ctx.c` currently binds `DOOM_CTX` to a measured
fixed account mapping and `doom_ctx_guard` refuses a moved mapping with 199.
`doom_sbf_bind_arena` records a pointer into the invocation's account/arena
mapping. Those are runtime pointers, not portable state bytes. The current
context size is 3,440,920 B plus a 24-byte probe tail (**measured**); the
generated context contains a 3,145,728 B zone (**measured**). The context image
loads and the CX1 mapping probe passes, but this does not establish a persisted
action-pointer or cross-invocation engine ABI.

The host-only `DOOM-HOST-CREENTRY-2`/V2 path is useful input, not EX2. It
demonstrates a pointer-free `DOOM-ST1` import/export, an explicit controller
cursor, and a strict successor publication rule over two native tics
(**measured**). The state-ABI proposal recommends ST1-style canonical state,
with the mobj pool as a derived checked mirror, and an explicit EX2 schema and
role map. That recommendation is **designed** and still needs the native EX2
worker's acceptance gate.

Do not silently promote `DOOM-CI2`, `DOOM-ST1`, `DOOM-MP1`, or `DOOM-PUB1` to a
chain ABI. EX2 must commit:

* state schema and exact little-endian fields;
* one authority between LS2 and the mobj pool, plus a deterministic mirror
  digest/check rule;
* typed action/kind IDs and the action-table version;
* WAD/map/resource identity and chunk interpretation;
* player ordering, gametic/cursor progression, and command width;
* engine ABI/image identity and migration/refusal behavior.

### Existing Doom descriptor builders are scaffolds

There are two builders and they must not be conflated:

| builder | current shape | audit finding |
|---|---|---|
| `src/basanos/dcg/workloads.py::doom_descriptor` | five explicit supplied WAD regions; mutable level/pool; eight scratch and eight strip outputs; forms 40/41/42; 23 placement accounts; WAD hash/init, tic, and render entries | **designed structural profile**. It has no config/controller region, no successor imports, and its current tic route writes only pool while reading level; it is not a complete EX2 state contract |
| `src/basanos/dcg/scale_workloads.py::doom_frame_descriptor` | one five-shard WAD region plus config, level, pool, eight scratch, eight strips; 24 planned accounts; forms 41/42 | **designed planner workload**. Its entries omit the WAD/config reads that real Doom init/render need, and it has no typed controller input |

Both exercise descriptor parsing, placement, access forms, and rent/planning
arithmetic. Neither has a Doom `KernelImpl`, native DCG execution receipt, SBF
Doom tic, or SBF Doom render. The existing `cu_ceiling` values in these
builders are not all Doom measurements: the retained host tic time
0.598629 ms is not a CU measurement, and the 1,157,357/1,399,850 figures are
isolated/synthetic Doom hot-loop boundaries, not `init`, `tic`, or real
per-strip render costs. Treat the builders as **open/designed** until a real
qualified image runs them.

## Required EX2-to-DCG mapping

The smallest viable bridge keeps the native engine's arithmetic and source
order, and supplies a narrow adapter around it. It does not rewrite the
working native engine wholesale.

### Regions and state

The proposed EX2/DCG profile uses the following logical regions. Names are
design labels, not allocated wire values.

| region | lifetime/account role | read/write mapping | required commitment |
|---|---|---|---|
| WAD chunks | `supplied` / private `supply_window` | read-only by form 40, 41 if needed, and 42 | exact chunk order/length, raw-vs-`DOOM-WC1` payload rule, concatenated WAD SHA-256, IWAD directory bounds, palette/map identity |
| config/resource manifest | `supplied` or committed, private storage | read-only after initialization; form 40 may write an EX2 config output in a separate step | EX2 schema, WAD root/chunk keys, map/profile/action-table identity, and any continuation cursor |
| current LS2 state | committed/mutable input | read-only for a tic instruction | pointer-free ST1-derived fields, gametic, RNG indices, action IDs, object/resource IDs, and state digest |
| current mobj mirror | committed/mutable input | read-only for a tic instruction | one authoritative source plus a deterministic mirror rule; no silent last-writer-wins |
| controller input | supplied/committed input | read-only by form 41 | typed CI2/EX2 header, expected gametic/cursor, player mask/order, fixed command fields, reserved-zero checks |
| next LS2 state | output/storage in a distinct ordinal | write-only by form 41; later read by render or successor | output header, exact state bytes, image/ABI/WAD binding, next gametic/cursor |
| next mobj mirror | output/storage in a distinct ordinal | write-only by form 41 | mirror digest and exact consistency with next LS2 |
| render scratch | `scratch` account, one per strip | write-only by form 42 | fixed size/high-water/refusal behavior; no shared writable scratch in the first wave |
| frame strip | `output` account, one per strip | write-only by form 42 | `DOOM-FB1` header with descriptor/image/ABI/WAD identity, gametic, strip ID and pixel geometry, followed by engine-produced palette-index bytes |

The distinct current/next accounts are intentional. `Execute` currently
separates read and mutable borrow sets and refuses an account that is both a
read and a write span of one entry. Although `state_mode=1` exists in the
access grammar as a declaration for persistent kernels, the current execution
borrow path is not an in-place Doom state implementation. A one-tic bridge
must either use distinct current/next state accounts or wait for an explicit,
tested state-mode runtime extension.

### Kernel calls

The qualified adapter should expose three typed entry forms, retaining the
existing proposed IDs only if their parser and image ABI are frozen together:

* **form 40, WAD/init continuation:** consume the five supplied WAD spans and
  one typed continuation record; run the actual bounded SHA-256/WAD directory
  operation; write the next config/cursor state. The existing `DOOM-BU1`
  compression and cursor logic in `stunts/doom-sbf/src/bring_up.rs` is a
  reusable semantic component. It is not permission to reuse a hash-only
  wrapper as a Doom engine result. A multi-step form must bind the cursor,
  midstate, WAD root, step count, and completion state to declared output
  bytes.
* **form 41, one tic:** read current LS2, current mobj mirror, WAD/config
  resources as required by the actual engine, and one typed controller input;
  import/rebind the runtime graph, run the real `G_Ticker`/tic path, export
  next LS2 and the checked mobj mirror. A changed command must change the
  actual state/frame behavior on a retained input, not merely change a test
  hash.
* **form 42, one strip:** read the sealed WAD/config and current LS2/mobj
  state; run the actual renderer against private scratch; write the selected
  strip's header and pixels. The output must be produced by the engine entry
  point, not by a wrapper that hashes an expected framebuffer.

The actual function signature remains the current DCG kernel interface
(`KernelImpl::run`), unless a separately specified extension is needed. The
minimum runtime extension likely consists of:

1. a checked contiguous-span/raw-slice capability for a qualified Doom kernel,
   available only when the route maps the complete span to the permitted
   account(s), with no unlisted account access;
2. an entry-local bind/unbind shim for the runtime context/arena; and
3. a typed form parser for controller, cursor, state-schema, and render-strip
   parameters.

`SpanMemory::read`/`write` copying alone does not provide the pointer-shaped
memory the existing C engine expects. The extension must not expose arbitrary
accounts or a general pointer escape. If the EX2 worker instead supplies a
small import/export shim that runs the engine against a qualified account
mapping, that shim must preserve the same route and mutation checks.

The EX2 state boundary should rebuild native pointers and action functions on
import and serialize only IDs/offsets/typed fields on export. It should not
persist `DOOM_CTX`, `arena_base`, `state_t.action`, thinker function pointers,
or pointers into WAD/account data. A changed code image or action table must
refuse or run an explicit migration; it must never reinterpret old pointer
bytes.

### Controller and derived frame output

The controller is not an untyped `params` blob. The first form-41 wire should
validate a typed structure containing, at minimum, EX2 version, expected
gametic, consumed DEMO cursor (or an explicit live-input sequence number),
active-player mask/order, fixed-width command bytes, and zero reserved fields.
The descriptor route should name the controller input as a supplied/committed
read, or the form specification must bind the exact input digest. A signature
or instruction echo authorizes a claim; it does not make an uncommitted input
equal to the bytes consumed by the engine.

The frame strip is a derived output, not an input commitment. Form 42 should
write a canonical `DOOM-FB1` header containing the qualified image/ABI,
descriptor or state identity, WAD identity, gametic, strip ID, width/height,
pixel encoding, and byte length, followed by the actual palette-index pixels.
The first test compares these read-back bytes to the retained native EX2/host
golden. It must not use a test-side SHA-256 wrapper as a substitute for a
handler-produced frame.

## WAD and owner provenance

### First profile: private supplied WAD

The first profile should keep the WAD private and supplied. This matches DCG's
supported `LIFETIME_SUPPLIED`/`ACCOUNT_KIND_SUPPLY_WINDOW` path and avoids the
open external-owner bootstrap problem. Use real retained WAD account bytes and
real supply/region-seal instructions. Do not mark a caller-provided digest,
`DSW1`, `DRS1`, or JSON manifest as an authenticated owner.

The host capture uses five 1 MiB `DOOM-WC1` account images with a 32-byte
header and a measured raw WAD of 4,196,020 B. DCG supply windows have their own
96-byte `DSW1` tail. The bridge must explicitly decide whether the region
body is the raw concatenated WAD or the `DOOM-WC1` account image, then bind
that choice in the manifest and form 40. It must not hash one representation,
read another, and call the result the same WAD.

Per-window `initial_content` tags such as
`sha256("dcg/doom/wad-0")` in the structural builder are not the concatenated
IWAD identity. The real WAD SHA-256, directory, lump ranges, map, palette,
and chunk manifest must be authenticated by a qualified init/resource path.
The external-digest region convention exists for this sort of fact, but the
current form 40 is only a declaration and no current DCG handler establishes
it.

### Later profile: shared static WAD

Shared WAD is a separate resource-provenance project, not a shortcut to avoid
rent. DCG's shared-window rules require the owner descriptor's digest and
owner region identity, sealed/frozen owner windows, and the owner's DRS1
readback at its derived address. The owner must be constructed by preceding
real instructions; a consumer test may not seed owner accounts or trust a
caller JSON digest. The current owner-provenance review leaves the external
owner claim **open** and identifies a binder/owner-region-ID seam in
`execute::require_owner_seal` that must be corrected and qualified before a
different consumer/owner region mapping is accepted.

Consequently, the integration sequence is private WAD first. Shared WAD is
allowed only after a non-circular owner/bootstrap contract, raw owner
readbacks, authority/generation policy, and a native/SBF negative matrix for
wrong owner, wrong descriptor, unfrozen, wrong generation, and bad fold.

## Code and file map

This is a map for the owning workers; this dispatch does not edit these files.

| layer | current file/function | required next boundary |
|---|---|---|
| EX1 refusal | `stunts/doom-sbf/src/engine.rs::{Request,validate_accounts,process}`; `stunts/doom-sbf/src/doom_sbf_engine.c::doom_engine_entry_gate` | preserve EX1 and 230 exactly; add EX2 as an explicit version/role/schema path, never by widening EX1 |
| context mapping | `stunts/doom-sbf/src/doom_sbf_ctx.c::{doom_ctx_guard,doom_sbf_bind_arena,arena_take}` | keep account/arena pointers invocation-local; provide EX2 import/rebind/export semantics rather than persisted addresses |
| native engine entry | `stunts/doom-sbf/src/doom_sbf_engine.c` and generated ctx/build path | expose a narrow EX2 engine shim only after pointer-free state/action gates; do not rewrite all native engine functions |
| host state semantics | ordered host patch 0003/0009 `DoomStateEncode`/`DoomStateDecode`, `ds_kind`, `ds_mobj_id`, `ds_state_id` | use as semantic template; freeze LS2/MP2 headers, authority, action table, and mirror rule |
| host controller | `scripts/doom_controller_producer.py`, host V2 `--controller-ingress-v2` | reuse cursor/command semantics only after binding them to EX2's typed account/form |
| WAD continuation | `stunts/doom-sbf/src/bring_up.rs::{process_init,advance,wad_read}` | reuse the actual compression/cursor logic for form 40; replace BU1 config/account framing with authenticated DCG routes |
| Doom descriptor | `src/basanos/dcg/workloads.py::doom_descriptor`; `src/basanos/dcg/scale_workloads.py::doom_frame_descriptor` | split scaffold from accepted EX2 profile; add config/controller/current-next state and real route/provenance declarations |
| DCG instruction decode | `chain/dcg-program/src/lib.rs::{Instruction::decode,process_instruction}` | no new Doom tag is necessary for the first bridge if forms 40–42 run through `Execute`; if a lifecycle tag is needed, specify it separately and keep it consumer-qualified |
| access forms | `chain/dcg-program/src/access.rs::FORMS`, Python mirror `src/basanos/dcg/access.py` | freeze typed form payloads and exact route counts; existing 40–42 rows are not execution support |
| runtime bridge | `chain/dcg-program/src/kernels/mod.rs::{KernelImpl,IMPLEMENTATIONS,resolve,dispatch}` | add Doom rows only in an explicitly qualified image/extension; bind machine name, form ID, profile/ABI, and exact image identity |
| kernel memory | `chain/dcg-program/src/kernels/mod.rs::SpanMemory` | add a checked qualified raw/contiguous span capability or an equivalent engine-local import/export shim; preserve declared-span-only access |
| DCG lifecycle | `desc_upload.rs`, `seal.rs`, `supply.rs`, `region_seal.rs`, `successor.rs` | use real DCD1/BDS2/DSW1/DRS1/DSC1 transitions; do not treat EX2 state bytes as BDS2 lifecycle state |
| integration tests | `chain/dcg-program/tests/` plus consumer-side focused tests | run actual native and exact local-SBF handlers with captured inputs and account readbacks; no seeded trusted DCG images for positive lifecycle evidence |

The active native EX2 worker owns Doom source and EX2 behavior. The active Tier
C worker owns DCG commit/consumer-source work. This note supplies the boundary
only; it does not edit or absorb either worker's source.

## Smallest real DCG acceptance experiment after EX2

This is the first experiment that should be run after the native EX2 gate. It
is **designed**, not yet run.

### Preconditions

* Native EX2 gate passes ST1/LS2 import/export, action rebinding, one real
  controller-driven tic, changed-input causality, malformed action/ID refusal,
  and raw state/frame equality against the retained native golden.
* The qualified DCG image has a non-placeholder machine/ABI identity and a
  registered implementation for the selected Doom form. BDS2 is sealed with
  the exact image hash; no historical deployed alpha image is substituted.
* Inputs are captured from the retained real account-image/WAD run or are
  produced by a preceding real instruction. Any intentionally hand-crafted
  input is labelled `crafted_fixture` and is not evidence.

### Positive sequence

1. Compile one small private EX2/DCG descriptor from the real WAD manifest,
   exact state schema, controller form, one current/next state pair, one
   scratch region, and one strip output. Validate that all routes fit the
   current span/account/cell caps.
2. Run actual DCG tags 19 and 5–11 to create/upload/seal descriptor and state;
   run tags 19 and 12–15 for private WAD windows and DRS1. Do not seed a
   trusted descriptor, BDS2, WAD window, or region seal with
   `ProgramTest::add_account`.
3. Run form 40's real WAD continuation if the accepted profile uses it. Read
   its produced cursor/config bytes from the writable output account and check
   the actual 1,025-step/complete transition against the authenticated WAD
   root. A pass is a continuation mechanics result, not full Doom.
4. Submit one `Execute` form-41 instruction with the typed controller input,
   current LS2/mobj bytes, and declared WAD/config reads. Require the actual
   qualified Doom entry point to import, run one tic, and write next LS2/MP2
   bytes. Read back the writable accounts and compare raw canonical state to
   the native EX2 golden.
5. Submit one separate `Execute` form-42 instruction for strip 0, reading the
   produced next state and private WAD/config, writing one scratch and one
   output account. Require the actual renderer entry point and compare the
   complete output header and pixel bytes to the native golden.
6. Repeat the same instructions with a changed typed controller command. The
   changed state/frame must be produced by the engine and differ at the
   expected boundary. A changed expected hash in a wrapper is not a pass.

### Negative and atomic controls

Run at least these through the same actual instruction entry point:

* wrong BDS2/image hash and wrong machine/ABI identity;
* wrong descriptor chunk, account ordinal, canonical bump, or writable flag;
* malformed controller version, reserved byte, player mask, cursor, or
  expected gametic;
* WAD byte/chunk order mutation and incomplete form-40 cursor;
* unknown action ID, out-of-range object/resource ID, and stale state digest;
* swapped current/next account and read/write alias;
* strip ID outside the descriptor and malformed output buffer.

For every refusal, snapshot all touched account bodies and lamports before the
instruction and require unchanged state. The positive result must identify
the actual `KernelImpl` and actual C engine entry executed; a Python model,
hash wrapper, patch-text assertion, or a successful descriptor parse is not
execution evidence.

### What this experiment does not claim

It does not claim eight-strip frame completion, sustained playback, source
fidelity beyond the selected native golden, cross-machine determinism,
validator behavior, shared-owner provenance, Tier C settlement, or useful
gameplay. It deliberately measures one tic and one strip as separate bounded
parts. A whole frame is accepted only as eight actual derived outputs after
per-strip CU/heap/packet measurements; it is not presumed to fit one
transaction or one kernel call.

## Successor and frame sequencing

The current descriptor has no Doom successor imports. A next-tic path must be
chosen explicitly:

* **Bounded same-descriptor parts:** form 41 writes next-state accounts;
  form 42 reads them; a later tic uses a new current/next pair. This is the
  smallest acceptance shape but consumes additional state accounts and is not
  a long-run lifecycle.
* **Versioned descriptor successor:** form 41's terminal state is sealed and
  exported; a next descriptor imports the exact non-shared state regions with
  `SealSuccessor`. This is the likely long-run path, but the current successor
  mechanism needs a state-region schema and exact LS2/MP2 identity before it
  can claim a Doom continuation.
* **In-place state mode:** only after a runtime extension proves safe mutable
  read/write borrowing and state-mode semantics through actual native/SBF
  tests. Do not infer this from the descriptor's `state_mode=1` field.

The first frame should therefore be a dependency chain:

```text
EX2/init or form40 resource proof
  -> form41 one-tic next-state output
  -> form42 strip 0 output
  -> form42 strip 1..7 outputs (only after each shape is measured)
  -> optional export/successor for the next tic
```

Render strips may be parallel only when each has disjoint writable scratch and
output accounts. They must all carry the same committed gametic and state/WAD
identity. The viewer or test assembles a frame only after all strip headers
agree; no strip output is allowed to become the next state by implication.

## Resource and measurement priorities

The current limits make the first part plausible but do not answer its CU
question:

| priority | question | current status |
|---:|---|---|
| P0 | Can EX2 import/rebind/export pointer-free state and run one real native tic? | **open**; host V2 passes, SBF/EX2 does not |
| P1 | Can a qualified DCG kernel receive the required contiguous state/WAD memory without violating `SpanMemory`'s declared-span rule? | **open**; current API is copying/read-only-slice oriented |
| P2 | Does form 40's real WAD continuation fit its multi-step cursor/state and packet/account shape in the qualified DCG image? | WAD continuation is **measured** standalone; DCG form binding is **open** |
| P3 | What are real form-41 `tic` CU, heap, stack, account bytes, and output bytes on local SBF? | **open**; no SBF Doom tic exists |
| P4 | What are real form-42 strip CU/heap/stack and per-strip packet/account costs on calm and combat inputs? | **open**; synthetic hot-loop figures are not render measurements |
| P5 | Does current/next state fit a bounded successor without aliasing or account-lock conflicts? | **open**; current `Execute` refuses same-entry read/write account alias |
| P6 | Does a full eight-strip frame fit as eight parts under real locks and cadence? | **open**; never divide a whole-frame number by eight |
| P7 | Is shared WAD worth its owner/bootstrap complexity and permanent rent? | **open**; private supplied WAD is the supported first profile |

Relevant source/runtime bounds are **measured/source constants** unless noted:
DCG has a 1,400,000-CU per-entry ceiling in the descriptor grammar, a 10 MiB
maximum account body, 16 kernel spans, 8 placement cells per span, and 32
entry ordinals; the supply frame is 1,024 B and successor folding is bounded
per call. Doom's five-WAD, 131,072-B level, 65,536-B pool, 131,072-B scratch,
and 8,192-B strip sizes are **designed/retained account shapes**, not a proof
that their engine calls fit. The 1,157,357-CU column and 1,410,758-CU span
figures are **measured isolated hot-loop diagnostics**; the 1,399,850-CU
boundary is a **measured diagnostic refusal point**, not a Doom render budget.
The actual form-41/form-42 SBF measurements have priority over all these
projections.

## Reusable work and missing work

### Reusable without a native-engine rewrite

* DCG descriptor grammar, placement, account PDA, DCD1/BDS2 lifecycle, private
  supply/region seal, and successor primitives, subject to a Doom-qualified
  descriptor rather than the current scaffold.
* `DOOM-BU1` SHA-256 compression/cursor logic for a real form-40 continuation.
* Host ST1/MP1 ID/fixup semantics and the V2 cursor/publication ordering as
  input to EX2.
* Host strip geometry and `DOOM-FB1` pixel ordering as a native-output golden.
* The existing C engine arithmetic and source order, behind a narrow EX2
  import/rebind/export shim.

### Missing before integration can be claimed

* EX2 versioned state/action/resource ABI and SBF actual entry path;
* a qualified DCG image policy and exact image/engine ABI binding;
* Doom `KernelImpl` registrations for forms 40–42;
* checked raw/contiguous span or engine-local memory bridge;
* typed controller parser and state/current-next route contract;
* actual WAD manifest/root binding across five windows;
* actual form-41 tic and form-42 render implementation measurements;
* state successor/export semantics and a no-alias lifecycle test;
* shared-owner/bootstrap/readback work if WAD sharing is later selected.

None of these gaps is closed by the current descriptor golden, the host V2
gate, the standalone WAD hash continuation, the DCG 1,660/82 local gate, or a
passing hash wrapper.

## Integration order and ownership boundary

1. Native EX2 worker: freeze pointer-free LS2/MP2, action table, typed CI2/EX2
   input, WAD/resource identity, and one native two-tic/one-render golden.
2. DCG/Tier C owners: decide and document the qualified image/extension
   policy. Keep generic DCG's standalone boundary; do not add a reverse import
   from `src/basanos/dcg` or `chain/dcg-program` to `stunts/doom-sbf`.
3. DCG runtime owner: implement only the minimal qualified bridge needed for
   raw declared spans, form dispatch, image/ABI binding, and atomic refusal
   behavior. Do not combine this with generic Tier C closure work.
4. Integration harness owner: construct a real private-WAD lifecycle from
   preceding DCG instructions, run the one-tic/one-strip experiment above,
   and retain exact account readbacks and local-SBF receipts.
5. Measurement owner: record real CU/heap/stack/packet/account-lock results
   for each part. Decide whether additional strips or a successor are viable.
6. Only then consider shared WAD owner provenance and broader playback.

The parallel native EX2 worker owns Doom source; the Tier C worker owns its DCG
source and commit/consumer path. This note intentionally does not touch either
worker's files. The present change is documentation-only and keeps all source,
evidence, and handoff files unchanged.

## Audit conclusion

The answer to “migrate Doom into DCG now?” is **yes for the integration map and
qualified one-part bridge; no for full runtime migration or capability claims
now**. Preserve EX1 and host V2. Finish EX2 first, then add a clearly named
Doom-qualified DCG implementation that is selected by exact machine/ABI/image
identity, uses private supplied WADs, typed controller input, pointer-free
state, and engine-produced strip output. Measure one real tic and one strip
through the actual DCG instruction path before spending effort on successors,
eight-strip frames, shared WAD ownership, or any generic plugin/CPI design.
