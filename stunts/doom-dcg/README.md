# Doom on DCG stateful v3

This crate composes one v3-only DCG stateful kernel with the pinned Doom
engine. The primary, headerless state account is the 9,734,160-byte engine
context, preserving `doom_ctx_guard` at `0x400000060`. ST1 and MP1 are the
following two spans. A sealed RI2/WAD resource initializes the context through
resumable 16,384-byte phases and 115 staged engine phases.

ADVANCE executes up to eight four-byte commands per call. Doom pre-tic halts
are returned as v3 prefix outcomes. The nine RSN/3 output views use the sealed
resource and a cleared workspace; complete strips are cached there while the
view phase copies bounded chunks to the staged outputs.

The SBF image routes every instruction only through
`dcg_program::stateful::v3::process_with_kernel`. It does not route headerless
primary accounts into revision-8 compatibility handlers. The DCG-side mutable
view callback is opt-in and leaves existing read-only view behavior as the
default.

`scripts/pack-dcg-resource.py` packs the retained RI2 image and five
header-stripped WAD chunks. Packed input, SBF output, and ProgramTest receipts
belong under ignored `out/` paths. The r6 experiment record contains the
reproduction command and measured limits.
