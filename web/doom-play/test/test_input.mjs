// Offline tests for the signed input format and the burner wallet.
//
// The page signs with WebCrypto Ed25519; the relayer verifies with node:crypto
// against the same bytes. Both directions are covered here, because a
// disagreement between them is exactly how a relayer would start refusing
// every batch a player sent.

import test from "node:test";
import assert from "node:assert/strict";
import {createPrivateKey, createPublicKey, sign as nodeSign, verify as nodeVerify} from "node:crypto";
import {webcrypto} from "node:crypto";

import {
  INPUT_MAGIC,
  INPUT_VERSION,
  INPUT_VERSION_EXPLICIT_TICS,
  INPUT_SIGN_DOMAIN,
  INPUT_HEADER_BYTES,
  COMMAND_BYTES,
  MAX_COMMANDS_PER_BATCH,
  MAX_INPUT_BYTES,
  BT_SPECIAL,
  BT_ATTACK,
  BT_USE,
  BT_SPEED,
  OFF_SLOT,
  OFF_FIRST_SEQ,
  OFF_CLIENT_SEQ,
  OFF_ISSUED_AT,
  OFF_SLOT_BINDING,
  OFF_PAYLOAD_BYTES,
  OFF_COMMANDS,
  InputError,
  encodeInput,
  encodeKeyState,
  decodeInput,
  signingPayload,
  signInput,
  verifyInput,
  validateInput,
  slotBinding,
  ticcmd,
  heldToCommands,
  TICCMD_HORIZONTAL,
} from "../input.js";

const subtle = webcrypto.subtle;

const BINDING = new Uint8Array(32).fill(7);
const NOW = 1_800_000_000;

async function burner() {
  const pair = await subtle.generateKey({name: "Ed25519"}, true, ["sign", "verify"]);
  const publicKeyRaw = new Uint8Array(await subtle.exportKey("raw", pair.publicKey));
  const pkcs8 = new Uint8Array(await subtle.exportKey("pkcs8", pair.privateKey));
  return {privateKey: pair.privateKey, publicKey: pair.publicKey, publicKeyRaw, pkcs8};
}

function commands(count, fill = 0) {
  const bytes = new Uint8Array(count * COMMAND_BYTES);
  for (let i = 0; i < count; i += 1) {
    bytes[i * COMMAND_BYTES] = fill;
    bytes[i * COMMAND_BYTES + 1] = 0;
    bytes[i * COMMAND_BYTES + 2] = 4;
    bytes[i * COMMAND_BYTES + 3] = 0;
  }
  return bytes;
}

function sample(overrides = {}) {
  return encodeInput({
    slot: 3,
    firstSeq: 128,
    commands: commands(4),
    clientSeq: 9,
    issuedAt: NOW,
    binding: BINDING,
    ...overrides,
  });
}

function sampleKeyState(overrides = {}) {
  return encodeInput({
    slot: 3,
    keys: {forward: true, attack: true},
    clientSeq: 9,
    issuedAt: NOW,
    binding: BINDING,
    ...overrides,
  });
}

const hex = (bytes) => Buffer.from(bytes).toString("hex");

// The production pairing is: the page signs with WebCrypto, the relayer
// verifies with node:crypto. That is what these tests exercise, because
// WebCrypto verify is the browser-side mirror and is not exercised offline.
// (The Chrome available in this environment returns false from
// crypto.subtle.verify for every algorithm, including P-256, so asserting on
// it here would only pin a broken environment quirk.)
function nodeKeyFromRaw(publicKeyRaw) {
  return createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), Buffer.from(publicKeyRaw)]),
    format: "der",
    type: "spki",
  });
}

function verifies(wallet, message, signature) {
  return nodeVerify(null, Buffer.from(signingPayload(message)), nodeKeyFromRaw(wallet.publicKeyRaw), Buffer.from(signature));
}

// ---------------------------------------------------------------- layout

test("the header is 64 bytes and the message is header plus four per command", () => {
  assert.equal(INPUT_HEADER_BYTES, 64);
  assert.equal(COMMAND_BYTES, 4);
  assert.equal(MAX_COMMANDS_PER_BATCH, 64);
  assert.equal(MAX_INPUT_BYTES, 64 + 64 * 4);
  assert.equal(sample().length, INPUT_HEADER_BYTES + 4 * COMMAND_BYTES);
  assert.equal(sample({commands: commands(64)}).length, MAX_INPUT_BYTES);
});

