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

`--mode chain` binds to [`scripts/doom_relayer_server.py`](../../scripts/doom_relayer_server.py),
which holds the measured R8m sender (`doom_r7_render_loop.fire_blind`) and the
R8b frame pipeline open for the life of a session and takes the player's input
as a stream over JSON lines on stdin and stdout. The relayer never sees a key:
the keys stay in the child process, next to it on the host.

The advance and the frame loop belong to the child, not to a request, so
`PipelineSender.background` is true and the relayer's per-batch `advance` and
`renderFrame` report where the chain is instead of driving it. What the relayer
does own is backpressure: it asks the child for its staging frontier and accepts
a batch only as far as the chain will consume within about a second, because
the page offers eight tics per animation frame and the engine eats about 35 a
second. Accepting all of it works and builds a backlog whose key-to-frame delay
grows through the session, which is the R8l5 failure.

```sh
node stunts/doom-relayer/server.mjs --mode chain --port 8787 --host 127.0.0.1 \
  --keys-dir /home/ec2-user/doom-keys/r8l4-fresh \
  --payer-key /home/ec2-user/doom-keys/tokyo-testnet-payer.json \
  --python /home/ec2-user/basanos-main/.venv/bin/python \
  --image-sha256 67038f62... --image-bytes 983384 \
  --seconds 900 --strip-rate 16 --render-buffers 2 \
  --advance-tics 4 --advance-pipeline-depth 4 --sweep-rate 8 \
  --spend-cap-fogo 2
```

`--host` defaults to `127.0.0.1`, so the relayer is not on every interface by
accident. It is the one long-running thing here and its stop is SIGINT or
SIGTERM, which asks the child to write its receipt and exit; a `kill -9` loses
the receipt. To play: [`docs/runbooks/doom-play-local.md`](../../docs/runbooks/doom-play-local.md).

## Files

| File | What it is |
|---|---|
| `server.mjs` | The HTTP API and the relayer itself. No dependencies. |
| `pool.mjs` | The in-memory slot pool. |
| `sender.mjs` | The chain contract, the mock implementation, and the pipeline binding. |
| `test/test_relayer.mjs` | 27 tests: 25 in mock mode, plus the chain binding and its backpressure. |

## Tests

```sh
node --test stunts/doom-relayer/test/test_relayer.mjs
```

Nothing in the suite touches the network or a key file. The chain-mode test
constructs a `PipelineSender` and reads its argv; it never starts the process.

## The input contract

Defined once, in `web/doom-play/input.js`, and imported by both sides. The page
signs with WebCrypto; the relayer verifies with `node:crypto`. Both directions
are tested, because a disagreement between them is exactly how a relayer would
start refusing every batch a player sent.
