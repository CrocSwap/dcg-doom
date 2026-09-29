import test from "node:test";
import assert from "node:assert/strict";
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

async function publicServer({maxViewers = 200, inputLimitPerSecond = 12, idleMs = 60_000, offerMs = 5_000} = {}, work) {
  let now = 1_000_000;
  const relayer = createRelayer({
    mode: "mock",
    network: "testnet",
    publicMode: true,
    publicMaxViewers: maxViewers,
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
  const call = async (method, path, body = undefined, origin = undefined) => {
    const response = await fetch(`${base}${path}`, {
      method,
      headers: {
        ...(body === undefined ? {} : {"content-type": "application/json"}),
        ...(origin ? {origin} : {}),
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
    for (const client of clients) {
      const result = await call("POST", "/v1/public/join", {publicKey: client.publicKey}, "https://doom.example");
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
      const event = {
        event: "frame", frame_index: 77, cursor: 88, gametic: 88,
        strip_accounts_b64: Array.from({length: 8}, (_, index) => Buffer.from(`strip-${index}`).toString("base64")),
      };
      relayer.onFrame(event);
      await readUntil('"frame_index":77');
      assert.match(text, /event: frame/);
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
      assert.equal(stopped.body.phase, "stopped");
      assert.match(stopped.body.pause_message, /session has stopped/);
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
