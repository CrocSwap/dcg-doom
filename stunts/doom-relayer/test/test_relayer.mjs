// Relayer API tests, all in mock mode.
//
// No chain access, no network. The point is that the API surface a player
// sees in mock mode is the same one they will see against a real relayer, so
// the page can be developed and tested without spending anything.

import test from "node:test";
import assert from "node:assert/strict";
import {webcrypto} from "node:crypto";

import {encodeInput, heldToCommands, MAX_COMMANDS_PER_BATCH, BT_ATTACK, BT_SPECIAL} from "../../../web/doom-play/input.js";
import {createRelayer, createServer, API_VERSION} from "../server.mjs";
import {SlotPool, makeSlot, SLOT_STATE, PoolError} from "../pool.mjs";
import {MockSender, createSender, SenderError, PipelineSender, PIPELINE_REQUIREMENTS} from "../sender.mjs";

const subtle = webcrypto.subtle;

// A player is a burner keypair, created the same way the page creates one.
async function player() {
  const pair = await subtle.generateKey({name: "Ed25519"}, true, ["sign", "verify"]);
  const publicKey = Buffer.from(await subtle.exportKey("raw", pair.publicKey)).toString("hex");
  return {privateKey: pair.privateKey, publicKey};
}

async function batch(wallet, relayer, session, {firstSeq, count = 4, clientSeq, keys = {forward: true}, issuedAt} = {}) {
  const binding = Uint8Array.from(Buffer.from(session.binding, "hex"));
  const message = encodeInput({
    slot: session.slot,
    firstSeq,
    commands: heldToCommands(keys, firstSeq, count),
    clientSeq: clientSeq ?? 1,
    issuedAt: issuedAt ?? Math.floor(Date.now() / 1000),
    binding,
  });
  const signature = new Uint8Array(await subtle.sign({name: "Ed25519"}, wallet.privateKey, await import("../../../web/doom-play/input.js").then((m) => m.signingPayload(message))));
  return {
    message: Buffer.from(message).toString("base64"),
    signature: Buffer.from(signature).toString("base64"),
    publicKey: wallet.publicKey,
  };
}

