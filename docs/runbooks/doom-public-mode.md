# Public Doom mode

This runbook operates the single shared testnet Doom game. It uses the existing
R15 image, and `scripts/doom_play.sh` remains the separate owner-play launcher.
Both launchers contend for `/tmp/doom-play-maintenance.lock` on Tokyo.

## Local build and simulation

Build a Cloudflare Pages upload tree:

```sh
scripts/build_doom_public_page.sh
```

The output is `out/public/doom-public-site/`. It contains the public page under
`doom-public/` and only the shared input and frame-decoder modules it imports
under `doom-play/`.

Run the local simulated-browser coverage without RPC access or FOGO:

```sh
node --test stunts/doom-relayer/test/test_public_mode.mjs
node --test stunts/doom-relayer/test/test_relayer.mjs
```

For a live browser measurement through an existing loopback tunnel, use two
isolated Chrome profiles. The active test duration is capped at 60 seconds; the
script releases input, waits for the 60-second idle pause, and compares the
relayer's reported spend before and after ten paused seconds.

```sh
node scripts/doom_public_browser_probe.mjs \
  'http://127.0.0.1:18000/doom-public/?relayer=http%3A%2F%2F127.0.0.1%3A18787' \
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' 30
```

The first visitor becomes the player. The relayer gives browsers random bearer
tokens, keeps one shared session, accepts signed input from the current player,
and fans its verified frame bytes to every stream. After 60 seconds without
accepted input it pauses the sender and offers control to random spectators for
five seconds each. Browsers heartbeat every ten seconds; an absent browser is
removed after 30 seconds. The viewer cap is 200 and the input limit is 12
accepted requests per second per browser. The page reads frames and PLAYPAL
through the relayer, never directly from chain RPC.

The on-page keyboard map mirrors `web/doom-play/app.js`: arrows/WASD move and
turn, Q/E strafe, Space/F fire, R use/open, and Shift run. Weapon switching is
not mapped by the current demo. The death banner says exactly “You died. Press
R to respawn.”

## Tokyo commands

`scripts/doom_public.sh start` requires `DOOM_PUBLIC_ORIGIN` to be the exact
HTTPS origin that will serve the static page. Defaults are a 300 FOGO cumulative
payer-balance cap, 200 viewers, and a seven-day process duration. Set
`DOOM_PUBLIC_SPEND_CAP` only to a value greater than zero and at most 300. The
cap and hourly cumulative spend are stored in
`out/public-mode/public-state.json` on Tokyo, so a relayer restart carries the
same cap, payer baseline, and frame forward. The cap cannot be raised on a
persisted run; set `DOOM_PUBLIC_STATE_PATH` to a new JSON file under
`out/public-mode/` only when the owner deliberately starts a separate run. The relayer
binds only to `127.0.0.1:8787`; the start command does not open an internet
endpoint or publish the page.

```sh
DOOM_PUBLIC_ORIGIN=https://basanos-doom-public.pages.dev scripts/doom_public.sh start
scripts/doom_public.sh status
scripts/doom_public.sh stop
```

Start refuses to run when the owner launcher, another relayer, or any Tokyo
maintenance lock is present. Stop verifies the public lock and exact run
directory before signaling its process, then requires zero Tokyo Doom relayers
before releasing the lock. If a start fails before health becomes ready, inspect
the Tokyo run's `server.log` and run `scripts/doom_public.sh stop`.

## Publish and TLS ingress (owner action)

These commands are intentionally not run as part of this change. Create the
Pages project once, build and deploy the static tree, then serve the relayer
through a named Cloudflare Tunnel. Replace `<owned-domain>` with the domain
already attached to the owner's Cloudflare account.

```sh
scripts/build_doom_public_page.sh
npx wrangler pages project create basanos-doom-public --production-branch main
npx wrangler pages deploy out/public/doom-public-site --project-name basanos-doom-public --branch main --commit-dirty=true
cloudflared tunnel create doom-public-relayer
cloudflared tunnel route dns doom-public-relayer doom-relay.<owned-domain>
```

Configure the named tunnel with a credentials file from that `cloudflared`
account and this ingress rule, followed by a catch-all 404:

```yaml
ingress:
  - hostname: doom-relay.<owned-domain>
    service: http://127.0.0.1:8787
  - service: http_status:404
```

Then run the service on Tokyo and start public mode with the Pages origin:

```sh
cloudflared tunnel run doom-public-relayer
DOOM_PUBLIC_ORIGIN=https://basanos-doom-public.pages.dev scripts/doom_public.sh start
```

The public page URL is
`https://basanos-doom-public.pages.dev/doom-public/?relayer=https://doom-relay.<owned-domain>`.
The Tokyo service stays loopback-bound; TLS terminates at Cloudflare Tunnel.

## Capacity and evidence limits

Measured R15 evidence reports a 65,536-tic input capacity for its existing
buffer. At the measured 29.377 engine tics per second in the 2026-09-28 send
pipeline run, that implies roughly 37 minutes of continuous active engine time
before the buffer is exhausted. This is an estimate from those two recorded
values, not a measured public-mode uptime. Idle pauses do not advance that
buffer. This implementation preserves the chain game state across a relayer
restart, but does not automatically REINIT or replace an exhausted context; the
seven-day process-duration setting must not be read as seven days of continuous
play. When the sender exits, the page displays that the session stopped. A
longer-lived game needs an owner-approved context rollover plan within the same
spend cap.

The default payer cap is hard-checked against the persisted original payer
balance in the Python sender, which polls every five seconds while active. A
small amount of already-submitted transaction fees can land between balance
polls; this behavior is bounded by in-flight sends but has not been proven as a
mathematically exact on-chain spend ceiling. Keep live tests at or below the
explicit 2 FOGO demo cap and verify the payer balance after stopping.
