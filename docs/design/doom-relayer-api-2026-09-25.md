# Doom relayer API

Date: 2026-09-25
Status: design note plus a working skeleton. The skeleton runs in mock mode
only. Nothing in this document has been exercised against a chain.

## What this is for

The player runs Doom on chain. The chain renders frames into eight strip
accounts per frame; the browser paints them and signs the player's keypresses;
the relayer sits in between and is the only party that sends a transaction.

```
browser                        relayer                        chain
  |                                |                             |
  |-- POST /v1/sessions ----------->|                             |
  |<-- slot, binding, strips -------|                             |
  |                                |                             |
  |-- POST .../input (signed) ---->|                             |
  |                                |-- DEPOSIT ----------------->|
  |                                |-- ADVANCE ----------------->|
  |                                |-- SNAPSHOT + 8 RENDER ----->|
  |<-- next_seq, frame signature ---|                             |
  |                                |                             |
  |--------- GET 8 strip accounts ------------------------------->|
  |                                |                             |
```

The player signs inputs. The relayer pays every fee and never asks the player
to sign anything. The player key is a throwaway ed25519 keypair made in the
tab; its security does not matter because it signs inputs only and holds no
funds.

## The four endpoints

### `POST /v1/sessions`

Assigns a slot from the pre-provisioned pool.

```json
{"publicKey": "<64 hex chars>", "network": "testnet", "buffer": 0, "slot": null}
```

`publicKey` is the raw 32-byte ed25519 public key, hex. `slot` asks for a
specific slot; omit it to take any idle one. 200:

```json
{
  "id": "6bbc3757...",
  "slot": 0,
  "state": "playing",
  "next_seq": 0,
  "binding": "<64 hex chars>",
  "context": "...",
  "snapshot_account": "...",
  "strip_accounts": ["... eight ..."],
  "frame": {"cursor": 0, "gametic": 0, "signature": null, "explorer": null},
  "mode": "mock"
}
```

`binding` is the 32-byte value a signed input batch must carry. The relayer
computes it as `sha256("DOOM-SLOT/1\0" || context || meta || input_buffer)` and
hands it to the page so the page and the relayer cannot disagree. It is not a
secret; it exists so a signature made for one slot cannot be replayed onto
another.

409 `pool_empty` when every slot is in use. See the slot pool section for why
that is a real constraint and not an oversight.

### `POST /v1/sessions/{id}/input`

```json
{"message": "<base64>", "signature": "<base64>", "publicKey": "<64 hex>"}
```

`publicKey` may be omitted; the session's key is used. 200:

```json
{
  "ok": true, "accepted": 8, "first_seq": 0, "next_seq": 8,
  "cursor": 8, "gametic": 8, "tics": 8,
  "deposit_signature": "...", "advance_signature": "...",
  "frame": {"cursor": 8, "gametic": 8, "signature": "...", "explorer": "..."}
}
```

The checks, in order, and what each one costs an attacker:

| Check | Failure | Why it is there |
|---|---|---|
| session exists and is `playing` | 404 / 409 | no work for an ended session |
| message parses | 400 `bad_message` | fixed 64-byte header, 1..64 commands |
| **ed25519 signature verifies** | 401 `bad_signature` | inputs are attributable to a player |
| signature is by *this session's* key | 401 `wrong_player` | a valid signature from another player is still not this player's input |
| `client_seq` strictly increases | 409 `stale_client_seq` | in-session replay |
| `slot` matches the session | 409 `batch_refused` | |
| `binding` matches the session | 409 `batch_refused` | cross-slot replay |
| `issued_at` within skew | 409 `batch_refused` | a batch cannot be held and fired much later |
| `first_seq` at or just ahead of `next_seq` | 409 `batch_refused` | a gap in the staged slots would stall the sweep |
| no command sets `BT_SPECIAL` (0x80) | 409 `batch_refused` | the program refuses it anyway; refusing here saves a transaction |
| message length | 400 `message_too_long` | one deposit carries at most 64 commands |

The signature check runs before any rule check, because an unsigned batch is
not a player's input however well formed it is.

The message format itself is defined once, in `web/doom-play/input.js`, and
imported by both the page and the relayer. There is no second copy to drift.

