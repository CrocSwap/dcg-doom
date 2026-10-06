# Doom on DCG: respawn, and the playable demo on the DCG program (2026-10-06)

**Status: measured.** The owner asked for three things (2026-10-06):
- connect the relayer and the viewer to the DCG-based Doom program;
- fix the respawn rule;
- get the demo working.

## Respawn

**The problem.** The engine's pre-tic guard (`stunts/doom-sbf/src/doom_sim2_guard.h`)
refused a dead player's use press with 295. Vanilla single-player Doom
reloads the whole level on respawn (`G_DoReborn` → `ga_loadlevel`), which
the staged on-chain profile cannot run inside a tic. On the DCG program the
kernel rejected the press (consumed, state unchanged), so a dead player
stayed dead for good: the 10-05 1,000-frame run had 83 such rejections.

**The change.**
- Engine patch `0036-doom-sim2-coop-respawn.patch` sends single player down
  the cooperative branch of `G_DoReborn`: the corpse stays, and the player
  respawns at its start spot with fresh inventory, in the next tic. The
  level and its monsters are unchanged.
- The guard drops its Use-while-dead refusal and its `PST_REBORN` halt.
- 295 stays a reserved code.
- The old R-series program is not rebuilt, and its RESET/9 path is unchanged.

**Checks.**
- **SBF ProgramTest, the lanes test (292 s):** the test leaves player 0 dead
  in the committed context (`players[0]` at byte 89,944 of the
  3,440,952-byte context struct; host and SBF layouts agree), then presses
  use. The press commits as an ordinary tic (225,210 CU), not a rejection,
  and leaves the player reborn. The next tic spawns the player live with
  100 health, and the session keeps advancing. The golden frame at cursor
  1,000 is unchanged.
- **SBF ProgramTest, full lifecycle (1,110 s):** passes. The 1,000-tic golden
  roots, frames and adversarial refusals are unchanged.
- **Image.** `04504ecda9ef68ea…` (943,544 bytes, Basanos `3e226f334`). It
  upgraded program `2uHBWbYE…` in place in 15.8 s (stage cap 90 s), and
  `dcg verify` matches it.

**A finding: an image upgrade invalidates live sessions.** The 10-05
session (`AUHwkbkW…`, provisioned under image `b65854a3…`) faulted on its
next advance after the upgrade: an access violation at `0x50115` about
13,000 compute units in. The engine context keeps pointers into the image's
layout. A new session (`G913rZjZ…`) was provisioned under the new image:
552 steps in 327 s, from the Mac.

## The demo pipeline

**The pipeline.** `stunts/doom-dcg/relay_pipeline.py` speaks the R-series
pipeline's JSON-lines protocol to the Node relayer (`stunts/doom-relayer`):
`hello`, `key_state`, `status`, `resume` and `stop`, plus `started`,
`progress`, `frame` and `finished` events. It drives the DCG session the
way the lanes driver does: 3 lanes, 4 advances per transaction, pipelined
commits and 250-frame priority windows. Inputs come from the player's live
key state; attack and use presses are latched, so a short tap is not lost.

**Frames.** After each commit the pipeline reads the nine views and pushes
the eight `DOOM-RS1` strips (the view minus its 128-byte header). A read can
lag the commit, so the pipeline re-reads until the views reach the frame,
and it never announces an older frame than the last one.

**The page.**
- A `testnet-dcg` network entry.
- The chain cross-check reads strip headers past the view header
  (`view_header_bytes` in the record; a new page test covers it).
- Push, strip and page tests pass.

**The launcher.** `scripts/doom_dcg_play.sh` runs the relayer on the Tokyo
box (`~/dcg-lane/v5`, beside the untouched v4 and R-series installs) behind
an SSH tunnel. `DOOM_DCG_LOCAL=1` runs it on the Mac instead.

## Measured

| Run | Moving after | Frames | Seconds | Frames/s | Testnet FOGO | Failures |
|---|---:|---:|---:|---:|---:|---|
| Mac, pipeline only (forward held) | – | 18 | 38 | ~0.5 | 0.070 | none |
| Mac, full chain (headless Chrome) | 6.17 s | 24 | 66.3 | 0.36 | 0.140 | none |
| Tokyo, pipeline only | – | 109 | 32.8 | 3.6–3.8 steady | 0.402 | none |
| Tokyo cold start 1 (45 s) | 3.83 s | 163 | 45.5 | 3.58 | 0.513 | none |
| Tokyo cold starts 2–6 (30 s each) | 2.99–4.34 s | 101–119 | 30.4–31.6 | 3.31–3.91 | 0.403–0.445 | none |

All six Tokyo cold starts ran through the one command, and none left a
process running on the box.

**Rendering.** A committed frame decoded with the DOOM1.WAD palette shows
the player's pistol facing an E1M1 wall: the expected view after the
opening move.

**Cost.** About 0.7 testnet FOGO per minute of play, mostly priority fees.
The R-series demo cost about 0.02 FOGO per minute. The launcher's default
cap is 10 FOGO.

## Not checked

- **A live death and respawn on testnet.** The respawn is shown in the SBF
  ProgramTest on the same image.
- **Public mode on the DCG pipeline:** rollover, pause, and the death
  banner (the pipeline emits no `death` events).
- **A headed Chrome session played by a person.** All the proofs here used
  headless Chrome and the page's scripted opening move.
- **The old session `AUHwkbkW…`** is unusable under the new image. Its rent
  has not yet been reclaimed.