test("encode then decode is a round trip", () => {
  const original = sample();
  const parsed = decodeInput(original);
  assert.equal(parsed.slot, 3);
  assert.equal(parsed.firstSeq, 128);
  assert.equal(parsed.clientSeq, 9);
  assert.equal(parsed.issuedAt, NOW);
  assert.equal(parsed.count, 4);
  assert.deepEqual(Array.from(parsed.binding), Array.from(BINDING));
  assert.deepEqual(Array.from(parsed.commands), Array.from(commands(4)));
});

test("every field lands at its documented offset, little-endian", () => {
  const message = sample();
  const view = new DataView(message.buffer);
  assert.equal(new TextDecoder().decode(message.subarray(0, 8)), INPUT_MAGIC);
  assert.equal(view.getUint16(8, true), INPUT_VERSION_EXPLICIT_TICS);
  assert.equal(view.getUint16(10, true), 4);
  assert.equal(view.getUint32(OFF_SLOT, true), 3);
  assert.equal(view.getUint32(OFF_FIRST_SEQ, true), 128);
  assert.equal(view.getUint32(OFF_CLIENT_SEQ, true), 9);
  assert.equal(view.getUint32(OFF_ISSUED_AT, true), NOW);
  assert.equal(hex(message.subarray(OFF_SLOT_BINDING, OFF_SLOT_BINDING + 32)), hex(BINDING));
  assert.equal(view.getUint32(OFF_PAYLOAD_BYTES, true), 16);
  assert.equal(message.length, OFF_COMMANDS + 16);
});

test("encode refuses a batch outside 1..64 commands", () => {
  assert.throws(() => sample({commands: commands(0)}), /1\.\.64 commands, got 0/);
  assert.throws(() => sample({commands: commands(65)}), /1\.\.64 commands, got 65/);
  assert.throws(() => sample({commands: new Uint8Array(5)}), /not a multiple of 4/);
  assert.throws(() => sample({binding: new Uint8Array(31)}), /must be 32 bytes/);
});

test("decode refuses a malformed message", () => {
  assert.throws(() => decodeInput(new Uint8Array(10)), /shorter than the 64-byte header/);

  const badMagic = sample();
  badMagic[0] = 0x58;
  assert.throws(() => decodeInput(badMagic), /magic/);

  const badVersion = sample();
  new DataView(badVersion.buffer).setUint16(8, 7, true);
  assert.throws(() => decodeInput(badVersion), /version 7/);

  // A count that disagrees with the declared payload length.
  const badCount = sample();
  new DataView(badCount.buffer).setUint16(10, 5, true);
  assert.throws(() => decodeInput(badCount), /disagrees with count/);

  // A truncated payload.
  const truncated = sample().subarray(0, INPUT_HEADER_BYTES + 8);
  assert.throws(() => decodeInput(truncated), /header says/);

  // Trailing junk.
  const trailing = new Uint8Array(sample().length + 1);
  trailing.set(sample());
  assert.throws(() => decodeInput(trailing), /header says/);

  // A well-formed header that claims zero commands. encodeInput refuses to
  // build one, so this has to be assembled by hand to reach the decode check.
  const zeroCount = sample().subarray(0, INPUT_HEADER_BYTES);
  new DataView(zeroCount.buffer).setUint16(10, 0, true);
  new DataView(zeroCount.buffer).setUint32(OFF_PAYLOAD_BYTES, 0, true);
  assert.throws(() => decodeInput(zeroCount), /outside 1\.\.64/);

  // And one that claims more than the program will take in a deposit.
  const tooMany = new Uint8Array(INPUT_HEADER_BYTES + 65 * COMMAND_BYTES);
  tooMany.set(new TextEncoder().encode(INPUT_MAGIC), 0);
  const tooManyView = new DataView(tooMany.buffer);
  tooManyView.setUint16(8, INPUT_VERSION_EXPLICIT_TICS, true);
  tooManyView.setUint16(10, 65, true);
  tooManyView.setUint32(OFF_PAYLOAD_BYTES, 65 * COMMAND_BYTES, true);
  tooManyView.setUint32(OFF_ISSUED_AT, NOW, true);
  assert.throws(() => decodeInput(tooMany), /outside 1\.\.64/);
});

// ---------------------------------------------------------------- signing

test("a key made with WebCrypto signs, and node verifies the signature", async () => {
  const wallet = await burner();
  const message = sample();
  const signature = await signInput(wallet.privateKey, message);
  assert.equal(signature.length, 64, "ed25519 signatures are 64 bytes");
  assert.equal(verifies(wallet, message, signature), true, "the relayer accepts it");

  // The pkcs8 blob the page stores in localStorage must import in node, or a
  // burner key made in a tab could never be verified by the relayer.
  const priv = createPrivateKey({key: Buffer.from(wallet.pkcs8), format: "der", type: "pkcs8"});
  assert.equal(priv.asymmetricKeyType, "ed25519");
  assert.equal(createPublicKey(priv).export({format: "der", type: "spki"}).subarray(-32).equals(Buffer.from(wallet.publicKeyRaw)), true);
});