### `GET /v1/sessions/{id}`

Slot, current tic, next expected sequence, and the strip account addresses:

```json
{
  "slot": 0, "next_seq": 128, "cursor": 128, "gametic": 128,
  "context": "...", "snapshot_account": "...", "strip_accounts": ["... eight ..."],
  "frame": {"cursor": 128, "gametic": 128, "signature": "...", "explorer": "..."},
  "tics_landed": 128, "frames_landed": 8
}
```

### `DELETE /v1/sessions/{id}`

Ends the session and returns the slot to the pool as spent.

### Supporting endpoints

- `GET /healthz` — mode, uptime, pool summary, counters.
- `GET /v1/config` — what a client may send: max commands per batch, command
  size, commitment levels, and the whole slot list.
- `GET /v1/slots` — the pool.
- `POST /v1/sessions/{id}/render` — force a frame, for a client that just joined.
- `GET /v1/sessions/{id}/frame` — the eight strip accounts as base64. On chain
  the viewer reads these from the network; this endpoint is what lets a mock
  relayer drive the same viewer with no chain access at all.

## Fees

The relayer pays everything. The player never signs a transaction and never
holds a balance, which is what removes the wallet extension from the loop.

If a future model charges the player, the shape is: the session response
carries a deposit address and an amount, the player sends it out of band, and
the relayer watches for it. That is deliberately not built now. The standing
decision is that the amounts are tiny and the relayer absorbs them.

## The slot pool, and what it needs from the program

This is the part that is not solved.

**The constraint.** Players cannot create their own context. The R8i
measurement is 71.70204 FOGO of rent for a fresh context plus a 1 FOGO
authority reserve, and 7.760594667 FOGO for a second render buffer
(`out/runs/doom-r8i-20260925/summary.json`, `spend`). So a context is
provisioned by the operator and handed out.

**The problem.** The R8i context has 6,016 input slots. At the measured tick
rates (35.0 tics/s at cap 8, 45.5 tics/s unthrottled in s1-base) that is
between 132 and 172 seconds of play. A 30-minute session needs about 63,000
tics. So a provisioned slot today buys roughly one thirtieth of the session
the player wants.

**Why the slot cannot simply be reused.** Three separate reasons, all in the
program as deployed:

1. `process_init` refuses to initialise a meta it already owns
   (`stunts/doom-sbf/src/sim2.rs:1243-1245`, refusal `275`). There is no
   in-place SIM2 reset.
2. `M_HALT` is latched into the meta and, once set, every later sweep returns
   `Stop::Halted` without touching state (`sim2.rs:837-840`). A halted
   context is permanently dead, and a player will eventually pause or
   finish a level.
3. `EXTEND-CAPACITY` is refused by the deployed image: the operation exists in
   the source but is behind the `capacity-extension` cargo feature
   (`sim2.rs:58-67`, `:1595-1596`), so op 7 falls through to `Custom(276)`.
   R8h measured exactly that.

`REBUILD/4` does zero a context, but it requires the BPF loader upgrade
authority *and* the context account itself as a signer
(`stunts/doom-sbf/src/static_resource.rs:384-427`), and it wipes the engine
state, so a reusable slot would still need a re-seed. That is
`reconstruct-context` plus `initialize-fresh`, which is a multi-transaction
dance, not a recycle.

**Three ways out, cheapest first.**

1. **Ship the `capacity-extension` feature.** The code is written and the
   growth arithmetic is already bounded: one instruction grows the buffer by at
   most `MAX_BUFFER_GROWTH` = 10,240 bytes, which is 1,280 slots
   (`sim2.rs:475-481`). The default and maximum buffer capacity is 65,536 slots
   (`sim2.rs:91-99`). From 6,016 slots to 65,536 is about 47 growth
   instructions. At 35 tics/s, 65,536 slots is about 31 minutes. This is the
   smallest change that gets to the 30-minute target, and it is a feature
   enable rather than new logic. It needs an SBF build, which this dispatch
   did not attempt. The rent for a grown buffer is *not measured here* and
   should be measured before relying on it.

