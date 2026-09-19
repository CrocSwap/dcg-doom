# Headless Doom host replay

Run `stunts/doom-host/build.sh`; it clones the pinned upstream source into the
scratch area named by `DOOM_SCRATCH` (default: the session scratchpad that
first recorded this path), applies the ordered patch set, fetches the NetBSD
`doom1-1.9.tgz` public mirror, and rejects any WAD except the
4,196,020-byte SHA-256-pinned v1.9 shareware asset.

The binary accepts `-headless -iwad PATH -demo DEMO1 -frames FILE`.  `DEMO1`
is the built-in shareware lump.  `--flip-forwardmove-tic 100` changes bit zero
of the first command byte in the private cached demo buffer; it never writes
the WAD.

To exercise the account-backed loader, first build the five chunk accounts and
manifest from the pinned WAD:

```
PYTHONPATH=. python3 stunts/doom-host/tools/doom_account_image.py build DOOM1.WAD \
  --out-dir out/doom-account
```

Then pass `-account-manifest out/doom-account/manifest.json` instead of
`-iwad`. The loader verifies ordered 1-MiB chunk accounts and reads WAD lumps
from the checked concatenated image without file I/O. `--account-level-state`
and `--account-mobj-pool` write the fixed 131,072-byte and 65,536-byte account
images. `verify` checks the manifest/chunks; `dump-level-state` and
`verify-level-state` extract or compare the embedded `DOOM-ST1` image.

`-state-hashes FILE` adds one `DOOM-ST1` v0 state digest per rendered tic;
`--s0 FILE` records the post-`load_level`, pre-first-tic state.  Use
`--save-at T --save FILE`, then a fresh invocation with `--load FILE` to
continue a replay; `--resave FILE` writes the decoded image without advancing.
`DOOM-ST1` is a fixed-header, little-endian image of mutable E1M1 state. Its
digest is `sha256("DOOM-ST1" || u32le(0) || wad_sha256 || image)`.

`frames.jsonl` has eight `strip_id` records (0–7) and one `strip_id:-1`
whole-frame record per gametic. Strip bytes are column-major: x advances over
the 40-column strip, y advances over each 200-pixel column. `DOOM-FB1` v0 is
`sha256("DOOM-FB1" || u32le(0) || wad_sha256 || u32le(gametic) ||
u32le(strip_id) || 8000 strip bytes)`. Whole records use the distinct
`DOOM-FB1-FRAME` domain, strip id 0, and the native row-major 64,000-byte
framebuffer.

Patch `0006` removes the six `alloca` sites (the SBF backend rejects dynamic
stack allocation) in favour of `Z_Malloc`/`Z_Free`, and adds read-only
instrumentation: `--zone-census FILE` writes one JSON line per phase
(`z_init`, `wad_open`, `engine_init`, `load_level`, `tic_1`, `tic_100`,
`tic_1000`, `final`) reporting non-purgeable, purgeable and extent bytes, and
`-mb-used N` sets the zone size so the floor can be probed. The DEMO1 stream
is unchanged by `0006`.

Patch `0005` adds the host-only reentrant parity harness. Pass
`--reentrant-parity FILE` with `-account-manifest` to compare the ordinary
full framebuffer against eight selected-strip calls from the same state image;
`-probe-refusals FILE` measures the three required fail-closed controls, and
`--max-tics N` bounds a diagnostic interval. Each strip account is 8,192 B:
64 B header followed by 8,000 column-major palette indexes. The 131,072-B
scratch reserves its 128-B header, a 64,000-B full-frame seed, and a 64,000-B
private render framebuffer. The host call forks a copy-on-write child so
vanilla renderer globals and decoded thinker allocations cannot alter the
ordinary replay; this is a host parity/cost tool, not an SBF measurement.

Patch `0010` adds the bounded native controller-causality harness. The legacy
`--controller`/`DOOM-CI1` path remains a captured-command equality check for
the retained tick receipt. The new `--controller-ingress` path accepts only a
producer-emitted `DOOM-CI2` v1 account, arms one command, and supplies its
`ticcmd_t` at the real `G_Ticker` ingress for exactly one loaded tic. The
producer is `tools/doom_controller_producer.py`; it derives the base command
from the retained DEMO1 lump and can emit a controlled signed forward-move
variant. `scripts/doom_controller_causality_gate.py` retains the controller
bytes, state/frame hashes, saved states, executable hash, and ordered patch
hashes while checking same-input repeatability, changed-input effect, and
absent/malformed refusal. This is a host-only gameplay/control capability
demonstration, not an interactive, SBF, validator, chain, or DCG claim.