// Start a relayer on an ephemeral port and return a fetch bound to it.
async function withServer(options, work) {
  const relayer = createRelayer({mode: "mock", network: "testnet", ...options});
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    return await work({relayer, call, base: `http://127.0.0.1:${port}`});
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

// ---------------------------------------------------------------- pool

test("a slot needs a full account set and exactly eight strips", () => {
  const good = {
    slot: 0,
    context: "C",
    meta: "M",
    inputBuffer: "B",
    state: "S",
    mirror: "R",
    authority: "A",
    snapshot: "N",
    strips: Array.from({length: 8}, (_, i) => `strip-${i}`),
  };
  assert.equal(makeSlot(good).state, SLOT_STATE.IDLE);
  assert.throws(() => makeSlot({...good, strips: good.strips.slice(0, 7)}), /exactly 8 strip accounts/);
  for (const field of ["context", "meta", "inputBuffer", "state", "mirror", "authority", "snapshot"]) {
    assert.throws(() => makeSlot({...good, [field]: undefined}), new RegExp(`needs a ${field}`), field);
  }
  assert.throws(() => makeSlot({...good, slot: null}), /needs an index/);
});

test("the pool hands out idle slots and refuses to double-book one", () => {
  const pool = new SlotPool({
    slots: [0, 1].map((slot) => ({
      slot,
      context: `C${slot}`,
      meta: `M${slot}`,
      inputBuffer: `B${slot}`,
      state: `S${slot}`,
      mirror: `R${slot}`,
      authority: `A${slot}`,
      snapshot: `N${slot}`,
      strips: Array.from({length: 8}, (_, i) => `s${slot}-${i}`),
    })),
  });
  assert.equal(pool.size, 2);
  assert.deepEqual(pool.summary().available, 2);

  const first = pool.assign({playerKey: "a"});
  const second = pool.assign({playerKey: "b"});
  assert.notEqual(first.slot.slot, second.slot.slot, "two players get two slots");
  assert.equal(pool.summary().in_use, 2);
  assert.throws(() => pool.assign({playerKey: "c"}), /no free slot/);

  // Asking for a busy slot by index is refused, not silently reassigned.
  assert.throws(() => pool.assign({playerKey: "c", requestedSlot: first.slot.slot}), /is in_use/);

  pool.release(first.sessionId, {spent: true});
  assert.equal(pool.summary().spent, 1);
  assert.equal(pool.summary().available, 0, "a spent slot does not come back");
});

test("the session id is derived from the slot and the player, so it is stable", () => {
  // A page reload with the same burner should re-attach to the same session
  // rather than stranding a slot, which means the id cannot be random.
  const make = () =>
    new SlotPool({
      slots: [{slot: 0, context: "C", meta: "M", inputBuffer: "B", state: "S", mirror: "R", authority: "A", snapshot: "N", strips: Array.from({length: 8}, (_, i) => `s${i}`)}],
    });
  const first = make().assign({playerKey: "burner:aa"});
  const again = make().assign({playerKey: "burner:aa"});
  assert.equal(first.sessionId, again.sessionId, "same slot, same player, same session");
  const other = make().assign({playerKey: "burner:bb"});
  assert.notEqual(first.sessionId, other.sessionId, "a different player gets a different session");
  assert.equal(first.sessionId.length, 32);

  // A released slot is no longer idle, so a second assign cannot hand the
  // same slot to a second player even if the ids would match.
  const pool = make();
  const held = pool.assign({playerKey: "burner:aa"});
  pool.release(held.sessionId, {spent: false, reason: "reset"});
  assert.throws(() => pool.assign({playerKey: "burner:bb"}), /no free slot/);
  assert.equal(pool.bySlot(0).state, SLOT_STATE.BROKEN);
});

test("releasing an unknown session is an error, not a silent no-op", () => {
  const pool = new SlotPool({slots: []});
  assert.throws(() => pool.release("nope"), PoolError);
  assert.throws(() => pool.bySlot(7), /no slot 7/);
});

// ---------------------------------------------------------------- mock sender

test("the mock sender advances a cursor and refuses gaps, overruns and bad commands", async () => {
  const sender = createSender({mode: "mock"});
  assert.equal(sender.mode, "mock");
  await sender.provisionSlot({network: "testnet", label: "s0", slot: 0});

  await assert.rejects(() => sender.deposit({slot: "nope", firstSeq: 0, commands: new Uint8Array(4)}), /no slot nope/);
  // A gap in the staged slots is what the program's sequential sweep needs.
  await assert.rejects(() => sender.deposit({slot: "s0", firstSeq: 8, commands: new Uint8Array(4)}), /deposit starts at 8/);

  await sender.deposit({slot: "s0", firstSeq: 0, commands: new Uint8Array(8)});
  const advanced = await sender.advance({slot: "s0"});
  assert.equal(advanced.cursor, 8);
  assert.equal(advanced.tics, 8);

  // The expected-cursor binding is what makes out-of-order senders safe.
  await assert.rejects(() => sender.advance({slot: "s0", expectedCursor: 0}), /is at cursor 8, not 0/);

  // BT_SPECIAL is refused before it can cost a transaction.
  const bad = new Uint8Array([0, 0, 0, BT_SPECIAL]);
  await assert.rejects(() => sender.deposit({slot: "s0", firstSeq: 8, commands: bad}), /BT_SPECIAL/);

  // Capacity is a hard ceiling: the cursor is already at it.
  assert.equal((await sender.readProgress({slot: "s0"})).capacity, 6016);
  sender.halt("s0", {capacity: null});
});

test("a halted mock slot refuses everything, standing in for a latched M_HALT", async () => {
  const sender = new MockSender();
  await sender.provisionSlot({network: "testnet", label: "s0", slot: 0});
  sender.halt("s0");
  await assert.rejects(() => sender.deposit({slot: "s0", firstSeq: 0, commands: new Uint8Array(4)}), /has halted/);
  await assert.rejects(() => sender.advance({slot: "s0"}), /has halted/);
  assert.equal((await sender.readProgress({slot: "s0"})).halted, true);
});

test("chain mode is wired, and refuses to start without keys or a script", async () => {
  // The chain sender is a binding to scripts/doom_relayer_server.py, which
  // holds the R8m sender and the R8b frame pipeline open for the life of a
  // session. Constructing one does not touch the network or a key file: it is
  // the process that does both, and it is not started until init() asks.
  assert.throws(() => createSender({mode: "chain"}), (error) => {
    assert.ok(error instanceof SenderError);
    assert.equal(error.code, "no_keys");
    assert.match(error.message, /keys stay on the host/);
    return true;
  });
  assert.throws(() => createSender({mode: "chain", keysDir: "/keys", payerKey: "/payer.json", script: "/nope/relay.py"}), (error) => {
    assert.ok(error instanceof SenderError);
    assert.equal(error.code, "no_pipeline_script");
    assert.match(error.message, /not at \/nope\/relay.py/);
    return true;
  });
  assert.throws(() => createSender({mode: "nonsense"}), /unknown sender mode/);

  const sender = createSender({
    mode: "chain",
    keysDir: "/keys",
    payerKey: "/payer.json",
    script: new URL("../../../scripts/doom_relayer_server.py", import.meta.url).pathname,
    stripRate: 16,
    advancePipelineDepth: 4,
    seconds: 600,
    imageSha256: "67038f62",
  });
  assert.equal(sender.mode, "chain");
  // A background sender runs its own advance and render loops, which is what
  // the relayer's per-batch calls must not duplicate.
  assert.equal(sender.background, true);
  assert.equal(sender.child, null);
  // Every chain-mode flag reaches the child, and the flags that change what the
  // sender does on chain are all present rather than defaulted away.
  const argv = sender.argv().join(" ");
  assert.match(argv, /--strip-rate 16/);
  assert.match(argv, /--advance-pipeline-depth 4/);
  assert.match(argv, /--seconds 600/);
  assert.match(argv, /--image-sha256 67038f62/);
  assert.match(argv, /--spend-cap-fogo/);
  assert.match(argv, /--render-buffers 1/);
  // Nothing in this test reaches a chain, and a dead child has to refuse rather
  // than resolve to a zero cursor, which would read as a chain that is not
  // moving when it is a process that is not running.
  await assert.rejects(() => sender.readProgress(), (error) => {
    assert.ok(error instanceof SenderError);
    assert.equal(error.code, "pipeline_exited");
    return true;
  });
  // The requirements are written down so the binding can be reviewed, and the
  // one that used to be missing now points at the file that provides it.
  assert.equal(PIPELINE_REQUIREMENTS.operations.length, 5);
  assert.equal(PIPELINE_REQUIREMENTS.missing.longLived, undefined);
  assert.match(PIPELINE_REQUIREMENTS.existing.longLived, /doom_relayer_server\.py/);
  assert.match(PIPELINE_REQUIREMENTS.transport, /JSON lines/);
});

// ---------------------------------------------------------------- API

test("GET /healthz reports the pool and the mode", async () => {
  await withServer({slotCount: 3}, async ({call}) => {
    const {status, body} = await call("GET", "/healthz");
    assert.equal(status, 200);
    assert.equal(body.ok, true);
    assert.equal(body.api, API_VERSION);
    assert.equal(body.mode, "mock");
    assert.equal(body.network, "testnet");
    assert.equal(body.pool.total, 3);
    assert.equal(body.pool.available, 3);
    assert.equal(body.stats.sessionsCreated, 0);
  });
});

test("GET /v1/config tells the page what it may send", async () => {
  await withServer({slotCount: 2}, async ({call}) => {
    const {status, body} = await call("GET", "/v1/config");
    assert.equal(status, 200);
    assert.equal(body.max_commands_per_batch, MAX_COMMANDS_PER_BATCH);
    assert.equal(body.command_bytes, 4);
    assert.equal(body.slots.length, 2);
    assert.equal(body.slots[0].state, SLOT_STATE.IDLE);
    assert.deepEqual(body.commitment, {paint: "processed", reconcile: "confirmed"});
  });
});

test("POST /v1/sessions hands out a slot and the addresses to read", async () => {
  await withServer({slotCount: 2}, async ({call}) => {
    const wallet = await player();
    const {status, body} = await call("POST", "/v1/sessions", {publicKey: wallet.publicKey});
    assert.equal(status, 200);
    assert.equal(body.state, "playing");
    assert.equal(body.slot, 0);
    assert.equal(body.next_seq, 0);
    assert.equal(body.strip_accounts.length, 8);
    assert.equal(body.snapshot_account, "MOCK-snapshot-0");
    assert.equal(body.binding.length, 64, "a 32-byte slot binding as hex");
    assert.ok(body.id);
  });
});

test("the same burner re-attaches to its session instead of taking a second slot", async () => {
  await withServer({slotCount: 2}, async ({relayer, call}) => {
    const wallet = await player();
    const first = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    assert.equal(first.reattached, false);
    assert.equal(first.slot, 0);
    await call("POST", `/v1/sessions/${first.id}/input`, await batch(wallet, relayer, first, {firstSeq: 0, count: 8, clientSeq: 1}));

    // A page reload presents the same key. It must get the same session and
    // the same slot, not a second slot and a stranded first one.
    const again = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    assert.equal(again.reattached, true);
    assert.equal(again.id, first.id);
    assert.equal(again.slot, 0);
    assert.equal(again.next_seq, 8, "the sequence continues where it left off");
    assert.equal(relayer.pool.summary().in_use, 1, "still one slot in use");
    assert.equal(relayer.stats.sessionsCreated, 1);

    // A different player still gets a different slot.
    const other = await player();
    const theirs = (await call("POST", "/v1/sessions", {publicKey: other.publicKey})).body;
    assert.equal(theirs.slot, 1);
    assert.equal(theirs.reattached, false);
  });
});

test("a session is refused once the pool is empty", async () => {
  await withServer({slotCount: 1}, async ({call}) => {
    const a = await player();
    const b = await player();
    assert.equal((await call("POST", "/v1/sessions", {publicKey: a.publicKey})).status, 200);
    // The same key re-attaches rather than taking the second slot, so this
    // exhausts the pool with two distinct keys and then a third.
    const {status, body} = await call("POST", "/v1/sessions", {publicKey: b.publicKey});
    assert.equal(status, 409);
    assert.equal(body.code, "pool_empty");
    assert.match(body.error, /no free slot/);
  });
});

test("a signed batch is accepted, deposited and advanced", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const payload = await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1});
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, payload);
    assert.equal(status, 200);
    assert.equal(body.ok, true);
    assert.equal(body.accepted, 8);
    assert.equal(body.next_seq, 8);
    assert.equal(body.tics, 8);
    assert.equal(body.cursor, 8);
    assert.equal(body.gametic, 8);
    assert.ok(body.deposit_signature);
    assert.ok(body.advance_signature);
    // The first batch always renders, so a joining player sees a frame.
    assert.equal(body.frame.signature, "mock-frame-1-8");
    assert.ok(body.frame.explorer);
    assert.equal(relayer.stats.inputsAccepted, 8);
  });
});