2. **Provision more contexts and treat them as single-use.** No program change
   at all. The cost is the measured 71.70204 + 1 + 7.76 ≈ 80.46 FOGO per slot
   against roughly 2.9 minutes of play, which is a poor trade at any volume.
   This is the fallback if option 1 is not available soon.

3. **Add a real recycle instruction.** A new SIM/2 operation that clears the
   staged slots, resets the cursor and gametic, and re-seeds the engine state
   from the captured tic-0 ST1 and MP1 — in one instruction, on chain. This is
   the only option that makes a slot pool genuinely cheap, because it removes
   the per-slot rent. It is a real program change with a real audit surface,
   and it should not be designed in a dispatch that also built a web page.

**Recommendation.** Option 1 now, option 3 as a follow-up. Until then the pool
is single-use and the honest number to put in front of a player is "about three
minutes".

## What the relayer needs from the Python sender

`sender.mjs` defines the contract as `CHAIN_SENDER_CONTRACT` and states the
requirements in `PIPELINE_REQUIREMENTS`. The binding to
`scripts/doom_r8b_tokyo_pipeline.py` is deliberately **not** written: another
worker owns that file and is changing its sender interface, so `createPipelineSender`
throws with the requirements rather than guessing at a CLI.

Four of the five operations already exist somewhere in the tree:

| Operation | Where it is today |
|---|---|
| `deposit` | `scripts/doom_sweep_testnet.py:1450` `deposit_ix` |
| `advance` | `scripts/doom_sweep_testnet.py:1454` `advance_ix` |
| `snapshot` | `scripts/doom_r8b_tokyo_pipeline.py:973` `snapshot_instruction` |
| `renderStrip` | `scripts/doom_r8b_tokyo_pipeline.py:987` `strip_instruction` |
| `longLived` | **missing** |

`longLived` is the blocker. `run` today takes `--seconds` and drives a scripted
command table (`key_command`, line 573), so it is a batch driver, not a server.
A relayer would have to shell out to `run` per batch, which restarts the sender
each time and loses exactly the cursor binding that `ADVANCE_EXPECTED_CURSOR`
exists to provide. The transport should be JSON lines on stdin and stdout, one
request and one response per line, so the process can be held open for the life
of a session.

## Mock mode

`--mode mock` runs the whole API with no chain access. The mock sender walks a
virtual cursor and builds eight real strip accounts with correct digests, so
the viewer's decode path is genuinely exercised rather than stubbed.

Mock mode is not a different API. That is the point: the page can be built and
tested against it without spending anything, and the surface a player sees in
mock mode is the surface they get against a real relayer.

## Security notes

- The relayer's CORS default is any origin. That is safe here because a request
  only does anything with a signature the caller cannot forge, and the
  relayer's own origin is not a secret. Narrow it with `--cors-origin` if the
  relayer is ever exposed more widely.
- The session id is `sha256("doom-relayer/1\0" || slot || player_key)` truncated
  to 32 hex characters. It is not random, so that a page reload with the same
  burner re-attaches to the same session instead of stranding a slot. It is not
  a credential: every request is authenticated by the input signature.
- Session ids are guessable from a burner key, which is public. Nothing
  sensitive is behind one.
- Requests are serialised per relayer, so a deposit and an advance cannot
  interleave on one slot.

## Commitment levels

Frames are painted from the fastest available commitment and reconciled at a
slower one, both named in `web/doom-play/network.js`:

- paint at `processed`
- reconcile at `confirmed`

`processed` is right for paint because a player watching a game wants the
newest frame, not the newest *settled* frame, and a wrong frame that gets
reorged out is replaced 700 ms later by the next poll. `confirmed` is the level
at which the relayer reports a tic as landed. This dispatch did not measure the
reorg behaviour of a frame under load, so the choice is reasoned rather than
measured.

## Status

Built and tested: the API, the pool, the mock sender, the input contract, and
the viewer that consumes them. 25 relayer tests and 44 viewer tests pass, all
offline.

Not done, and not to be assumed:

- The chain sender. No transaction has been sent from this dispatch.
- Any 30-minute session. The largest measured session is the R8i 30-second run.
- Rent for a grown input buffer, and any recycle instruction.
- Reorg behaviour of a painted frame.
