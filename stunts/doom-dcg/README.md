# Doom on DCG stateful v2

This crate composes one DCG stateful-v2 kernel with Doom's existing
DOOM-SIM/2 transport handlers under one SVM entrypoint. The session binds the
`basanos-doom-v2` kernel identity, schema `DOOM/1`, four-byte indexed commands,
and the snapshot plus eight strip view ABIs. Build input is the ignored,
read-only `dcg-repo/` checkout at `4d1446f`.

The current adapter is an explicit integration shim, not a working Doom port.
The kernel authenticates the resource account and captures the invocation
state address, then refuses engine access if it is not the fixed
`DOOM_CTX=0x400000060` address required by the pinned 0007 engine. V2 state
spans arrive after the authority, session, and stream accounts, so they do not
have that address. V2 render callbacks also receive read-only state only; they
do not receive the committed RI2/WAD resource or a workspace account. See the
round-5 experiment record for the proposals and the measured limits.

The no-entrypoint feature is enabled only for the composed `doom-dcg` image.
The standalone `doom-sbf` production profile keeps its existing transport
entrypoint when that feature is off.

`scripts/pack-dcg-resource.py` packs the retained RI2 image and the five
header-stripped WAD chunks into the v2 single-resource account envelope. The
input artifacts and packed output belong under ignored `out/`, not Git.