// A background sender, as the chain binding is, with the frontier under the
// test's control. The point of the frontier is backpressure: a player holding a
// key offers 8 tics per animation frame and the engine eats about 35 a second,
// so a relayer that accepted everything would build a backlog whose latency
// grows through the session. That was R8l5's measured failure.
function backgroundSender({frontier, cursor = 0}) {
  const deposits = [];
  return {
    mode: "chain",
    background: true,
    deposits,
    cursor,
    async provisionSlot() {
      return {
        slot: 0,
        label: "chain-test",
        context: "CTX",
        meta: "MET",
        inputBuffer: "BUF",
        state: "STA",
        mirror: "MIR",
        authority: "AUT",
        snapshot: "SNP",
        strips: Array.from({length: 8}, (_, i) => `STRIP-${i}`),
        inputCapacity: 6016,
        cursor,
        gametic: cursor,
        nextSeq: cursor,
      };
    },
    async stagingFrontier() {
      return frontier;
    },
    async deposit({firstSeq, commands}) {
      deposits.push({firstSeq, tics: commands.length / 4});
      return {accepted: commands.length / 4, signature: null};
    },
    async advance() {
      return {cursor, gametic: cursor, tics: 0, signature: null};
    },
    async renderFrame() {
      return {cursor, gametic: cursor, signature: "chain-frame-1", strips: null};
    },
    async readProgress() {
      return {cursor, gametic: cursor, capacity: 6016, halted: false};
    },
    async close() {},
  };
}