test("the signing payload is the domain tag followed by the message", () => {
  const message = sampleKeyState();
  const payload = signingPayload(message);
  assert.equal(payload.length, INPUT_SIGN_DOMAIN.length + message.length);
  assert.equal(new TextDecoder().decode(payload.subarray(0, INPUT_SIGN_DOMAIN.length)), INPUT_SIGN_DOMAIN);
  assert.deepEqual(Array.from(payload.subarray(INPUT_SIGN_DOMAIN.length)), Array.from(message));
});

test("version 2 signs a sequence-free key state and bypasses page-side cursor windows", () => {
  const message = sampleKeyState();
  const parsed = decodeInput(message);
  assert.equal(new DataView(message.buffer).getUint16(8, true), INPUT_VERSION);
  assert.equal(parsed.sequenceMode, "now");
  assert.equal(parsed.firstSeq, 0);
  assert.equal(parsed.count, 1);
  assert.deepEqual(parsed.keys, {forward: true, back: false, left: false, right: false, strafeLeft: false, strafeRight: false, attack: true, use: false, run: false});
  assert.equal(validateInput(parsed, {expectedSlot: 3, expectedBinding: BINDING, nowSeconds: NOW, nextSeq: 50000, window: 1}).ok, true);
  assert.deepEqual(Array.from(encodeKeyState({forward: true, attack: true})), Array.from(parsed.commands));
});

test("a signature does not transfer to another message, another key, or another tag", async () => {
  const wallet = await burner();
  const other = await burner();
  const message = sample();
  const signature = await signInput(wallet.privateKey, message);

  assert.equal(verifies(wallet, sample({firstSeq: 129}), signature), false, "different message");
  assert.equal(verifies(other, message, signature), false, "different key");
  assert.equal(verifies(wallet, message, new Uint8Array(64)), false, "zero signature");
  assert.equal(verifies(wallet, message, signature.subarray(0, 63)), false, "short signature");
  assert.equal(verifies(wallet, message, new Uint8Array(65)), false, "long signature");

  // The same private key must not validate under the bare message without the
  // domain tag, which is what the tag is there to stop.
  const raw = createPrivateKey({
    key: Buffer.from(wallet.pkcs8),
    format: "der",
    type: "pkcs8",
  });
  const untagged = nodeSign(null, Buffer.from(message), raw);
  assert.equal(nodeVerify(null, Buffer.from(message), createPublicKey(raw), Buffer.from(untagged)), true);
  assert.equal(nodeVerify(null, Buffer.from(signingPayload(message)), createPublicKey(raw), Buffer.from(untagged)), false, "tag required");
});

test("a signature over a slot-binding for one context does not verify for another", async () => {
  const wallet = await burner();
  const a = await slotBinding({context: "9kmADZit1MbcEJ2iej61QQBQnZZWM7w1gaPV4ZrtsTBd", meta: "21nbvnd67avq3faDzkbGWWe3CvDarfttBAC4D2aQjkTS", inputBuffer: "8SNVxSUj8m6yTFAWwYfQq6WEUNB1pUxDG8yTaBWdooUN"});
  const b = await slotBinding({context: "9kmADZit1MbcEJ2iej61QQBQnZZWM7w1gaPV4ZrtsTBd", meta: "21nbvnd67avq3faDzkbGWWe3CvDarfttBAC4D2aQjkTS", inputBuffer: "FfixNxJ4ZhiBwQZTScnhFTjsm1crVZiRb86P7FiRPfs8"});
  assert.notEqual(hex(a), hex(b), "a different input buffer gives a different binding");
  assert.equal(a.length, 32);

  const forA = sample({binding: a});
  const signature = await signInput(wallet.privateKey, forA);
  assert.equal(verifies(wallet, forA, signature), true);
  assert.equal(verifies(wallet, sample({binding: b}), signature), false);
});

