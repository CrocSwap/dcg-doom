# Doom on DCG v1: round-3 adapter seam

This crate pins the app-owned `ApplicationManifest` and registers the Doom sim
step, render snapshot, and render-strip kernels as version-1 stateful kernels
that advertise only DCG consensus mode. It depends on the extracted DCG crate
by path at `../../dcg-repo/crates/dcg-program` and reuses `stunts/doom-sbf` by
path. The local `dcg-repo` checkout must be at commit
`5c2de34c12e2c766d3d38a5d8b893d35d1ed5695`.

The registrations are intentionally fail-closed. DCG v1 supplies the pure byte
traits and static manifest, but it has no app-owned SVM dispatcher, no
authenticated multi-account spans in the kernel call, and no Doom input-stream
or engine-state account binding. The manifest therefore declares the intended
ports and state schemas, while every kernel execution method returns
`KernelError::Refused`. The linked SBF image keeps the existing DOOM-SIM/2
transport entrypoint; its SVM handlers are not calls through the DCG kernels.

Build details, ProgramTest evidence, compute comparisons, and the designed gap
list are in `docs/experiments/doom-dcg-r3-adapter-2026-09-30.md`.
