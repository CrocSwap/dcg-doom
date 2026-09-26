// Offline tests for the frame push.
//
// The fixture is the same eight real strip accounts test_strip.mjs uses, read
// from Fogo testnet, so a pushed frame here is the chain's own bytes rather
// than something this test invented. What is under test is the part that is new:
// that a pushed frame goes through the same whole-frame checks a chain read
// does, that the page paints them in order and never twice, and that the
// periodic chain cross-check can say "mismatch".

import test from "node:test";
import assert from "node:assert/strict";
import {gunzipSync} from "node:zlib";
import {createHash} from "node:crypto";
import {readFileSync} from "node:fs";
import {fileURLToPath} from "node:url";
import {dirname, join} from "node:path";

import {STRIP_COUNT, STRIP_ACCOUNT_BYTES, STRIP_HASH_DOMAIN, decodeFrame} from "../strip.js";
import {FrameStream, compareFrames, decodePushedFrame, frameAccounts, framePayloads} from "../push.js";

const here = dirname(fileURLToPath(import.meta.url));
const meta = JSON.parse(readFileSync(join(here, "fixtures", "frame-cursor-6016.json"), "utf8"));
const realStrips = [];
{
  const joined = gunzipSync(readFileSync(join(here, "fixtures", "frame-cursor-6016-strips.bin.gz")));
  for (let i = 0; i < STRIP_COUNT; i += 1) {
    realStrips.push(new Uint8Array(joined.subarray(i * STRIP_ACCOUNT_BYTES, (i + 1) * STRIP_ACCOUNT_BYTES)));
  }
}

const toBase64 = (bytes) => Buffer.from(bytes).toString("base64");
const sha256 = (bytes) => createHash("sha256").update(Buffer.from(bytes)).digest("hex");

// A pushed record, in the shape the relayer's frame event puts on the wire.
function pushedRecord({frameIndex = 1, strips = realStrips, accounts = null, frameSha = null} = {}) {
  return {
    frame_index: frameIndex,
    buffer: 0,
    cursor: meta.cursor ?? 6016,
    gametic: meta.gametic ?? null,
    frame_sha256: frameSha ?? sha256(decodeFrameSync(strips).pixels),
    snapshot_hash: meta.snapshot_hash ?? null,
    strip_accounts_b64: strips.map(toBase64),
    snapshot_account: "Snapshot1111111111111111111111111111111111",
    strip_accounts: accounts ?? Array.from({length: STRIP_COUNT}, (_, i) => `Strip${i}11111111111111111111111111111111111`),
  };
}

function decodeFrameSync(strips) {
  // decodeFrame is async because of the digest checks; the tests that build a
  // record only need the pixel digest, and `decodePushedFrame` is the async path
  // under test, so the pixels are assembled here directly.
  const pixels = new Uint8Array(320 * 200);
  for (const strip of strips) {
    const view = new DataView(strip.buffer, strip.byteOffset, strip.byteLength);
    const stripId = view.getUint32(16, true);
    const body = strip.subarray(128);
    for (let row = 0; row < 200; row += 1) {
      pixels.set(body.subarray(row * 40, row * 40 + 40), row * 320 + stripId * 40);
    }
  }
  return {pixels};
}

// A minimal EventSource, so the stream can be driven without a browser.
class FakeEventSource {
  static last = null;
  constructor(url) {
    this.url = url;
    this.listeners = new Map();
    this.closed = false;
    FakeEventSource.last = this;
  }
  addEventListener(name, handler) {
    if (!this.listeners.has(name)) this.listeners.set(name, []);
    this.listeners.get(name).push(handler);
  }
  close() {
    this.closed = true;
  }
  fire(name, event) {
    for (const handler of this.listeners.get(name) ?? []) handler(event);
  }
  async deliver(record) {
    this.fire("frame", {data: JSON.stringify(record)});
    // Painting a frame runs eight digest checks, so the handler is a promise
    // chain. The stream is asked when it is done rather than this guessing how
    // many turns of the event loop that takes.
    await FakeEventSource.stream?.idle();
  }
}