test("the slot binding is stable and order-sensitive", async () => {
  const args = {context: "9kmADZit1MbcEJ2iej61QQBQnZZWM7w1gaPV4ZrtsTBd", meta: "21nbvnd67avq3faDzkbGWWe3CvDarfttBAC4D2aQjkTS", inputBuffer: "8SNVxSUj8m6yTFAWwYfQq6WEUNB1pUxDG8yTaBWdooUN"};
  const first = hex(await slotBinding(args));
  const second = hex(await slotBinding({...args}));
  assert.equal(first, second, "same inputs, same binding");
  const swapped = hex(await slotBinding({...args, context: args.meta, meta: args.context}));
  assert.notEqual(first, swapped, "context and meta are not interchangeable");
  const missing = hex(await slotBinding({context: args.context, meta: null, inputBuffer: null}));
  assert.equal(missing.length, 64, "absent accounts hash as zero, they do not throw");
  assert.notEqual(first, missing);
});

// ---------------------------------------------------------------- validation

test("a well-formed batch passes validation", () => {
  const parsed = decodeInput(sample());
  const verdict = validateInput(parsed, {expectedSlot: 3, expectedBinding: BINDING, nowSeconds: NOW, nextSeq: 128});
  assert.equal(verdict.ok, true);
});

test("validation refuses the wrong slot and the wrong context", () => {
  const parsed = decodeInput(sample());
  assert.match(validateInput(parsed, {expectedSlot: 4, expectedBinding: BINDING, nowSeconds: NOW}).reason, /slot 3, session is slot 4/);
  const other = new Uint8Array(32).fill(9);
  assert.match(validateInput(parsed, {expectedSlot: 3, expectedBinding: other, nowSeconds: NOW}).reason, /binding does not match/);
});

test("validation refuses a stale timestamp and a wild one", () => {
  const parsed = decodeInput(sample());
  assert.equal(validateInput(parsed, {nowSeconds: NOW, maxSkewSeconds: 120}).ok, true);
  assert.match(validateInput(parsed, {nowSeconds: NOW + 121, maxSkewSeconds: 120}).reason, /issued_at/);
  assert.match(validateInput(parsed, {nowSeconds: NOW - 121, maxSkewSeconds: 120}).reason, /issued_at/);
  assert.equal(validateInput(parsed, {nowSeconds: NOW + 10_000, maxSkewSeconds: 120}).ok, false, "the future is not exempt");
});

