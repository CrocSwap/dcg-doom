# Doom on DCG stateful v3

This crate composes one v3-only DCG stateful kernel with the pinned Doom
engine. The primary, headerless state account is the 9,734,160-byte engine
context, preserving `doom_ctx_guard` at `0x400000060`. ST1 and MP1 are the
following two spans. A sealed RI2/WAD resource initializes the context through
resumable 16,384-byte phases and 115 staged engine phases.

ADVANCE executes one four-byte command per call, the current SVM compute budget
for the pinned engine. Doom pre-tic halts
are returned as v3 prefix outcomes. The nine RSN/3 output views use the sealed
resource and a dedicated writable workspace. Snapshot phases copy the
committed context into that workspace in bounded chunks. Strip phases map the
workspace first so Doom keeps its pinned fixed address; the primary context,
ST1, and MP1 remain read-only, and DCG refuses view instructions that grant
write privileges to those state accounts. Each new snapshot reseeds the
context and clears the strip and render caches so arena marks cannot leak
between frames. The workspace also caches the RSN/3 projection, complete
strips, and render arena high-water mark. Its payload is fixed at 10,462,608
bytes, and the RSN/3 cache is refused if the projection exceeds its 640 KiB
bound.

The SBF image routes every instruction only through
`dcg_program::stateful::v3::process_with_kernel`. It does not route headerless
primary accounts into revision-8 compatibility handlers. Doom uses the
workspace-first `RUN_PHASE` account order so the workspace header stays at the
engine's fixed address; DCG snapshots and checks that 128-byte header around
each callback. Doom itself refuses writable state spans, and its begin-phase
callback overwrites the workspace bytes it consumes rather than asking DCG to
clear the 10 MiB payload. Ordinary v3 views retain the canonical account order
and clear workspace payloads by default.

The accepted DCG change is pinned in `Cargo.toml` to commit
`711f2f18209f68658b5f1600df44fc248a416f29` from the local `dcg-repo/` Git
clone. To recreate it, clone the DCG source at main commit
`057f088c5f31b9fd53750628403eac96f0d6e133` into `dcg-repo/`, apply
`review/dcg-stateful-v3-workspace-render.patch`, then make the accepted fixes:
remove the DCG-wide writable-state view refusal; cap workspace payloads with
`MAX_VIEW_BYTES` consistently in create, grow, and view validation; snapshot
and compare all 128 workspace-header bytes around callbacks; and add the
workspace-first order, refusal, header restore/refusal, and no-clear SBF tests.
The resulting tree is commit `711f2f18209f68658b5f1600df44fc248a416f29`.
Both the `dcg-program` dependency and `curve25519-dalek` patch are pinned to
that Git revision; Cargo resolves the vendored Dalek package as a workspace
member. Regenerate `Cargo.lock` with Cargo after recreating the clone. Do not
apply the rejected `review/dcg-stateful-v3-mutable-render.patch`.

`scripts/pack-dcg-resource.py` packs the retained RI2 image and five
header-stripped WAD chunks. Packed input, SBF output, and ProgramTest receipts
belong under ignored `out/` paths. The r6 experiment record contains the
reproduction command and measured limits.
