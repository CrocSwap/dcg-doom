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
ST1, and MP1 remain read-only under Doom's kernel validator. DCG permits
writable account privileges in view instructions under its address rule, so
Doom rejects that overprivileged attempt in its own callback. Each new
snapshot reseeds the context and clears the strip and render caches so arena
marks cannot leak between frames. The workspace also caches the RSN/3
projection, complete strips, and render arena high-water mark. Its payload is
fixed at 10,462,608 bytes, and the RSN/3 cache is refused if the projection
exceeds its 640 KiB bound.

The SBF image routes every instruction only through
`dcg_program::stateful::v3::process_with_kernel`. It does not route headerless
primary accounts into revision-8 compatibility handlers. Doom uses the
workspace-first `RUN_PHASE` account order so the workspace header stays at the
engine's fixed address; DCG snapshots and checks that 128-byte header around
each callback. Doom itself refuses writable state spans, and its begin-phase
callback overwrites the workspace bytes it consumes rather than asking DCG to
clear the 10 MiB payload. Ordinary v3 views retain the canonical account order
and clear workspace payloads by default.

The accepted DCG change is pinned in `Cargo.toml` to the durable-compute-graph
repository at revision `0ec72af4a108e1457c5c601787e1a775aff78458`. Check out
that revision in `/Users/colkitt/sith/toys/crypto/durable-compute-graph` before
resolving this crate's Git dependencies. The `dcg-program` dependency and its
`curve25519-dalek` patch use the same pinned revision. The DCG workspace keeps
its vendored crate as a path patch, not a workspace member, so the checked-in
lockfile remains valid under `cargo metadata --locked`.

`scripts/pack-dcg-resource.py` packs the retained RI2 image and five
header-stripped WAD chunks. Packed input, SBF output, and ProgramTest receipts
belong under ignored `out/` paths. The r6 experiment record contains the
reproduction command and measured limits.
