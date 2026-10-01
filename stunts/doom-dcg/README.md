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
ST1, and MP1 remain read-only. The workspace also caches the RSN/3 projection,
complete strips, and render arena high-water mark. Its payload is fixed at
10,462,608 bytes, and the RSN/3 cache is refused if the projection exceeds its
640 KiB bound.

The SBF image routes every instruction only through
`dcg_program::stateful::v3::process_with_kernel`. It does not route headerless
primary accounts into revision-8 compatibility handlers. The DCG mutable-state
render callback is not used. The additive workspace-first RUN_PHASE interface
and large-workspace limit are recorded in
`review/dcg-stateful-v3-workspace-render.patch` against DCG main; ordinary v3
views remain read-only by default.

`scripts/pack-dcg-resource.py` packs the retained RI2 image and five
header-stripped WAD chunks. Packed input, SBF output, and ProgramTest receipts
belong under ignored `out/` paths. The r6 experiment record contains the
reproduction command and measured limits.