test("chain mode stages a signed batch, and refuses input past the staging frontier", async () => {
  const sender = backgroundSender({frontier: 20});
  const relayer = createRelayer({mode: "chain", network: "testnet", sender});
  await relayer.init();
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    // The slot is the real context the sender is driving, so the player's first
    // batch is staged where the chain is, not at tic zero.
    assert.equal(created.context, "CTX");
    assert.equal(created.next_seq, 0);
    assert.equal(created.strip_accounts.length, 8);
    assert.equal(created.mode, "chain");

    // A whole batch inside the frontier: all of it is staged.
    const first = await call(
      "POST",
      `/v1/sessions/${created.id}/input`,
      await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1}),
    );
    assert.equal(first.status, 200);
    assert.equal(first.body.accepted, 8);
    assert.equal(first.body.offered, 8);
    assert.equal(first.body.next_seq, 8);
    assert.deepEqual(sender.deposits, [{firstSeq: 0, tics: 8}]);
    // A background sender renders on its own schedule, so a batch does not
    // force a frame; the newest one it knows about is reported instead.
    assert.equal(first.body.frame.signature, "chain-frame-1");

    // A batch that runs past the frontier is queued, not trimmed and not
    // refused. The producer reads the command stream by absolute sequence and
    // reaches those tics as its in-flight packets land, and a page that leads
    // the window by its own round trip -- 0.3-0.4 s measured, 7-11 tics at the
    // measured 23.8-26.3 tics/s, against a four-tic window -- puts every batch
    // above the frontier on purpose. Refusing there is the R10 input drop:
    // 19 POSTs, 0 accepted, all 409 `input_stale`, 0 command tics deposited.
    const second = await call(
      "POST",
      `/v1/sessions/${created.id}/input`,
      await batch(wallet, relayer, created, {firstSeq: 8, count: 8, clientSeq: 2}),
    );
    assert.equal(second.status, 200);
    assert.equal(second.body.offered, 8);
    assert.equal(second.body.accepted, 8);
    assert.equal(second.body.next_seq, 16);
    assert.deepEqual(sender.deposits[1], {firstSeq: 8, tics: 8});

    const third = await call(
      "POST",
      `/v1/sessions/${created.id}/input`,
      await batch(wallet, relayer, created, {firstSeq: 16, count: 8, clientSeq: 3}),
    );
    assert.equal(third.status, 200);
    assert.equal(third.body.accepted, 8);
    assert.deepEqual(sender.deposits[2], {firstSeq: 16, tics: 8});

    // What is still refused is a batch so far past the frontier that it is a
    // backlog rather than a keystroke: the R8l5 failure this bound was written
    // for, one lead further out. The frontier is 20 and the lead is 96 tics.
    const fourth = await call(
      "POST",
      `/v1/sessions/${created.id}/input`,
      await batch(wallet, relayer, created, {firstSeq: 116, count: 8, clientSeq: 4}),
    );
    assert.equal(fourth.status, 409);
    assert.equal(fourth.body.code, "input_window_full");
    assert.equal(relayer.stats.inputsRejected, 1);
    // Nothing was staged, so the page's next_seq has not moved and the same
    // batch is still valid to re-offer.
    assert.equal(sender.deposits.length, 3);

    // And just inside the lead it is accepted whole, which is the whole point:
    // a page aiming two round trips ahead is not refused for being early.
    const fifth = await call(
      "POST",
      `/v1/sessions/${created.id}/input`,
      await batch(wallet, relayer, created, {firstSeq: 108, count: 8, clientSeq: 5}),
    );
    assert.equal(fifth.status, 200, JSON.stringify(fifth.body));
    assert.equal(fifth.body.accepted, 8);
    assert.deepEqual(sender.deposits[3], {firstSeq: 108, tics: 8});

    // And the frame-bytes endpoint says why it has none, rather than handing
    // back eight empty strip accounts.
    const bytes = await call("GET", `/v1/sessions/${created.id}/frame`);
    assert.equal(bytes.status, 501);
    assert.equal(bytes.body.code, "no_frame_bytes_in_chain_mode");
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("a batch with a bad signature is refused", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const payload = await batch(wallet, relayer, created, {firstSeq: 0});

    // Flip one bit in the signature.
    const signature = Buffer.from(payload.signature, "base64");
    signature[10] ^= 0x01;
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, {...payload, signature: signature.toString("base64")});
    assert.equal(status, 401);
    assert.equal(body.code, "bad_signature");
    assert.equal(relayer.stats.inputsRejected, 1);
    assert.equal(relayer.stats.inputsAccepted, 0);
  });
});

test("a batch signed by a different burner is refused", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const owner = await player();
    const stranger = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: owner.publicKey})).body;
    // Well-formed and correctly signed, but by a key the session never saw.
    const payload = await batch(stranger, relayer, created, {firstSeq: 0});
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, payload);
    assert.equal(status, 401);
    assert.equal(body.code, "wrong_player");
  });
});

test("a replayed batch is refused, and so is a rewound one", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const first = await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1});
    assert.equal((await call("POST", `/v1/sessions/${created.id}/input`, first)).status, 200);

    // Byte-identical replay: same client_seq, already past next_seq.
    const replay = await call("POST", `/v1/sessions/${created.id}/input`, first);
    assert.equal(replay.status, 409);

    // A fresh signature rewound to an earlier sequence is also refused.
    const rewound = await batch(wallet, relayer, created, {firstSeq: 0, count: 4, clientSeq: 2});
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, rewound);
    assert.equal(status, 409);
    assert.match(body.error, /behind the slot/);
  });
});