test("validation refuses replay behind the slot and runaway lookahead", () => {
  const parsed = decodeInput(sample());
  assert.match(validateInput(parsed, {nextSeq: 200}).reason, /behind the slot's next_seq 200/);
  assert.match(validateInput(parsed, {nextSeq: 0, window: 100}).reason, /more than 100 ahead/);
  // A small retry window in front of the cursor is allowed.
  assert.equal(validateInput(parsed, {nextSeq: 100, window: 100}).ok, true);
});

test("validation refuses BT_SPECIAL, which the program would refuse anyway", () => {
  const bad = commands(3);
  bad[7] = BT_SPECIAL;
  const parsed = decodeInput(sample({commands: bad}));
  const verdict = validateInput(parsed, {nowSeconds: NOW});
  assert.equal(verdict.ok, false);
  assert.match(verdict.reason, /command 1 sets BT_SPECIAL/);

  // The top button bit is fine; only 0x80 is special.
  const topBit = commands(1);
  topBit[3] = 0x7f;
  assert.equal(validateInput(decodeInput(sample({commands: topBit})), {nowSeconds: NOW}).ok, true);
  // And a command with 0x80 anywhere in the buttons byte is caught.
  for (const value of [0x80, 0x81, 0xff]) {
    const probe = commands(1);
    probe[3] = value;
    assert.equal(validateInput(decodeInput(sample({commands: probe})), {nowSeconds: NOW}).ok, false, `buttons 0x${value.toString(16)}`);
  }
});

test("button bits are the vanilla BT_ values and 0x80 is not among them", () => {
  assert.equal(BT_ATTACK, 0x01);
  assert.equal(BT_USE, 0x02);
  assert.equal(BT_SPEED, 0x04);
  for (const bit of [BT_ATTACK, BT_USE, BT_SPEED]) {
    assert.equal(bit & BT_SPECIAL, 0, `0x${bit.toString(16)} must not touch BT_SPECIAL`);
  }
  assert.equal(BT_ATTACK | BT_USE | BT_SPEED, 0x07);
});

// ---------------------------------------------------------------- ticcmds

test("ticcmd packs four bytes with the program's own scaling", () => {
  // forwardmove and sidemove are signed bytes; angleturn is byte << 8; the
  // program reads byte 2 as (unsigned char) << 8, so 4 is +1024 and 252 is
  // -1024 (stunts/doom-host/patches/0010-...:88).
  assert.deepEqual(ticcmd(), [0, 0, 0, 0]);
  assert.deepEqual(ticcmd({forward: 50}), [50, 0, 0, 0]);
  assert.deepEqual(ticcmd({forward: -50}), [206, 0, 0, 0], "-50 wraps to 0xce");
  assert.deepEqual(ticcmd({side: -50}), [0, 206, 0, 0]);
  assert.deepEqual(ticcmd({turn: 4 * 256}), [0, 0, 4, 0], "turn right");
  assert.deepEqual(ticcmd({turn: -4 * 256}), [0, 0, 252, 0], "turn left is 252, not 4-256");
  assert.deepEqual(ticcmd({buttons: BT_ATTACK | BT_USE}), [0, 0, 0, 3]);

  // Out-of-range values are masked into the byte, never thrown, so a held key
  // can never produce a message the relayer would have to special-case.
  assert.deepEqual(ticcmd({forward: 1000}), [232, 0, 0, 0]);
  assert.deepEqual(ticcmd({buttons: 0x1ff}), [0, 0, 0, 0xff]);
});

test("held keys become a per-tic command stream", () => {
  const none = heldToCommands({}, 0, 8);
  assert.equal(none.length, 8 * COMMAND_BYTES);
  assert.ok(Array.from(none).every((byte) => byte === 0), "no keys held is a neutral command every tic");

  const forward = heldToCommands({forward: true}, 0, 4);
  assert.deepEqual(Array.from(forward.subarray(0, 4)), [50, 0, 0, 0]);
  assert.deepEqual(Array.from(forward.subarray(4, 8)), [50, 0, 0, 0], "forward is applied every tic");

  const strafe = heldToCommands({strafeRight: true}, 0, 1);
  assert.deepEqual(Array.from(strafe), [0, 50, 0, 0]);
  const strafeLeft = heldToCommands({strafeLeft: true}, 0, 1);
  assert.deepEqual(Array.from(strafeLeft), [0, 206, 0, 0]);

  const fire = heldToCommands({attack: true}, 0, 1);
  assert.deepEqual(Array.from(fire), [0, 0, 0, BT_ATTACK]);
  const all = heldToCommands({attack: true, use: true, run: true}, 0, 1);
  assert.deepEqual(Array.from(all), [0, 0, 0, BT_ATTACK | BT_USE | BT_SPEED]);
});

test("turn is contributed on every fourth tic, as vanilla accumulates it", () => {
  assert.equal(TICCMD_HORIZONTAL, 4);
  // A batch starting on tic 0 turns on tic 0 but not on tics 1, 2, 3.
  // Doom turns counter-clockwise for a positive angleturn: left is +4, right 252.
  const batch = heldToCommands({left: true}, 0, 8);
  const turnByte = (tic) => batch[tic * COMMAND_BYTES + 2];
  assert.deepEqual([0, 1, 2, 3, 4, 5, 6, 7].map(turnByte), [4, 0, 0, 0, 4, 0, 0, 0]);

  // A batch that starts mid-cycle keeps the global tic alignment, so a player
  // holding one key does not get a different turn rate depending on where a
  // batch boundary happened to fall.
  const offset = heldToCommands({right: true}, 3, 4);
  assert.deepEqual([3, 4, 5, 6].map((tic) => offset[(tic - 3) * COMMAND_BYTES + 2]), [0, 252, 0, 0]);
});

test("heldToCommands never emits BT_SPECIAL", () => {
  const every = [
    {forward: true, back: true, left: true, right: true, strafeLeft: true, strafeRight: true},
    {attack: true, use: true, run: true},
  ];
  for (const keys of every) {
    for (let start = 0; start < 8; start += 1) {
      const bytes = heldToCommands(keys, start, 8);
      for (let i = 0; i < bytes.length; i += COMMAND_BYTES) {
        assert.equal(bytes[i + 3] & BT_SPECIAL, 0, `buttons byte at tic ${start + i / COMMAND_BYTES}`);
      }
    }
  }
});

test("a 64-command batch from held keys validates end to end", async () => {
  const wallet = await burner();
  const message = encodeInput({
    slot: 0,
    firstSeq: 0,
    commands: heldToCommands({forward: true, right: true, attack: true}, 0, MAX_COMMANDS_PER_BATCH),
    clientSeq: 1,
    issuedAt: NOW,
    binding: BINDING,
  });
  const signature = await signInput(wallet.privateKey, message);
  assert.equal(verifies(wallet, message, signature), true);
  const parsed = decodeInput(message);
  assert.equal(parsed.count, MAX_COMMANDS_PER_BATCH);
  assert.equal(validateInput(parsed, {expectedSlot: 0, expectedBinding: BINDING, nowSeconds: NOW, nextSeq: 0}).ok, true);
});