test("a pushed record becomes a frame the chain's own strips describe", async () => {
  const record = pushedRecord();
  const frame = await decodePushedFrame(record);
  assert.equal(frame.cursor, record.cursor, "the cursor is the strips' own");
  assert.equal(frame.pushed, true);
  assert.equal(frame.frameIndex, 1);
  assert.equal(frame.pixels.length, 320 * 200);
  // The same decoder, so the same whole-frame condition: the eight strips agree
  // on cursor, gametic, snapshot hash and state root, and each one's own
  // SHA-256 over header and pixels was recomputed and matched.
  const direct = await decodeFrame(realStrips);
  assert.equal(sha256(frame.pixels), sha256(direct.pixels), "pushed and read agree pixel for pixel");
});

test("a pushed frame whose pixels were edited on the way is refused", async () => {
  const record = pushedRecord();
  // The relayer rewrites one pixel of one strip. The strip's digest is over its
  // header and its pixels, so this cannot survive decodePushedFrame.
  const tampered = realStrips.map((strip, index) => {
    if (index !== 3) return strip;
    const copy = new Uint8Array(strip);
    copy[128 + 17] = (copy[128 + 17] + 1) & 0xff;
    return copy;
  });
  record.strip_accounts_b64 = tampered.map(toBase64);
  await assert.rejects(() => decodePushedFrame(record), /digest mismatch/);
});

test("a pushed frame that is eight strips of two frames is refused", async () => {
  const record = pushedRecord();
  // One strip from a different cursor: the eight no longer agree, which is the
  // torn frame the direct read used to catch by polling at the wrong moment.
  const other = new Uint8Array(realStrips[2]);
  new DataView(other.buffer).setUint32(20, 999999, true);
  record.strip_accounts_b64 = realStrips.map((strip, index) => toBase64(index === 2 ? other : strip));
  await assert.rejects(() => decodePushedFrame(record));
});

test("a record with no bytes, or the wrong number, is refused before decoding", () => {
  assert.equal(framePayloads(null), null);
  assert.equal(framePayloads({}), null);
  assert.equal(framePayloads({strip_accounts_b64: [toBase64(realStrips[0])]}), null);
  assert.equal(framePayloads({strip_accounts_b64: Array(8).fill("")}), null);
  const good = framePayloads(pushedRecord());
  assert.equal(good.length, STRIP_COUNT);
  assert.equal(good[0].length, STRIP_ACCOUNT_BYTES, "the pushed bytes are whole accounts, not pixels");
});

test("the accounts a record names are the ones a cross-check reads", () => {
  const accounts = frameAccounts(pushedRecord());
  assert.equal(accounts.strips.length, STRIP_COUNT);
  assert.match(accounts.snapshot, /^Snapshot1/);
  assert.equal(frameAccounts({strip_accounts: ["only one"]}), null);
  assert.equal(frameAccounts({strip_accounts: Array(8).fill(0)}), null);
});

test("the cross-check compares pixels, and says 'moved on' rather than 'mismatch'", async () => {
  const frame = await decodePushedFrame(pushedRecord());
  const same = {...frame};
  assert.deepEqual(
    {comparable: compareFrames(frame, same).comparable, match: compareFrames(frame, same).match},
    {comparable: true, match: true},
  );
  const edited = new Uint8Array(frame.pixels);
  edited[900] = (edited[900] + 1) & 0xff;
  const mismatch = compareFrames(frame, {...frame, pixels: edited});
  assert.equal(mismatch.comparable, true);
  assert.equal(mismatch.match, false);
  assert.equal(mismatch.first_pixel, 900);
  const moved = compareFrames(frame, {...frame, cursor: frame.cursor + 4, gametic: frame.gametic + 4});
  assert.equal(moved.comparable, false, "a frame that moved on is not a mismatch");
  assert.match(moved.reason, /moved on/);
});

