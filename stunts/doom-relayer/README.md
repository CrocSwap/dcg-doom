# Doom relayer

The relayer for [web/doom-play](../../web/doom-play). It accepts signed input
batches from a player, sends the tic, snapshot and strip transactions, pays
every fee, and hands out game slots from a pre-provisioned pool.

The player never signs a transaction and never holds a balance. They sign
input batches with a throwaway key made in their tab.

Design note, including what the slot pool needs from the program:
[`docs/design/doom-relayer-api-2026-09-25.md`](../../docs/design/doom-relayer-api-2026-09-25.md).

## Running it in mock mode

No chain access, no keys, no cost. This is the mode the page is developed
against.

```sh
node stunts/doom-relayer/server.mjs --mode mock --port 8787 --slots 4
```

Then serve the page and point it at the relayer:

```sh
python -m http.server 8000
open "http://localhost:8000/web/doom-play/?relayer=http://127.0.0.1:8787"
```

Flags: `--mode mock|chain`, `--port`, `--slots`, `--network`, `--cors-origin`.

## Chain mode

`--mode chain` is declared and deliberately not wired. It needs a long-lived
sender on the Python side that does not exist yet; `createPipelineSender`
throws with the requirements rather than guessing at an interface another
worker is changing. See `PIPELINE_REQUIREMENTS` in `sender.mjs`.

## Files

| File | What it is |
|---|---|
| `server.mjs` | The HTTP API and the relayer itself. No dependencies. |
| `pool.mjs` | The in-memory slot pool. |
| `sender.mjs` | The chain contract, the mock implementation, and the unwired pipeline binding. |
| `test/test_relayer.mjs` | 25 tests, all in mock mode. |

## Tests

```sh
node --test stunts/doom-relayer/test/test_relayer.mjs
```

Nothing in the suite touches the network or a key file.

## The input contract

Defined once, in `web/doom-play/input.js`, and imported by both sides. The page
signs with WebCrypto; the relayer verifies with `node:crypto`. Both directions
are tested, because a disagreement between them is exactly how a relayer would
start refusing every batch a player sent.