test("a batch for another slot, or bound to another context, is refused", async () => {
  await withServer({slotCount: 2}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const other = Uint8Array.from(Buffer.from(created.binding, "hex"));
    other[0] ^= 0xff;

    // Signed properly, but the message claims a different slot.
    const wrongSlot = await batch(wallet, relayer, {...created, slot: created.slot + 1}, {firstSeq: 0});
    const slotResult = await call("POST", `/v1/sessions/${created.id}/input`, wrongSlot);
    assert.equal(slotResult.status, 409);
    assert.match(slotResult.body.error, /session is slot 0/);

    // Signed properly for this slot, but bound to another context.
    const message = encodeInput({
      slot: created.slot,
      firstSeq: 0,
      commands: heldToCommands({}, 0, 4),
      clientSeq: 2,
      issuedAt: Math.floor(Date.now() / 1000),
      binding: other,
    });
    const {signingPayload} = await import("../../../web/doom-play/input.js");
    const signature = new Uint8Array(await subtle.sign({name: "Ed25519"}, wallet.privateKey, signingPayload(message)));
    const bindingResult = await call("POST", `/v1/sessions/${created.id}/input`, {
      message: Buffer.from(message).toString("base64"),
      signature: Buffer.from(signature).toString("base64"),
      publicKey: wallet.publicKey,
    });
    assert.equal(bindingResult.status, 409);
    assert.match(bindingResult.body.error, /binding does not match/);
  });
});

test("a stale timestamp and a BT_SPECIAL command are both refused", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;

    const stale = await batch(wallet, relayer, created, {firstSeq: 0, clientSeq: 1, issuedAt: Math.floor(Date.now() / 1000) - 5000});
    const staleResult = await call("POST", `/v1/sessions/${created.id}/input`, stale);
    assert.equal(staleResult.status, 409);
    assert.match(staleResult.body.error, /issued_at/);

    // BT_SPECIAL never leaves the page, but a hand-rolled client could send
    // it, and the program would refuse it at a cost of a transaction.
    const commands = heldToCommands({attack: true}, 0, 1);
    commands[3] |= BT_SPECIAL;
    const message = encodeInput({
      slot: created.slot,
      firstSeq: 0,
      commands,
      clientSeq: 2,
      issuedAt: Math.floor(Date.now() / 1000),
      binding: Uint8Array.from(Buffer.from(created.binding, "hex")),
    });
    const {signingPayload} = await import("../../../web/doom-play/input.js");
    const signature = new Uint8Array(await subtle.sign({name: "Ed25519"}, wallet.privateKey, signingPayload(message)));
    const special = await call("POST", `/v1/sessions/${created.id}/input`, {
      message: Buffer.from(message).toString("base64"),
      signature: Buffer.from(signature).toString("base64"),
      publicKey: wallet.publicKey,
    });
    assert.equal(special.status, 409);
    assert.match(special.body.error, /BT_SPECIAL/);
    assert.equal(commands[3] & BT_ATTACK, BT_ATTACK, "the test really did set the attack bit too");
  });
});

test("malformed request bodies are refused with a reason", async () => {
  // Two slots, because the anonymous session below takes one.
  await withServer({slotCount: 2}, async ({relayer, call}) => {
    const noBody = await call("POST", "/v1/sessions", {});
    assert.equal(noBody.status, 200, "an anonymous session is allowed");

    // An unknown session is a 404 even with a well-formed body.
    const missing = await call("POST", "/v1/sessions/unknown/input", {
      message: Buffer.alloc(68, 7).toString("base64"),
      signature: Buffer.alloc(64, 3).toString("base64"),
    });
    assert.equal(missing.status, 404);
    assert.equal(missing.body.code, "unknown_session");

    // A body that is not base64 is a 400.
    const badMessage = await call("POST", "/v1/sessions/unknown/input", {message: "not base64!!"});
    assert.equal(badMessage.status, 400);
    assert.match(badMessage.body.error, /must be base64/);

    // An unknown session is checked before the body is looked at, so a bad key
    // on an unknown session is still a 404.
    assert.equal(missing.status, 404);

    // Against a real session, a well-formed and correctly signed batch still
    // fails on a public key that is not 32 bytes of hex.
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const good = await batch(wallet, relayer, created, {firstSeq: 0, clientSeq: 1});
    const badKey = await call("POST", `/v1/sessions/${created.id}/input`, {...good, publicKey: "abc"});
    assert.equal(badKey.status, 400);
    assert.match(badKey.body.error, /32 bytes of hex/);

    // And a message whose magic is wrong is a 400 with the reason, before any
    // signature work is done.
    const badMagic = await call("POST", `/v1/sessions/${created.id}/input`, {
      ...good,
      message: Buffer.concat([Buffer.from("NOPE"), Buffer.alloc(60, 7)]).toString("base64"),
    });
    assert.equal(badMagic.status, 400);
    assert.equal(badMagic.body.code, "bad_message");
    assert.match(badMagic.body.error, /magic/);
  });
});

test("an over-long message is refused before it is parsed", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const oversized = Buffer.alloc(64 + 65 * 4, 7).toString("base64");
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, {
      message: oversized,
      signature: Buffer.alloc(64, 3).toString("base64"),
    });
    assert.equal(status, 400);
    assert.equal(body.code, "message_too_long");
  });
});

test("GET /v1/sessions/:id reports slot, tic and strip accounts", async () => {
  await withServer({slotCount: 2}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    await call("POST", `/v1/sessions/${created.id}/input`, await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1}));

    const {status, body} = await call("GET", `/v1/sessions/${created.id}`);
    assert.equal(status, 200);
    assert.equal(body.slot, 0);
    assert.equal(body.next_seq, 8);
    assert.equal(body.cursor, 8);
    assert.equal(body.gametic, 8);
    assert.equal(body.strip_accounts.length, 8);
    assert.equal(body.state, "playing");
    assert.equal(body.tics_landed, 8);
  });
});