test("the stream paints frames in arrival order, once each", async () => {
  const painted = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-1",
    onFrame: (frame) => painted.push(frame.frameIndex),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  assert.match(FakeEventSource.last.url, /\/v1\/sessions\/s-1\/frames$/);
  for (let index = 1; index <= 5; index += 1) {
    await FakeEventSource.last.deliver(pushedRecord({frameIndex: index}));
  }
  assert.deepEqual(painted, [1, 2, 3, 4, 5], "every frame painted, in order, none twice");
  // A reconnect hands back the frame already painted. Re-painting it would put
  // the picture backwards.
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 5}));
  assert.deepEqual(painted, [1, 2, 3, 4, 5]);
  assert.equal(stream.summary().skipped, 1);
  assert.equal(stream.summary().pushed, 5);
  stream.stop();
  assert.equal(FakeEventSource.last.closed, true);
});

test("the stream asks for frames after the last one it painted", () => {
  const stream = new FrameStream({base: "http://r.invalid", sessionId: "a b", EventSourceImpl: FakeEventSource});
  stream.lastFrameIndex = 41;
  assert.equal(stream.url, "http://r.invalid/v1/sessions/a%20b/frames?since=41");
});

test("one frame in twenty is read from the chain and compared", async () => {
  const reads = [];
  const checks = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-2",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 5,
    readChain: async (record) => {
      reads.push(record.frame_index);
      return decodeFrame(realStrips);
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  for (let index = 1; index <= 12; index += 1) {
    await FakeEventSource.last.deliver(pushedRecord({frameIndex: index}));
  }
  assert.deepEqual(reads, [5, 10], "every fifth frame is read from the chain, and only that one");
  assert.equal(checks.length, 2);
  assert.ok(checks.every((check) => check.comparison.match), "the chain read agrees with what was painted");
  const summary = stream.summary();
  assert.equal(summary.cross_checks, 2);
  assert.equal(summary.cross_check_matches, 2);
  assert.equal(summary.cross_check_mismatches, 0);
});

test("a chain read that disagrees is counted as a mismatch, not swallowed", async () => {
  const checks = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-3",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 1,
    readChain: async () => {
      const frame = await decodeFrame(realStrips);
      const pixels = new Uint8Array(frame.pixels);
      pixels[42] = (pixels[42] + 1) & 0xff;
      return {...frame, pixels};
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 1}));
  assert.equal(checks.length, 1);
  assert.equal(checks[0].comparison.match, false);
  assert.equal(stream.summary().cross_check_mismatches, 1);
  assert.equal(stream.summary().cross_check_uncomparable, 0);
});

test("a frame that does not decode is counted and the stream keeps going", async () => {
  const painted = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-4",
    onFrame: (frame) => painted.push(frame.frameIndex),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  const broken = pushedRecord({frameIndex: 1});
  broken.strip_accounts_b64 = realStrips.map((strip, index) => {
    if (index !== 0) return toBase64(strip);
    const copy = new Uint8Array(strip);
    copy[128] = (copy[128] + 1) & 0xff;
    return toBase64(copy);
  });
  await FakeEventSource.last.deliver(broken);
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 2}));
  assert.deepEqual(painted, [2], "one bad frame does not stop the stream");
  assert.equal(stream.summary().torn, 1);
  assert.match(stream.summary().last_error, /digest mismatch/);
});

test("a malformed event body is counted, not thrown", async () => {
  const stream = new FrameStream({base: "http://r.invalid", sessionId: "s-5", onFrame: () => {}, EventSourceImpl: FakeEventSource});
  stream.start();
  FakeEventSource.stream = stream;
  FakeEventSource.last.fire("frame", {data: "{not json"});
  for (let i = 0; i < 4; i += 1) await new Promise((resolve) => setImmediate(resolve));
  assert.equal(stream.summary().torn, 1);
  assert.equal(stream.summary().pushed, 0);
});

test("the strip digest domain is the one the program writes, NUL included", () => {
  assert.equal(STRIP_HASH_DOMAIN, "DOOM-RENDER-STRIP/1\0");
});
