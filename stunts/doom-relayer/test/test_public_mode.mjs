import test from "node:test";
import assert from "node:assert/strict";
import {mkdtempSync, readFileSync, rmSync, statSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {webcrypto} from "node:crypto";

import {encodeInput, signingPayload} from "../../../web/doom-play/input.js";
import {createRelayer, createServer} from "../server.mjs";

const subtle = webcrypto.subtle;

async function browser() {
  const pair = await subtle.generateKey({name: "Ed25519"}, true, ["sign", "verify"]);
  return {
    pair,
    publicKey: Buffer.from(await subtle.exportKey("raw", pair.publicKey)).toString("hex"),
  };
}

async function inputPayload(client, session, token, clientSeq = 1) {
  const message = encodeInput({
    slot: session.slot,
    keys: {forward: true},
    clientSeq,
    issuedAt: Math.floor(Date.now() / 1000),
    binding: Uint8Array.from(Buffer.from(session.binding, "hex")),
  });
  const signature = new Uint8Array(await subtle.sign({name: "Ed25519"}, client.pair.privateKey, signingPayload(message)));
  return {
    token,
    publicKey: client.publicKey,
    message: Buffer.from(message).toString("base64"),
    signature: Buffer.from(signature).toString("base64"),
  };
}

async function publicServer({maxViewers = 200, maxStreams = 200, publicStatePath = null, inputLimitPerSecond = 12, idleMs = 60_000, offerMs = 5_000} = {}, work) {
  let now = 1_000_000;
  const relayer = createRelayer({
    mode: "mock",
    network: "testnet",
    publicMode: true,
    publicStatePath,
    publicMaxViewers: maxViewers,
    publicMaxStreams: maxStreams,
    publicSpendCapFogo: 300,
    publicNow: () => now,
    publicRandom: () => 0,
  });
  relayer.publicMode.idleMs = idleMs;
  relayer.publicMode.offerMs = offerMs;
  const sender = relayer.sender;
  const calls = {pause: 0, resume: 0};
  const pause = sender.pause?.bind(sender);
  const resume = sender.resume?.bind(sender);
  sender.pause = async () => { calls.pause += 1; return pause?.(); };
  sender.resume = async () => { calls.resume += 1; return resume?.(); };
  relayer.publicMode.inputLimitPerSecond = inputLimitPerSecond;
  const server = createServer(relayer, {corsOrigin: "https://doom.example"});
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const call = async (method, path, body = undefined, origin = undefined, clientIp = "198.51.100.1") => {
    const response = await fetch(`${base}${path}`, {
      method,
      headers: {
        ...(body === undefined ? {} : {"content-type": "application/json"}),
        ...(origin ? {origin} : {}),
        ...(clientIp ? {"cf-connecting-ip": clientIp} : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return {status: response.status, body: response.status === 204 ? null : await response.json(), response};
  };
  try {
    await work({relayer, call, calls, setNow(value) { now = value; }, getNow() { return now; }, base});
  } finally {
    relayer.publicMode.stop();
    await new Promise((resolve) => server.close(resolve));
  }
}

test("six simulated browsers share one game, rotate five-second offers, and accept only the player", async () => {
  await publicServer({}, async ({relayer, call, setNow, getNow}) => {
    const clients = await Promise.all(Array.from({length: 6}, () => browser()));
    const joined = [];
    for (let index = 0; index < clients.length; index += 1) {
      const client = clients[index];
      const result = await call("POST", "/v1/public/join", {publicKey: client.publicKey}, "https://doom.example", `198.51.100.${index + 1}`);
      assert.equal(result.status, 200);
      joined.push(result.body);
    }
    assert.equal(new Set(joined.map((entry) => entry.session.id)).size, 1, "all browsers attach to one chain session");
    assert.equal(joined[0].state.is_player, true, "first visitor controls");
    assert.equal(joined.slice(1).every((entry) => !entry.state.is_player), true);
    assert.equal(new Set(joined.map((entry) => entry.token)).size, 6, "each browser receives its own token");

    const outsiderInput = await inputPayload(clients[1], joined[1].session, joined[1].token);
    const outsiderResult = await call("POST", "/v1/public/input", outsiderInput);
    assert.equal(outsiderResult.status, 403, "spectator input is refused");

    const playerInput = await inputPayload(clients[0], joined[0].session, joined[0].token);
    const accepted = await call("POST", "/v1/public/input", playerInput);
    assert.equal(accepted.status, 200, "signed player input is accepted");

    const heartbeatAll = () => joined.forEach((entry) => relayer.publicMode.heartbeat(entry.token));
    setNow(getNow() + 60_001);
    heartbeatAll();
    await relayer.publicMode.tick();
    let offered = joined.find((entry) => relayer.publicMode.state(entry.token).offer?.mine);
    assert.ok(offered, "a spectator receives the first offer");
    assert.equal(relayer.publicMode.state(offered.token).offer.seconds, 5);
    assert.equal(relayer.publicMode.state(joined[0].token).is_player, false, "old player cannot send during handoff");

    const rotated = new Set([offered.token]);
    for (let round = 0; round < 4; round += 1) {
      setNow(getNow() + 5_001);
      heartbeatAll();
      await relayer.publicMode.tick();
      offered = joined.find((entry) => relayer.publicMode.state(entry.token).offer?.mine);
      assert.ok(offered, "another spectator receives the next offer");
      rotated.add(offered.token);
    }
    assert.equal(rotated.size, 5, "all five spectators are offered once before a new round");

    await call("DELETE", `/v1/public/leave?token=${encodeURIComponent(offered.token)}`);
    await relayer.publicMode.tick();
    const acceptedClient = joined.find((entry) => entry.token !== offered.token && relayer.publicMode.state(entry.token).offer?.mine);
    assert.ok(acceptedClient, "disconnecting an offered spectator immediately rotates the offer");
    if (!acceptedClient) return;
    const state = await call("POST", "/v1/public/offer/accept", {token: acceptedClient.token});
    assert.equal(state.status, 200);
    assert.equal(state.body.is_player, true);
    assert.equal(relayer.publicMode.state(joined[0].token).is_player, false);
  });
});

test("empty lobby pauses, next accepted input resumes, death text and fanout survive the HTTP stream", async () => {
  await publicServer({}, async ({relayer, call, calls, base}) => {
    const client = await browser();
    const joined = (await call("POST", "/v1/public/join", {publicKey: client.publicKey})).body;
    await call("DELETE", `/v1/public/leave?token=${encodeURIComponent(joined.token)}`);
    await relayer.publicMode.tick();
    assert.equal(relayer.publicMode.paused, true, "empty lobby remains transaction-paused");
    assert.equal(calls.pause, 0, "empty lobby starts paused before any sender loop opens");

    const rejoined = (await call("POST", "/v1/public/join", {publicKey: client.publicKey})).body;
    const controller = new AbortController();
    const stream = await fetch(`${base}/v1/public/stream?token=${encodeURIComponent(rejoined.token)}`, {signal: controller.signal});
    assert.equal(stream.status, 200);
    const reader = stream.body.getReader();
    const decoder = new TextDecoder();
    let text = "";
    const readUntil = async (needle) => {
      while (!text.includes(needle)) {
        const {value, done} = await reader.read();
        if (done) break;
        text += decoder.decode(value, {stream: true});
      }
    };
    try {
      await readUntil('"phase":"paused"');
      const largeStrip = Buffer.alloc(12_288, 0x5a).toString("base64");
      const event = {
        event: "frame", frame_index: 77, cursor: 88, gametic: 88,
        strip_accounts_b64: Array.from({length: 8}, () => largeStrip),
      };
      relayer.onFrame(event);
      await readUntil('"frame_index":77');
      assert.match(text, /event: frame/);
      assert.equal(relayer.publicMode.streams.size, 1, "a normal frame-sized write can drain without dropping the browser stream");
      assert.equal(relayer.subscriptions.size, 1, "the frame subscription remains active while the response drains");
      relayer.publicMode.onPipelineEvent({event: "death"});
      const death = await call("GET", `/v1/public/state?token=${encodeURIComponent(rejoined.token)}`);
      assert.equal(death.body.death_message, "You died. Press R to respawn.");
      const body = await inputPayload(client, rejoined.session, rejoined.token);
      const result = await call("POST", "/v1/public/input", body);
      assert.equal(result.status, 200);
      assert.equal(calls.resume, 1, "first accepted input opens the paused sender");
      await call("DELETE", `/v1/public/leave?token=${encodeURIComponent(rejoined.token)}`);
      assert.equal(calls.pause, 1, "last active browser leaving pauses the sender immediately");
      assert.equal(relayer.publicMode.paused, true);
      relayer.publicMode.onPipelineEvent({event: "finished", spend_fogo: 0.5});
      const stopped = await call("GET", `/v1/public/state?token=${encodeURIComponent(rejoined.token)}`);
      assert.equal(stopped.status, 401, "leaving revokes the old public token");
      const renewed = await call("POST", "/v1/public/join", {publicKey: client.publicKey});
      assert.equal(renewed.body.state.phase, "stopped");
      assert.match(renewed.body.state.pause_message, /session has stopped/);
    } finally {
      controller.abort();
    }
  });
});

test("public spend is grouped by UTC hour and the cap changes the shared banner", async () => {
  await publicServer({}, async ({relayer, setNow}) => {
    const start = Date.UTC(2026, 8, 29, 12, 0, 0);
    setNow(start);
    relayer.publicMode.onPipelineEvent({event: "progress", spend_fogo: 0.2, cursor: 10});
    setNow(start + 10_000);
    relayer.publicMode.onPipelineEvent({event: "progress", spend_fogo: 0.7, cursor: 20});
    const hour = relayer.publicMode.run.hourly_spend["2026-09-29T12"];
    assert.equal(hour.start_fogo, 0.2);
    assert.equal(hour.end_fogo, 0.7);
    assert.equal(hour.spent_fogo, 0.5);
    relayer.publicMode.onPipelineEvent({event: "progress", spend_fogo: 300, cursor: 30});
    assert.equal(relayer.publicMode.state().phase, "cap_reached");
    assert.match(relayer.publicMode.state().pause_message, /spend cap reached/);
  });
});

test("a successful rollover pauses input, clears the cursor, and keeps the same browser in control", async () => {
  await publicServer({}, async ({relayer, call}) => {
    const player = await browser();
    const spectator = await browser();
    const joined = (await call("POST", "/v1/public/join", {publicKey: player.publicKey})).body;
    const watching = (await call("POST", "/v1/public/join", {publicKey: spectator.publicKey})).body;
    const slot = relayer.publicSession.slot;
    slot.cursor = 513;
    slot.gametic = 513;
    slot.nextSeq = 520;
    relayer.publicMode.onPipelineEvent({event: "rollover_start", segment: 1, from_cursor: 512, capacity: 65_536});

    const during = await call("GET", `/v1/public/state?token=${encodeURIComponent(joined.token)}`);
    assert.equal(during.body.phase, "rolling_over");
    assert.equal(during.body.rolling_over, true);
    assert.equal(during.body.is_player, true, "the active browser keeps its turn");
    assert.equal(during.body.pause_message, "New game starting…");
    const blocked = await call("POST", "/v1/public/input", await inputPayload(player, joined.session, joined.token));
    assert.equal(blocked.status, 503);
    assert.equal(blocked.body.code, "rollover_in_progress");

    relayer.publicMode.onPipelineEvent({event: "rollover_complete", segment: 1, cursor: 0});
    assert.equal(slot.cursor, 0);
    assert.equal(slot.gametic, 0);
    assert.equal(slot.nextSeq, 0);
    const after = await call("GET", `/v1/public/state?token=${encodeURIComponent(joined.token)}`);
    assert.equal(after.body.rolling_over, false);
    assert.equal(after.body.is_player, true);
    assert.equal(after.body.phase, "paused", "the same player can reopen the sender with the next input");
    assert.equal(watching.state.is_player, false);
    const resumed = await call("POST", "/v1/public/input", await inputPayload(player, joined.session, joined.token, 2));
    assert.equal(resumed.status, 200);

    const html = await (await import("node:fs/promises")).readFile(new URL("../../../web/doom-public/index.html", import.meta.url), "utf8");
    assert.match(html, /Controls stay visible while you watch\./);
    assert.match(html, /class="controls"/);
  });
});

test("a failed rollover stops the shared session and invokes failure cleanup once", async () => {
  await publicServer({}, async ({relayer, call}) => {
    const player = await browser();
    const joined = (await call("POST", "/v1/public/join", {publicKey: player.publicKey})).body;
    let cleanupCalls = 0;
    relayer.publicMode.onRolloverFailure = async () => { cleanupCalls += 1; };
    relayer.publicMode.onPipelineEvent({event: "rollover_start", segment: 1, from_cursor: 512, capacity: 65_536});
    relayer.publicMode.onPipelineEvent({event: "rollover_failed", segment: 1, error: "RESET readback timed out"});
    relayer.publicMode.onPipelineEvent({event: "failed", error: "public rollover failed"});

    const stopped = await call("GET", `/v1/public/state?token=${encodeURIComponent(joined.token)}`);
    assert.equal(stopped.body.phase, "stopped");
    assert.equal(stopped.body.rolling_over, false);
    assert.match(stopped.body.pause_message, /session has stopped/);
    const refused = await call("POST", "/v1/public/input", await inputPayload(player, joined.session, joined.token));
    assert.equal(refused.status, 503);
    assert.equal(refused.body.code, "pipeline_stopped");
    assert.equal(cleanupCalls, 1);
  });
});

test("review probes: bad stream tokens return 401, public routes cannot end the game, and unauthenticated calls bypass the sender queue", async () => {
  await publicServer({}, async ({relayer, call, base}) => {
    const player = await browser();
    const joined = (await call("POST", "/v1/public/join", {publicKey: player.publicKey})).body;
    let releaseQueue;
    const blocked = relayer.serialise(() => new Promise((resolve) => { releaseQueue = resolve; }));
    await new Promise((resolve) => setImmediate(resolve));
    try {
      const badStream = await call("GET", "/v1/public/stream?token=bogus");
      assert.equal(badStream.status, 401);
      assert.equal(badStream.body.code, "unknown_public_session");
      assert.equal((await call("GET", "/healthz")).status, 200, "a bad stream token does not crash the process");
      for (const path of ["/v1/config", "/healthz"]) {
        const response = await call("GET", path);
        assert.equal(response.status, 200);
        assert.ok(!JSON.stringify(response.body).includes(player.publicKey), `${path} does not expose a player's burner key`);
      }

      const denied = [
        await call("POST", "/v1/sessions", {publicKey: player.publicKey}),
        await call("GET", `/v1/sessions/${encodeURIComponent(joined.session.id)}`),
        await call("POST", `/v1/sessions/${encodeURIComponent(joined.session.id)}/input`, {message: "bad", signature: "bad"}),
        await call("GET", `/v1/sessions/${encodeURIComponent(joined.session.id)}/frame`),
        await call("GET", `/v1/sessions/${encodeURIComponent(joined.session.id)}/frames`),
        await call("POST", `/v1/sessions/${encodeURIComponent(joined.session.id)}/render`),
        await call("DELETE", `/v1/sessions/${encodeURIComponent(joined.session.id)}`),
        await call("GET", "/v1/slots"),
        await call("POST", "/v1/public/heartbeat", {token: "bogus"}),
      ];
      assert.ok(denied.every((result) => result.status === 404 || result.status === 401));
      assert.equal(denied.at(-2).status, 404, "/v1/slots is disabled in public mode");
      assert.equal(denied.at(-1).status, 401, "invalid tokens are rejected before entering the queue");
      const second = await browser();
      const secondJoin = await call("POST", "/v1/public/join", {publicKey: second.publicKey});
      assert.equal(secondJoin.status, 200, "joining does not wait behind the serialized sender queue");
    } finally {
      releaseQueue();
      await blocked;
    }
    assert.equal((await call("GET", `/v1/public/state?token=${encodeURIComponent(joined.token)}`)).body.phase, "paused");
  });
});

test("review probe: one stream per token, a global cap, normal frame backpressure drains, and slow clients are bounded", async () => {
  await publicServer({maxViewers: 3, maxStreams: 1}, async ({relayer, call, base}) => {
    const firstClient = await browser();
    const first = (await call("POST", "/v1/public/join", {publicKey: firstClient.publicKey}, undefined, "203.0.113.10")).body;
    const open = async (token) => fetch(`${base}/v1/public/stream?token=${encodeURIComponent(token)}`);
    const original = await open(first.token);
    assert.equal(original.status, 200);
    const replacement = await open(first.token);
    assert.equal(replacement.status, 200, "a valid new stream replaces the old stream for this token");
    assert.equal(relayer.publicMode.streams.size, 1);
    assert.equal(relayer.subscriptions.size, 1, "the replaced frame subscription is removed");

    const secondClient = await browser();
    const second = (await call("POST", "/v1/public/join", {publicKey: secondClient.publicKey}, undefined, "203.0.113.11")).body;
    const capped = await open(second.token);
    assert.equal(capped.status, 429, "the global stream cap is enforced");
    await capped.body.cancel();

    const largeStrip = Buffer.alloc(12_288, 0x5a).toString("base64");
    const frame = (frame_index) => ({
      event: "frame",
      frame_index,
      strip_accounts_b64: Array.from({length: 8}, () => largeStrip),
    });
    relayer.onFrame(frame(1));
    assert.equal(relayer.publicMode.streams.size, 1, "one frame-sized write queues normally under backpressure");
    assert.equal(relayer.subscriptions.size, 1, "the slow stream retains its frame subscription while the queue drains");
    for (let index = 2; index <= 12 && relayer.publicMode.streams.size > 0; index += 1) relayer.onFrame(frame(index));
    assert.equal(relayer.publicMode.streams.size, 0, "a sustained slow client is dropped when its bounded frame queue fills");
    assert.equal(relayer.subscriptions.size, 0, "dropping a sustained slow stream releases its frame subscription");
    await replacement.body.cancel().catch(() => {});
    await original.body.cancel().catch(() => {});
  });
});

test("review probe: Cloudflare client IPs are capped at four live joins and rejoining the same key replaces its token", async () => {
  await publicServer({}, async ({call}) => {
    const firstClient = await browser();
    const first = await call("POST", "/v1/public/join", {publicKey: firstClient.publicKey}, undefined, "203.0.113.20");
    assert.equal(first.status, 200);
    const rejoined = await call("POST", "/v1/public/join", {publicKey: firstClient.publicKey}, undefined, "203.0.113.20");
    assert.equal(rejoined.status, 200);
    assert.notEqual(first.body.token, rejoined.body.token);
    const staleHeartbeat = await call("POST", "/v1/public/heartbeat", {token: first.body.token}, undefined, "203.0.113.20");
    assert.equal(staleHeartbeat.status, 401, "a token replaced by rejoin is rejected immediately");
    for (let index = 0; index < 3; index += 1) {
      const client = await browser();
      assert.equal((await call("POST", "/v1/public/join", {publicKey: client.publicKey}, undefined, "203.0.113.20")).status, 200);
    }
    const fifth = await browser();
    const capped = await call("POST", "/v1/public/join", {publicKey: fifth.publicKey}, undefined, "203.0.113.20");
    assert.equal(capped.status, 429);
    assert.equal(capped.body.code, "ip_join_cap");
  });
});

test("public run state is persisted at most once per second while retaining the newest frame", async () => {
  const root = mkdtempSync(join(tmpdir(), "doom-public-state-"));
  const statePath = join(root, "public-state.json");
  try {
    await publicServer({publicStatePath: statePath}, async ({relayer}) => {
      const initialWrite = statSync(statePath).mtimeMs;
      relayer.publicMode.onFrame({frame_index: 1});
      relayer.publicMode.onFrame({frame_index: 2});
      assert.equal(statSync(statePath).mtimeMs, initialWrite, "frame updates inside the first second do not rewrite state synchronously");
      await new Promise((resolve) => setTimeout(resolve, 1_050));
      assert.equal(JSON.parse(readFileSync(statePath, "utf8")).last_frame.frame_index, 2);
      assert.ok(statSync(statePath).mtimeMs > initialWrite);
    });
  } finally {
    rmSync(root, {recursive: true, force: true});
  }
});

test("viewer cap, input rate limit, CORS, and shared-page restrictions are enforced", async () => {
  await publicServer({maxViewers: 2, inputLimitPerSecond: 1}, async ({call}) => {
    const first = await browser();
    const second = await browser();
    const third = await browser();
    const a = (await call("POST", "/v1/public/join", {publicKey: first.publicKey}, "https://doom.example")).body;
    const b = (await call("POST", "/v1/public/join", {publicKey: second.publicKey})).body;
    const full = await call("POST", "/v1/public/join", {publicKey: third.publicKey});
    assert.equal(full.status, 429, "viewer cap blocks the third browser");
    const origin = await call("OPTIONS", "/v1/public/join", undefined, "https://doom.example");
    assert.equal(origin.status, 204);
    assert.equal(origin.response.headers.get("access-control-allow-origin"), "https://doom.example");

    const firstInput = await inputPayload(first, a.session, a.token, 1);
    assert.equal((await call("POST", "/v1/public/input", firstInput)).status, 200);
    const secondInput = await inputPayload(first, a.session, a.token, 2);
    const limited = await call("POST", "/v1/public/input", secondInput);
    assert.equal(limited.status, 429, "per-client requests above one per second are refused");
    assert.notEqual(a.token, b.token);
    const privateApi = await call("POST", "/v1/sessions", {publicKey: third.publicKey});
    assert.equal(privateApi.status, 404, "public mode does not expose a second game/session");
  });
});