test("POST /v1/sessions/:id/render forces a frame", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    await call("POST", `/v1/sessions/${created.id}/input`, await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1}));
    const before = relayer.stats.framesRendered;
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/render`);
    assert.equal(status, 200);
    assert.equal(body.cursor, 8);
    assert.equal(relayer.stats.framesRendered, before + 1);
  });
});

test("DELETE ends a session and spends the slot", async () => {
  await withServer({slotCount: 2}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    await call("POST", `/v1/sessions/${created.id}/input`, await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1}));

    const {status, body} = await call("DELETE", `/v1/sessions/${created.id}`);
    assert.equal(status, 200);
    assert.equal(body.state, "ended");
    assert.equal(body.slot_state, SLOT_STATE.SPENT);
    assert.equal(body.tics_landed, 8);
    assert.equal(relayer.pool.summary().spent, 1);
    assert.equal(relayer.pool.summary().available, 1, "the other slot is still free");

    // A second player can still join, on the other slot.
    const other = await player();
    assert.equal((await call("POST", "/v1/sessions", {publicKey: other.publicKey})).body.slot, 1);

    // And input to an ended session is refused.
    const late = await call("POST", `/v1/sessions/${created.id}/input`, await batch(wallet, relayer, created, {firstSeq: 8, count: 4, clientSeq: 2}));
    assert.equal(late.status, 409);
    assert.equal(late.body.code, "session_not_playing");
  });
});

test("an unknown route says so, and lists the routes", async () => {
  await withServer({}, async ({call}) => {
    const {status, body} = await call("GET", "/v1/nope");
    assert.equal(status, 404);
    assert.equal(body.code, "no_route");
    assert.match(body.error, /GET \/v1\/nope is not a route/);
    assert.ok(body.error.includes("v1") && body.error.includes("sessions"), "the route list is included");
  });
});

test("GET /v1/slots reports the whole pool", async () => {
  await withServer({slotCount: 2}, async ({call}) => {
    const wallet = await player();
    await call("POST", "/v1/sessions", {publicKey: wallet.publicKey});
    const {status, body} = await call("GET", "/v1/slots");
    assert.equal(status, 200);
    assert.equal(body.slots.length, 2);
    assert.equal(body.slots[0].state, SLOT_STATE.IN_USE);
    assert.equal(body.slots[0].session_id.length, 32);
    assert.equal(body.slots[1].state, SLOT_STATE.IDLE);
    assert.equal(body.pool.in_use, 1);
  });
});

test("concurrent requests on one slot are serialised, not interleaved", async () => {
  await withServer({slotCount: 1}, async ({relayer, call}) => {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    // Two batches that both start at sequence 0. Without serialisation both
    // would pass the next_seq check and one would be deposited twice.
    const a = await batch(wallet, relayer, created, {firstSeq: 0, count: 4, clientSeq: 1});
    const b = await batch(wallet, relayer, created, {firstSeq: 0, count: 4, clientSeq: 2});
    const [first, second] = await Promise.all([
      call("POST", `/v1/sessions/${created.id}/input`, a),
      call("POST", `/v1/sessions/${created.id}/input`, b),
    ]);
    const statuses = [first.status, second.status].sort();
    assert.deepEqual(statuses, [200, 409], "exactly one of two racing batches wins");
    const winner = first.status === 200 ? first : second;
    assert.equal(winner.body.next_seq, 4);
    assert.equal(relayer.stats.inputsAccepted, 4, "the loser was not deposited");
  });
});

// A background sender whose chain moves while nobody is holding a key, which is
// what the R8l6 session did: the engine consumes tics whether or not anyone
// writes them, so a page that counts only what it has sent falls behind the
// cursor and every batch it offers names a slot the game has already read.
function movingSender({advancePerRead = 4, lead = 36, width = 4, jumps = []} = {}) {
  const inner = backgroundSender({frontier: 0});
  const state = {cursor: 1000, reads: 0, deposits: [], width};
  const read = () => {
    state.reads += 1;
    // `jumps` models the chain running on while the page was idle; after that
    // it advances a tic at a time, as a live pipeline does.
    state.cursor += jumps.length ? jumps.shift() : advancePerRead;
    return {
      cursor: state.cursor,
      gametic: state.cursor,
      capacity: 6016,
      halted: false,
      ticsPerSecond: 35,
      stagingFrontier: state.cursor + lead,
    };
  };
  return {
    ...inner,
    get cursor() {
      return state.cursor;
    },
    state,
    async provisionSlot() {
      const slot = await inner.provisionSlot();
      return {...slot, cursor: state.cursor, nextSeq: state.cursor};
    },
    async readProgress() {
      return read();
    },
    async stagingFrontier() {
      read();
      return state.cursor + lead;
    },
    async advance() {
      return {cursor: state.cursor, gametic: state.cursor, tics: 0, signature: null, background: true};
    },
    async deposit({firstSeq, commands}) {
      // A batch is only readable by the engine if the producer had not already
      // deposited it, which it has not done for the last `width` tics.
      const live = state.cursor + lead - width;
      await inner.deposit({firstSeq, commands});
      state.deposits.push({firstSeq, tics: commands.length / 4, readable: firstSeq >= live});
      return {accepted: commands.length / 4, signature: null};
    },
  };
}

test("the status poll tracks a moving chain, with no keystroke involved", async () => {
  const sender = movingSender();
  const relayer = createRelayer({mode: "chain", network: "testnet", sender});
  await relayer.init();
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const first = (await call("GET", `/v1/sessions/${created.id}`)).body;
    const second = (await call("GET", `/v1/sessions/${created.id}`)).body;
    // A cursor that only moved on a keystroke is what made the panel look hung
    // while the engine ran on.
    assert.ok(second.cursor > first.cursor, `cursor moved on a status poll: ${first.cursor} -> ${second.cursor}`);
    assert.equal(second.tics_per_second, 35, "the sender's measured rate is reported, not a hardcoded null");
    assert.equal(second.staging_frontier, second.cursor + 36, "the frontier travels with the cursor");
    // The producer has deposited everything below the last advance packet, so
    // the writable window is one packet wide however wide the frontier is.
    assert.equal(second.input_window, 4, "input_window is the live window, not the staging frontier");
    assert.ok(second.next_seq >= second.cursor, "next_seq never offers a tic the chain has consumed");
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("a batch behind the cursor is refused with a resume point, not deposited", async () => {
  const sender = movingSender({advancePerRead: 4, jumps: [100, 4]});
  // The chain runs on between the stale batch and the aimed one, which is the
  // whole point: the page re-reads the window before it signs.
  const relayer = createRelayer({mode: "chain", network: "testnet", sender});
  await relayer.init();
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    // A page that had been idle: it is offering the sequence it last knew, and
    // the chain has run a hundred tics past it since.
    const stale = await batch(wallet, relayer, created, {firstSeq: created.next_seq ?? 0, count: 4, clientSeq: 1});
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, stale);
    assert.equal(status, 409, "input for a consumed tic is not accepted");
    assert.match(body.error, /behind/);
    assert.equal(sender.deposits.length, 0, "nothing was staged into a dead slot");
    assert.ok(body.detail.cursor > (created.next_seq ?? 0), "the refusal names the cursor to resume from");
    assert.equal(body.detail.next_seq, body.detail.cursor, "next_seq is anchored to the chain");
    assert.equal(body.detail.input_window, undefined);
    // And a batch aimed at the live window, off a window read taken as freshly
    // as the page takes it, is accepted whole and lands where the engine can
    // still read it.
    const window = (await call("GET", `/v1/sessions/${created.id}`)).body;
    const aimed = await batch(wallet, relayer, created, {
      firstSeq: window.staging_frontier - window.input_window,
      count: window.input_window,
      clientSeq: 2,
    });
    const ok = await call("POST", `/v1/sessions/${created.id}/input`, aimed);
    assert.equal(ok.status, 200);
    assert.equal(ok.body.accepted, window.input_window);
    assert.equal(sender.deposits.length, 1);
    // Ahead of the cursor, so not in a slot the engine has already read. Whether
    // the producer had also deposited it is not knowable from out here: the
    // producer's own generated cursor lives inside fire_blind and is not in the
    // status it reports. Until it is, the top of the window is the best aim
    // available and this is the guarantee it buys.
    assert.ok(
      sender.state.deposits[0].firstSeq >= sender.cursor,
      `the batch is ahead of the cursor ${sender.cursor}: ${sender.state.deposits[0].firstSeq}`,
    );
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("every flag start.sh passes reaches the child, including the reset account", () => {
  // `argv()` is the only place a chain-mode flag becomes a process argument. A
  // flag that is not built here is a flag the pipeline has never seen, and
  // `start.sh` had been passing `--reset-start-account` for a dispatch: the
  // whole RESET/9 path was unreachable from the operator's own start command.
  const sender = createSender({
    mode: "chain",
    keysDir: "/keys",
    payerKey: "/payer.json",
    script: new URL("../../../scripts/doom_relayer_server.py", import.meta.url).pathname,
    resetStartAccount: "9XpmEE9iBu5XuJj3S1H32tauME79HGXUR2hT31n9NjM1",
    frameDeadlineSeconds: 2,
    rpcLagDropSlots: 512,
    rpcLagCheckSeconds: 10,
  });
  const argv = sender.argv();
  assert.ok(argv.includes("--reset-start-account"));
  assert.equal(argv[argv.indexOf("--reset-start-account") + 1], "9XpmEE9iBu5XuJj3S1H32tauME79HGXUR2hT31n9NjM1");
  assert.equal(argv[argv.indexOf("--frame-deadline-seconds") + 1], "2");
  assert.equal(argv[argv.indexOf("--rpc-lag-drop-slots") + 1], "512");
  assert.equal(argv[argv.indexOf("--rpc-lag-check-seconds") + 1], "10");
});

test("the session answer names the buffer holding the latest complete frame", async () => {
  // The child reads a frame back before it counts it, so it knows which buffer
  // is whole; the viewer reads eight strip accounts that land in eight
  // transactions and cannot. The pointer has to reach the page together with
  // that buffer's accounts, or the page reads the buffer being written -- which
  // at strip rate 64 is most reads, and was 1 frame painted in 60 s.
  const pointer = {
    buffer: 1,
    cursor: 51140,
    gametic: 51140,
    snapshot_hash: "ab".repeat(32),
    state_root: "cd".repeat(32),
    signature: "chain-frame-7",
    snapshot_account: "snap-1",
    strip_accounts: Array.from({length: 8}, (_, i) => `strip-1-${i}`),
  };
  const inner = backgroundSender({frontier: 20, cursor: 0});
  const sender = {
    ...inner,
    async readProgress() {
      return {...(await inner.readProgress()), latestFrame: pointer};
    },
    async renderFrame() {
      return {...pointer, strips: null, background: true};
    },
  };
  const relayer = createRelayer({mode: "chain", network: "testnet", sender});
  await relayer.init();
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const status = (await call("GET", `/v1/sessions/${created.id}`)).body;
    assert.equal(status.latest_frame.buffer, 1);
    assert.equal(status.latest_frame.snapshot_account, "snap-1");
    assert.deepEqual(status.latest_frame.strip_accounts, pointer.strip_accounts);
    // And it rides on the input answer too, so a held key does not have to wait
    // for the status poll to learn which buffer to read.
    const posted = (await call(
      "POST",
      `/v1/sessions/${created.id}/input`,
      await batch(wallet, relayer, created, {firstSeq: 0, count: 8, clientSeq: 1}),
    )).body;
    assert.equal(posted.latest_frame.buffer, 1);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("a producer-trimmed batch is a partial accept, not a server fault", async () => {
  // The producer's own staging limit is the last word on which tics are still
  // writable, and it can be tighter than the frontier the relayer trimmed
  // against. "One tic less than offered" has to come back as a 200 the page can
  // carry on from; a 500 would leave it retrying the same batch forever, which
  // is the accepted-and-dropped input path with a different status code.
  const deposits = [];
  const producer = {generated_cursor: 0, stage_limit: 0};
  const inner = backgroundSender({frontier: 1020, cursor: 1000});
  const sender = {
    ...inner,
    async readProgress() {
      return {...(await inner.readProgress()), stagingFrontier: 1020, ticsPerSecond: 35};
    },
    async provisionSlot() {
      const slot = await inner.provisionSlot();
      producer.generated_cursor = 1016;
      // Tighter than the four tics the page offers, so the producer has to trim.
      producer.stage_limit = 1018;
      return slot;
    },
    async deposit({firstSeq, commands}) {
      const offered = commands.length / 4;
      const accepted = Math.max(1, Math.min(offered, producer.stage_limit - firstSeq));
      producer.generated_cursor = firstSeq + accepted;
      deposits.push({firstSeq, tics: accepted});
      return {accepted, firstSeq, cursor: 1016, producer_stage_limit: producer.stage_limit};
    },
  };
  const relayer = createRelayer({mode: "chain", network: "testnet", sender});
  await relayer.init();
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const window = (await call("GET", `/v1/sessions/${created.id}`)).body;
    const width = window.input_window || 4;
    const aimed = await batch(wallet, relayer, created, {
      firstSeq: window.staging_frontier - width,
      count: width,
      clientSeq: 1,
    });
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, aimed);
    assert.equal(status, 200, `a short accept is a 200: ${JSON.stringify(body)}`);
    assert.ok(body.accepted >= 1 && body.accepted < width, `short accept: ${body.accepted} of ${width}`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("a pipeline refusal reaches the page as a resume point, not a 500", async () => {
  // A refusal from the pipeline names the sequence to write next. Carrying it
  // as a 409 with its detail is what lets the page re-aim in one frame instead
  // of showing an error and leaving the player's keys unheld.
  const relayer = createRelayer({
    mode: "chain",
    network: "testnet",
    sender: {
      ...backgroundSender({frontier: 1020, cursor: 1000}),
      async readProgress() {
        return {...(await backgroundSender({frontier: 1020, cursor: 1000}).readProgress()),
          stagingFrontier: 1020, ticsPerSecond: 35};
      },
      async deposit() {
        throw new SenderError(
          "first_seq 100 .. 104 is at or below the producer's generated cursor 104: those tics are " +
            "already on chain and cannot be rewritten; resume at 104",
          "pipeline_refused",
          {detail: {next_seq: 104, cursor: 96, staging_frontier: 120, producer_generated_cursor: 104}},
        );
      },
    },
  });
  await relayer.init();
  const server = createServer(relayer);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const {port} = server.address();
  const call = async (method, path, body) => {
    const response = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: body ? {"content-type": "application/json"} : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return {status: response.status, body: await response.json()};
  };
  try {
    const wallet = await player();
    const created = (await call("POST", "/v1/sessions", {publicKey: wallet.publicKey})).body;
    const window = (await call("GET", `/v1/sessions/${created.id}`)).body;
    const width = window.input_window || 4;
    const aimed = await batch(wallet, relayer, created, {
      firstSeq: window.staging_frontier - width,
      count: width,
      clientSeq: 1,
    });
    const {status, body} = await call("POST", `/v1/sessions/${created.id}/input`, aimed);
    assert.equal(status, 409, `a stale sequence is a 409: ${JSON.stringify(body)}`);
    assert.equal(body.code, "input_stale");
    assert.equal(body.detail.next_seq, 104);
    assert.equal(body.detail.producer_generated_cursor, 104);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("the input bound is asked of the child, never served from a cached status", async () => {
  // The bound used to be the cursor plus a constant, so a cached copy was stale
  // by the same amount as the page's own floor and the two cancelled. It is now
  // the tic producer's staging limit -- an absolute sequence -- while the
  // page's floor is a cursor read moments later. Serving the bound from
  // `this.status`, which a 5 s progress event also writes, aims every batch at
  // a sequence the engine has already read: refused, on every push, forever.
  const sender = new PipelineSender({
    mode: "chain",
    keysDir: "/keys",
    payerKey: "/payer.json",
    script: new URL("../../../scripts/doom_relayer_server.py", import.meta.url).pathname,
    runDir: "/run",
  });
  sender.closed = false;
  let asked = 0;
  sender.request = async (payload) => {
    if (payload.op === "status") {
      asked += 1;
      return {ok: true, cursor: 1000 + asked * 10, staging_frontier: 1036 + asked * 10};
    }
    throw new Error(`unexpected op ${payload.op}`);
  };
  // A stale status in hand, exactly as a progress event would leave.
  sender.status = {ok: true, cursor: 1000, staging_frontier: 1036};
  const first = await sender.stagingFrontier();
  assert.equal(first, 1046, "the bound is a fresh answer, not the cached 1036");
  const second = await sender.stagingFrontier();
  assert.equal(second, 1056);
  assert.equal(asked, 2, "one ask per bound, not one per session");
});
