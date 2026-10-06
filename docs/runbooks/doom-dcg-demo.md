# Doom on DCG: the playable testnet demo

Doom running as a DCG stateful session on Fogo testnet (`stunts/doom-dcg`),
played in the browser. This is the DCG-based successor to the R-series demo
in [`doom-demo.md`](doom-demo.md); both share the page and the Node relayer.

## Play

```sh
aws ec2 start-instances --region ap-northeast-1 --instance-ids i-01c33f014d9b485eb   # the Tokyo box, if stopped
scripts/doom_dcg_play.sh            # 300 s by default; scripts/doom_dcg_play.sh 120 for two minutes
```

Chrome opens the game and the picture moves within a few seconds. Controls:
- arrow keys move and turn;
- Ctrl or Space fires;
- **R uses doors, and respawns you after you die**;
- Shift runs.

Ctrl-C stops everything. The session receipt lands in
`out/runs/doom-dcg-demo-2026-10-06/play-<time>/relayer-session.json`. Stop
the Tokyo box afterwards:

```sh
aws ec2 stop-instances --region ap-northeast-1 --instance-ids i-01c33f014d9b485eb
```

Options (environment variables):

| Variable | Default | Meaning |
|---|---|---|
| `SPEND_CAP` | 10 | testnet FOGO cap for the session (about 0.7 a minute is spent) |
| `DOOM_HEADLESS=1` | off | headless Chrome |
| `DOOM_DCG_LOCAL=1` | off | run the relayer on this Mac instead of Tokyo (slow motion: about 0.5 frames/s) |
| `DOOM_HOST` | `basanos-tokyo-2` | the relayer host |

## How it works

1. The page (`web/doom-play`, network `testnet-dcg`) sends the player's key
   state to the Node relayer (`stunts/doom-relayer/server.mjs`) and paints
   pushed frames.
2. The relayer runs `stunts/doom-dcg/relay_pipeline.py`, which drives the DCG
   session on program `2uHBWbYE…` with three render lanes, as the lanes driver
   does. For each frame it does three things:
   - writes 8 tics of input from the current key state, advances the session
     8 tics, and captures the state to a lane;
   - renders the eight strips on that lane;
   - commits them to the session's views.
3. After each commit the pipeline reads the nine views back and pushes the
   eight `DOOM-RS1` strips to the page. Each view is a 128-byte DCG view
   header followed by the strip account. Every 20th frame, the page also
   reads the strip headers from chain and compares their digests.
4. On the Tokyo box (`~/dcg-lane/v5`) the keys never leave the box. The Mac
   reaches the relayer through an SSH tunnel.

**Respawn.** A dead player's use press respawns the player at the level
start, with fresh inventory; the level and its monsters stay as they are.
This is engine patch `stunts/doom-host/patches/0036-doom-sim2-coop-respawn.patch`
(the cooperative-play path). Before it, the press was refused with 295, and a
dead player could never move again.

## Current deployment (2026-10-06)

- **Program:** `2uHBWbYEzGnjtik4fibo3eqipmgAY7pKUSL8Yp98j9vJ`, image
  `04504ecd…` (943,544 bytes; `out/runs/doom-respawn-2026-10-06/receipt.json`);
  `dcg verify` matches.
- **Session:** `G913rZjZ4QgT3Je79DfXQipFDrFNs6bR6g9h3XABUxoL`, provisioned
  2026-10-06 under that image (552 steps, 327 s).
- **Driver config and keys:**
  - this Mac: `out/runs/doom-dcg-demo-2026-10-06/driver-config.json`, with
    keys in `out/chain/doom-current-dcg-testnet-2026-10-05/` (0600);
  - Tokyo: `~/dcg-lane/v5/run`, with keys in `~/dcg-lane/v4/run/keys`.

**An image upgrade needs a new session.** The engine context stores
pointers into the program image's layout. After an upgrade, a session
provisioned under the old image faults on its next advance (measured: an
access violation about 13,000 compute units in). Provision a new session
(`lanes_testnet.py provision --new-session`), then update the `testnet-dcg`
entry in `web/doom-play/network.js` and the driver config on Tokyo.

## Measured (2026-10-06)

Six consecutive cold starts through `scripts/doom_dcg_play.sh`, all from
Tokyo:
- the picture was moving 2.99 to 4.34 s after launch;
- each run sustained 3.31 to 3.91 frames/s;
- no failures, and nothing was left running on the box afterwards.

The same pipeline run from the Mac reaches about 0.4 to 0.6 frames/s. Details
are in `docs/experiments/doom-dcg-demo-2026-10-06.md`.

## Not done yet

- **Public mode** (spectators, idle handoff) still runs the R-series
  relayer. The DCG pipeline answers `resume` and `stop`, but public mode's
  rollover and pause path have not been tried with it.
- **A live death and respawn on testnet** has not been staged. The respawn is
  shown in the SBF ProgramTest, on the same image.
- **The page's death banner** is not shown: the DCG pipeline does not emit
  `death` events yet.
