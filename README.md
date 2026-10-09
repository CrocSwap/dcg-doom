# Doom on DCG

Doom running as a [durable-compute-graph](https://github.com/CrocSwap/durable-compute-graph)
(DCG) stateful session on Fogo testnet: the engine advances on chain, three
render lanes draw each frame, and a browser page plays it through a relayer.
It is DCG's flagship consensus-mode application.

Testnet only, research code. See `NOTICE.md` for licensing (GPL-3; the engine
is fetched, never vendored; the WAD is downloaded, never committed).

## Layout

| Path | What it is |
|---|---|
| `stunts/doom-dcg/` | the DCG application program (Rust), its ProgramTests, and the Python testnet drivers and relay pipeline |
| `stunts/doom-sbf/` | the engine linked into the SBF image (C and Rust) |
| `stunts/doom-host/` | fetches and patches the upstream engine and WAD (`build.sh`), host tools |
| `stunts/doom-relayer/` | the Node relayer: HTTP API, public lobby, pipeline binding |
| `web/doom-play/`, `web/doom-public/` | the player page and the public page |
| `scripts/` | play and public-mode launchers, the public-API probe, provenance tools |
| `docs/` | runbooks, designs, and experiment records |

DCG is consumed as a tagged release (`v0.2.0-alpha`): Cargo reads it from
GitHub by tag, and Python installs `dcg-sequencer` from the same tag.

## Build and test

```sh
uv sync                                   # Python: dcg-sequencer, solders, pytest
uv run pytest                             # offline Python tests
(cd stunts/doom-relayer && node --test test/test_relayer.mjs test/test_public_mode.mjs)
(cd web/doom-play && node --test test/test_input.mjs test/test_inputlog.mjs test/test_page_offline.mjs \
   test/test_palette.mjs test/test_push.mjs test/test_strip.mjs)

DOOM_SCRATCH=$PWD/.cache/doom-scratch sh stunts/doom-host/build.sh   # engine + WAD, ignored
DOOM_SCRATCH=$PWD/.cache/doom-scratch DOOM_SBF_SOURCE_TREE=$(git rev-parse HEAD:stunts/doom-sbf) \
  cargo-build-sbf --manifest-path stunts/doom-dcg/Cargo.toml --features sbf \
  --sbf-out-dir out/sbf --tools-version v1.51 --skip-tools-install
```

The SBF build refuses a dirty checkout (its provenance guard). The SBF
ProgramTests (about 12 minutes) need the packed resource
(`stunts/doom-dcg/scripts/pack-dcg-resource.py`); the command is in
`docs/experiments/doom-dcg-r7b-workspace-2026-10-01.md`.

## Play and public mode

- Play from a Mac with the relayer on the Tokyo box:
  `scripts/doom_dcg_play.sh` ([`docs/runbooks/doom-dcg-demo.md`](docs/runbooks/doom-dcg-demo.md)).
- The shared public game: `scripts/doom_dcg_public.sh deploy|start|stop|status`
  (same runbook, "Public mode"; lobby details in
  [`docs/runbooks/doom-public-mode.md`](docs/runbooks/doom-public-mode.md)).

Current testnet deployment: program `2uHBWbYE…`, session `G913rZjZ…`
(`web/doom-play/network.js`, network `testnet-dcg`). Keys are never in this
repository; they live under ignored `out/` paths on the machine that sends.
