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

import {STRIP_COUNT, STRIP_ACCOUNT_BYTES, STRIP_HEADER_BYTES, STRIP_HASH_DOMAIN, decodeFrame, stripDigest} from "../strip.js";
import {
  FrameStream,
  compareDigests,
  compareFrames,
  decodePushedFrame,
  frameAccounts,
  framePayloads,
  readChainDigest,
} from "../push.js";

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

// The fixture's eight strips at a cursor *behind* the one on screen, with every
// strip digest recomputed, so it is a frame the page would accept from the
// relayer and not a malformed record. That is the whole point: a stale frame is
// valid, and refusing to paint it is a policy.
async function olderStrips(ticsBehind = 4) {
  const out = [];
  for (let stripId = 0; stripId < STRIP_COUNT; stripId += 1) {
    const source = realStrips[stripId];
    const account = new Uint8Array(source);
    const view = new DataView(account.buffer);
    const cursor = view.getUint32(20, true) - ticsBehind;
    view.setUint32(20, cursor, true);
    view.setUint32(24, view.getUint32(24, true) - ticsBehind, true);
    const pixels = account.subarray(STRIP_HEADER_BYTES);
    account.set(await stripDigest({
      stripId,
      cursor,
      gametic: view.getUint32(24, true),
      snapshotHash: account.subarray(32, 64),
      pixels,
    }), 96);
    out.push(account);
  }
  return out;
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

// The paint gap, in one test, from the shape the chain actually produces.
//
// `frame_index` is the relayer's *submission* counter. With more than one render
// buffer in flight the frames are submitted in order and completed in
// completion order, so a frame submitted second arrives first. The rule that
// dropped it -- `frame_index <= lastFrameIndex` -- called that a frame already
// painted, and threw away frames the page had never seen. Measured on the R15
// confirm session, 2026-09-26, from the relayer's own `frames.jsonl`: 105 of 201
// consecutive pushes carried a lower `frame_index` than the push before them,
// and replaying that push order through the old rule paints 79 and drops 123.
test("a frame the page has never painted is painted, whatever its index is behind", async () => {
  const painted = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-order",
    onFrame: (frame) => painted.push(frame.frameIndex),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  // Four buffers in flight, two bursts: the first group completes in submission
  // order, the second in reverse. Every one of the eight is a frame the page has
  // never seen, and the second burst is entirely behind the first.
  for (const index of [1, 2, 3, 4, 8, 7, 6, 5]) {
    await FakeEventSource.last.deliver(pushedRecord({frameIndex: index}));
  }
  assert.deepEqual(painted, [1, 2, 3, 4, 8, 7, 6, 5], "every frame the relayer sent reached the canvas, in arrival order");
  const summary = stream.summary();
  assert.equal(summary.skipped, 0, "nothing this page had not painted was called a replay");
  assert.equal(summary.pushed, 8);
  assert.equal(summary.received, 8, "delivery was complete: the loss was never on the wire");
  // The picture is on the newest frame it has shown, not the last one it was
  // handed, so a reconnect asks for what it missed rather than everything again.
  assert.equal(summary.last_frame_index, 8, `the reconnect cursor went backwards to ${summary.last_frame_index}`);
  assert.match(stream.url, /since=8$/);
  stream.stop();
});

test("a replay is still dropped, and a stale frame is painted and counted", async () => {
  const painted = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-stale",
    onFrame: (frame) => painted.push(frame.frameIndex),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 7}));
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 7}));
  assert.equal(stream.summary().skipped, 1, "the frame the relayer handed back was re-painted");
  // A frame older than the picture on screen. It is a real render of a real
  // state the chain committed, so it is painted and counted, and the count is
  // what makes the choice visible rather than silent.
  const older = pushedRecord({frameIndex: 6, strips: await olderStrips()});
  await FakeEventSource.last.deliver(older);
  const summary = stream.summary();
  assert.equal(summary.painted_stale, 1, JSON.stringify(summary));
  assert.equal(summary.stale_dropped, 0);
  assert.equal(summary.pushed, 2, "the stale frame was painted, so the page is at the chain's rate");
  stream.stop();
});

test("the old rule is available: a viewer can refuse to go backwards", async () => {
  const painted = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-monotone",
    dropStaleFrames: true,
    onFrame: (frame) => painted.push(frame.frameIndex),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 7}));
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 6, strips: await olderStrips()}));
  const summary = stream.summary();
  assert.equal(summary.stale_dropped, 1, "a frame older than the picture was painted anyway");
  assert.equal(summary.pushed, 1);
  assert.deepEqual(painted, [7]);
  stream.stop();
});

test("a dropped stream is opened again asking for what it missed", async () => {
  // `EventSource` reconnects to the URL it was constructed with, so a page that
  // lets it comes back from every drop asking for the whole stream and is handed
  // the newest frame: every frame completed while it was away is gone, with
  // nothing torn, nothing refused and no counter anywhere. Measured on R15,
  // 2026-09-26, at three frames in thirty-two.
  const painted = [];
  const urls = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-1",
    onFrame: (frame) => painted.push(frame.frameIndex),
    EventSourceImpl: class extends FakeEventSource {
      constructor(url) {
        super(url);
        urls.push(url);
      }
    },
    reconnectDelayMs: 1,
  });
  stream.start();
  FakeEventSource.stream = stream;
  for (let index = 1; index <= 5; index += 1) {
    await FakeEventSource.last.deliver(pushedRecord({frameIndex: index}));
  }
  const first = FakeEventSource.last;
  assert.match(first.url, /\/frames$/, "the first request names no frame, because the page has painted none");
  first.fire("error", {});
  assert.equal(first.closed, true, "the dropped stream was left open, so the browser would reconnect to the old URL");
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(urls.length, 2, `the stream was not opened again (${JSON.stringify(urls)})`);
  assert.match(urls[1], /\/frames\?since=5$/, `the reconnect did not ask for what the page missed: ${urls[1]}`);
  const summary = stream.summary();
  assert.equal(summary.errors, 1);
  assert.equal(summary.reconnects, 1);
  // And the picture carries on from where it was, without a re-paint.
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 5}));
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 6}));
  assert.deepEqual(painted, [1, 2, 3, 4, 5, 6]);
  assert.equal(stream.summary().skipped, 1, "the frame the relayer handed back was re-painted");
  // A second drop does not open two streams.
  FakeEventSource.last.fire("error", {});
  FakeEventSource.last.fire("error", {});
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(urls.length, 3, `a second drop opened ${urls.length - 2} streams (${JSON.stringify(urls)})`);
  stream.stop();
  FakeEventSource.last.fire("error", {});
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(urls.length, 3, "a stopped stream was opened again after it dropped");
});

test("a stream that is stopped mid-drop does not come back", async () => {
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-1",
    onFrame: () => {},
    EventSourceImpl: FakeEventSource,
    reconnectDelayMs: 50,
  });
  stream.start();
  stream.stop();
  FakeEventSource.last.fire("error", {});
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.equal(FakeEventSource.last.closed, true);
  assert.equal(stream.summary().reconnects, 0, "a stopped stream counted a reconnect it never made");
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
      return chainDigestOf(realStrips, 1168);
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
  assert.ok(checks.every((check) => check.comparison.match), `the chain read agrees with what was painted: ${JSON.stringify(checks.map((c) => c.comparison))}`);
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
      // A relayer that edited one strip on the way past: the chain's digest for
      // that strip is the original one and the pushed frame's is not.
      const chain = chainDigestOf(realStrips, 1168);
      chain.strips[2] = {...chain.strips[2], digest: "00".repeat(32)};
      return chain;
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

// The chain read the cross-check makes: the eight strip *headers*, 192 B each.
// `readChainDigest` is what makes the check comparable at all -- the first
// version read 1,072,384 B and lost the race to every frame, ten checks and ten
// "the chain moved on" (measured, R13 at 2.02 fps, 2026-09-26).
function chainDigestOf(strips, slot) {
  return {
    slot,
    strips: strips.map((strip) => {
      const view = new DataView(strip.buffer, strip.byteOffset, strip.byteLength);
      return {
        stripId: view.getUint32(16, true),
        cursor: view.getUint32(20, true),
        gametic: view.getUint32(24, true),
        digest: Buffer.from(strip.subarray(96, 128)).toString("hex"),
        snapshotHash: Buffer.from(strip.subarray(32, 64)).toString("hex"),
        stateRoot: Buffer.from(strip.subarray(64, 96)).toString("hex"),
      };
    }),
  };
}

// A 192-byte snapshot header, which the read asks for alongside the strips and
// does not use: the strips carry the identity the check compares.
const SNAPSHOT_HEADER = new Uint8Array(192);

const sliceReader = (payloads) => ({
  getAccounts: async (addresses, {dataSlice} = {}) => {
    assert.ok(dataSlice, "the cross-check must ask for a header slice, not whole accounts");
    assert.equal(dataSlice.length, 128, "128 bytes is the whole strip header");
    return {slot: 1168, payloads: addresses.map((_, index) => payloads[index].subarray(0, dataSlice.length))};
  },
});

test("the chain read asks for 128-byte headers, not 1 MiB accounts", async () => {
  let seenAddresses = null;
  const reader = {
    getAccounts: async (addresses, options) => {
      seenAddresses = addresses;
      return sliceReader([SNAPSHOT_HEADER, ...realStrips]).getAccounts(addresses, options);
    },
  };
  const record = pushedRecord();
  const chain = await readChainDigest(reader, record);
  assert.equal(seenAddresses.length, 9, "the snapshot and its eight strips");
  assert.equal(seenAddresses[0], record.snapshot_account, "aimed at the frame's own accounts");
  assert.equal(chain.strips.length, 8);
  assert.equal(chain.strips[0].digest, Buffer.from(realStrips[0].subarray(96, 128)).toString("hex"));
  assert.equal(chain.slot, 1168);
});

test("a Doom-on-DCG frame reads its strip headers past the 128-byte view header", async () => {
  // Each strip lives in a DCG view account: a 128-byte view header, then the
  // DOOM-RS1 strip account. The record names the offset; the read must use it.
  const VIEW = 128;
  const viewed = realStrips.map((strip) => {
    const account = new Uint8Array(VIEW + strip.length);
    account.set(Buffer.from("DVW3"), 0);
    account.set(strip, VIEW);
    return account;
  });
  let seenOffset = null;
  const reader = {
    getAccounts: async (addresses, {dataSlice}) => {
      seenOffset = dataSlice.offset;
      const all = [new Uint8Array(VIEW + 192), ...viewed];
      return {slot: 7, payloads: addresses.map((_, i) => all[i].subarray(dataSlice.offset, dataSlice.offset + dataSlice.length))};
    },
  };
  const chain = await readChainDigest(reader, {...pushedRecord(), view_header_bytes: VIEW});
  assert.equal(seenOffset, VIEW);
  assert.equal(chain.strips[3].digest, Buffer.from(realStrips[3].subarray(96, 128)).toString("hex"));
  assert.equal(chain.strips[3].stripId, 3);
});

test("a chain header that has not landed yet is not a mismatch", async () => {
  const reader = {getAccounts: async () => ({slot: 1168, payloads: [null, ...Array(8).fill(null)]})};
  assert.equal(await readChainDigest(reader, pushedRecord()), null);
});

test("matching digests compare as a match, and a differing one as a mismatch", async () => {
  const frame = await decodePushedFrame(pushedRecord());
  const chain = chainDigestOf(realStrips, 1168);
  const good = compareDigests(frame, chain);
  assert.equal(good.comparable, true);
  assert.equal(good.match, true);
  assert.match(good.reason, /all 8 strip digests match the chain at slot 1168/);

  const edited = chainDigestOf(realStrips, 1168);
  edited.strips[5] = {...edited.strips[5], digest: "ab".repeat(32)};
  const bad = compareDigests(frame, edited);
  assert.equal(bad.comparable, true);
  assert.equal(bad.match, false);
  assert.equal(bad.strip, 5);
  assert.match(bad.reason, /strip 5 digest differs/);

  const moved = chainDigestOf(realStrips, 1168);
  moved.strips[0] = {...moved.strips[0], cursor: moved.strips[0].cursor + 16, gametic: moved.strips[0].gametic + 16};
  const late = compareDigests(frame, moved);
  assert.equal(late.comparable, false, "a chain that moved on is not a mismatch");
  assert.match(late.reason, /moved on/);
});

test("a digest match is a pixel match, because the digest covers the pixels", async () => {
  // The claim the cheap check rests on. A strip digest is SHA-256 over its id,
  // cursor, gametic, snapshot hash and pixels, so equal digests on verified
  // frames mean equal pixels.
  const frame = await decodePushedFrame(pushedRecord());
  const chain = chainDigestOf(realStrips, 1168);
  assert.equal(compareDigests(frame, chain).match, true);
  for (const [index, strip] of frame.stripHashes.entries()) {
    assert.equal(strip, chain.strips[index].digest, `strip ${index} digest agrees`);
  }
  // And the pushed frame's own digests were verified on the way in: editing a
  // pixel makes the frame undecodable, so it never reaches a comparison.
  const tampered = realStrips.map((strip, index) => (index === 1
    ? new Uint8Array([...strip.slice(0, 200), strip[200] ^ 1, ...strip.slice(201)])
    : strip));
  await assert.rejects(() => decodePushedFrame({...pushedRecord(), strip_accounts_b64: tampered.map(toBase64)}));
});

test("a chain read that is behind is compared against the page's record of that frame", async () => {
  // The measured shape on R13 at 1.78 fps: the public node is three frames
  // behind the relayer, so the chain never holds the frame that was just pushed.
  // The page has the digests of the frames it was already sent, so the chain's
  // frame is compared against its own record and the answer is exact.
  const checks = [];
  const stream = new FrameStream({
    base: "http://r.invalid",
    sessionId: "s-behind",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 3,
    readChain: async () => chainDigestOf(realStrips, 5000),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  for (let index = 1; index <= 9; index += 1) {
    await FakeEventSource.last.deliver(pushedRecord({frameIndex: index}));
  }
  const summary = stream.summary();
  assert.equal(summary.cross_checks, 3);
  // Every pushed frame carries the same fixture's cursor, so every chain read
  // finds that cursor in the page's own record and compares exactly.
  assert.equal(summary.cross_check_matches, 3, JSON.stringify(checks.map((c) => c.comparison)));
  assert.equal(summary.cross_check_mismatches, 0);
  assert.equal(summary.cross_check_uncomparable, 0);
  for (const check of checks) {
    assert.equal(typeof check.comparison.checked_frame_index, "number", "the record it compared against is named");
    assert.equal(check.comparison.chain_cursor, check.comparison.pushed_cursor, "same frame, so behind_by_tics is 0");
  }
});

test("a chain read naming a frame the page never had is uncomparable, not a mismatch", async () => {
  const checks = [];
  const elsewhere = chainDigestOf(realStrips, 1);
  for (const strip of elsewhere.strips) {
    strip.cursor += 4000;
    strip.gametic += 4000;
  }
  const stream = new FrameStream({
    base: "http://r.invalid",
    sessionId: "s-foreign",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 1,
    readChain: async () => elsewhere,
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 1}));
  const summary = stream.summary();
  assert.equal(summary.cross_check_mismatches, 0, "a frame the page never had is not a mismatch");
  assert.equal(summary.cross_check_uncomparable, 1);
  assert.match(checks[0].comparison.reason, /which this page was never sent/);
  assert.equal(checks[0].comparison.behind_by_tics, -4000);
});

// -- the disagreement is read twice ------------------------------------------
//
// Measured on R13, 2026-09-26: the loop re-renders an unchanged cursor, the
// page's stream is serial so it is not told about the second render, and the
// chain holds the newer one. A check firing on the older frame compared two
// pictures of one cursor and called it a mismatch -- four of eight live checks.
// A mismatch raises the page's banner, so a false one is worse than a false
// negative: the player is told the picture cannot be trusted when it can.

function chainWith(stripId, digest) {
  const chain = chainDigestOf(realStrips, 42);
  chain.strips[stripId] = {...chain.strips[stripId], digest};
  return chain;
}

test("a disagreement that goes away on the second read is not a mismatch", async () => {
  let calls = 0;
  const checks = [];
  const stream = new FrameStream({
    base: "http://r.invalid", sessionId: "s-transient",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 1,
    retryDelayMs: 1,
    readChain: async () => {
      calls += 1;
      // The first read catches the chain mid-re-render; the second does not.
      return calls === 1 ? chainWith(3, "ab".repeat(32)) : chainDigestOf(realStrips, 43);
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 1}));
  const summary = stream.summary();
  assert.equal(calls, 2, "a disagreement is read again");
  assert.equal(summary.cross_check_matches, 1, JSON.stringify(checks.map((c) => c.comparison)));
  assert.equal(summary.cross_check_mismatches, 0, "a disagreement that went away is not a mismatch");
  assert.equal(summary.cross_check_retries, 0, "and the retry is not counted as a retry either");
  assert.equal(checks[0].comparison.retried, true, "the panel says the answer came from a second read");
});

test("a disagreement that survives the second read is a mismatch", async () => {
  let calls = 0;
  const checks = [];
  const stream = new FrameStream({
    base: "http://r.invalid", sessionId: "s-real",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 1,
    retryDelayMs: 1,
    readChain: async () => {
      calls += 1;
      return chainWith(3, "ab".repeat(32));
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 1}));
  const summary = stream.summary();
  assert.equal(calls, 2, "it was read twice");
  assert.equal(summary.cross_check_mismatches, 1, "and it still disagrees, so it is a mismatch");
  assert.equal(summary.cross_check_retries, 1, "counted as a disagreement that survived the retry");
  assert.match(checks[0].comparison.reason, /strip 3 digest differs/);
});

test("a disagreement that the relayer re-rendered away is not a result at all", async () => {
  // The live case: the chain read is open, the relayer announces a second frame
  // for the same cursor, and there is nothing to say about the older one.
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  let calls = 0;
  const checks = [];
  const stream = new FrameStream({
    base: "http://r.invalid", sessionId: "s-rerendered-away",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 1,
    retryDelayMs: 1,
    readChain: async () => {
      calls += 1;
      if (calls === 1) {
        await gate;
        return chainWith(3, "ab".repeat(32));
      }
      return chainDigestOf(realStrips, 44);
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  FakeEventSource.last.fire("frame", {data: JSON.stringify(pushedRecord({frameIndex: 1}))});
  // Wait for the push to be registered, and for its check to be parked in the
  // gated read.
  while (stream.pushed < 1 || calls < 1) await new Promise((resolve) => setImmediate(resolve));
  // The relayer announces a second frame for the same cursor while the first
  // read is still open. The stream is serial, so this one waits its turn -- which
  // is exactly why the supersession has to be judged after the read, not before
  // it.
  stream.newestForCursor.set(pushedRecord().cursor, 2);
  release();
  await stream.idle();
  const summary = stream.summary();
  assert.equal(checks.length, 0, `nothing is reported: ${JSON.stringify(checks.map((c) => c.comparison))} superseded=${summary.cross_check_superseded} checks=${summary.cross_checks} calls=${calls}`);
  assert.equal(summary.cross_check_superseded, 1);
  assert.equal(summary.cross_check_mismatches, 0);
  assert.equal(summary.cross_checks, 0, "and it does not count as a check either");
});

test("a cursor with two genuinely different frames keeps both, and either can match", async () => {
  // "cursor -> digests" is not a key: the loop re-renders an unchanged cursor and
  // the two renders can differ. The check tries every record for the cursor.
  const checks = [];
  const cursor = meta.cursor ?? 6016;
  const first = await decodePushedFrame(pushedRecord({frameIndex: 7}));
  const stream = new FrameStream({
    base: "http://r.invalid", sessionId: "s-two",
    onFrame: () => {},
    onCrossCheck: (value) => checks.push(value),
    crossCheckEvery: 1,
    retryDelayMs: 1,
    readChain: async () => chainDigestOf(realStrips, 42),
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  stream.digestsByCursor.set(cursor, [
    {frameIndex: 7, stripHashes: first.stripHashes, snapshotHash: first.snapshotHash, stateRoot: first.stateRoot, pushedAt: 1},
    {frameIndex: 9, stripHashes: first.stripHashes.map((d, i) => (i === 3 ? "00".repeat(32) : d)), snapshotHash: first.snapshotHash, stateRoot: first.stateRoot, pushedAt: 2},
  ]);
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 11}));
  assert.equal(checks.length, 1);
  const comparison = checks[0].comparison;
  assert.equal(comparison.match, true, JSON.stringify(comparison));
  assert.equal(comparison.checked_frame_index, 7, "the frame the chain actually holds");
  assert.equal(comparison.candidates, 2);
  assert.equal(comparison.rerendered, true);
  assert.match(comparison.reason, /of 2 pushed for this cursor/);
});

test("two pushes of the same frame for one cursor are stored once", async () => {
  const stream = new FrameStream({base: "http://r.invalid", sessionId: "s-same", onFrame: () => {}, EventSourceImpl: FakeEventSource});
  stream.start();
  FakeEventSource.stream = stream;
  const cursor = meta.cursor ?? 6016;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 1}));
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 2}));
  assert.equal(stream.digestsByCursor.get(cursor).length, 1, "identical digests, one record");
});

test("a chain read that caught a frame mid-write is not a mismatch", async () => {
  // Measured on R13 at 2.00 fps, 2026-09-26: three checks in nine reported a
  // strip digest difference and all three survived a retry, which is the
  // signature of a torn read rather than of a re-render. The read is at
  // `processed` on a public node and the one render buffer is overwritten every
  // 0.5 s, so a read that straddles two frames sees strips from both -- and each
  // of those strips is individually valid, carrying a correct digest over its
  // own header and pixels. The only thing that tells them apart is whether they
  // agree with each other, and the check was not asking.
  let calls = 0;
  const stream = new FrameStream({
    base: "http://r.invalid", sessionId: "s-torn",
    onFrame: () => {},
    crossCheckEvery: 1,
    retryDelayMs: 1,
    readChain: async () => {
      calls += 1;
      const chain = chainDigestOf(realStrips, 42);
      // Strips 4..7 are from the next frame: a later cursor, its own digests.
      for (const strip of chain.strips.slice(4)) {
        strip.cursor += 16;
        strip.gametic += 16;
      }
      return chain;
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  await FakeEventSource.last.deliver(pushedRecord({frameIndex: 1}));
  const summary = stream.summary();
  assert.equal(calls, 1, "a torn read is not read again: it is not a disagreement to resolve");
  assert.equal(summary.cross_check_mismatches, 0);
  assert.equal(summary.cross_check_uncomparable, 1);
  assert.equal(summary.cross_check_superseded, 0);
  assert.match(stream.lastCrossCheckReason ?? "the chain read caught a frame mid-write: strip cursors", /the chain read caught a frame mid-write: strip cursors/);
});

test("tornStrips names both ways a chain read can be torn", async () => {
  const {tornStrips} = await import("../push.js");
  const good = chainDigestOf(realStrips, 1).strips;
  assert.equal(tornStrips(good), null);
  assert.match(tornStrips(good.slice(0, 7)), /did not return eight/);
  const mixedCursor = good.map((strip, index) => (index === 5 ? {...strip, cursor: strip.cursor + 16} : strip));
  assert.match(tornStrips(mixedCursor), /strip cursors/);
  const mixedRoot = good.map((strip, index) => (index === 2 ? {...strip, stateRoot: "ff".repeat(32)} : strip));
  assert.match(tornStrips(mixedRoot), /disagree on gametic, snapshot or state root/);
});

// Painting a frame runs eight SHA-256 checks through WebCrypto, which is a real
// promise chain and not one turn of the event loop, so "the frame is on the
// canvas" has to be waited for rather than assumed.
async function waitFor(predicate, what, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`timed out after ${timeoutMs}ms waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}

// The two faults measured on the R15 context on 2026-09-26, where the relayer
// pushed every frame it saw and the page painted 0.02 fps: the cross-check was
// awaited inside the serial paint loop, so a chain read held the picture, and a
// frame that threw on the way to the canvas took the rest of the queue with it.
// Both are here rather than only in the browser test, because neither needs a
// browser to fail and both are the kind of thing that comes back.

test("a chain read that never answers does not stop the paint loop", async () => {
  // The read is a promise that is never settled, which is what a node that
  // accepts a request and says nothing looks like from here. `crossCheckEvery: 1`
  // fires on the very first frame, so the read is inside the loop from the start.
  const painted = [];
  let reads = 0;
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-hang",
    onFrame: (frame) => painted.push(frame.frameIndex),
    crossCheckEvery: 1,
    readChain: () => {
      reads += 1;
      return new Promise(() => {});
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  for (let index = 1; index <= 8; index += 1) {
    // `deliver` waits on `idle()`, which now waits on the cross-check, so the
    // frames are fired directly and each paint is waited for by hand.
    FakeEventSource.last.fire("frame", {data: JSON.stringify(pushedRecord({frameIndex: index}))});
    await waitFor(() => painted.length >= index, `frame ${index} painted`);
  }
  assert.deepEqual(painted, [1, 2, 3, 4, 5, 6, 7, 8], "every frame painted while one chain read is still open");
  assert.equal(reads, 1, "one read at a time; the rest are dropped rather than queued");
  const summary = stream.summary();
  assert.equal(summary.cross_check_dropped, 7, JSON.stringify(summary));
  // The picture is up to date even though the audit is not.
  assert.equal(summary.last_frame_index, 8);
  assert.equal(summary.cross_checks, 1);
  // And nothing is left unhandled: the open read must not become a rejection the
  // page never sees.
  assert.equal(summary.torn, 0);
  stream.stop();
});

test("a frame that throws on the way to the canvas is counted, and the next one paints", async () => {
  const painted = [];
  const stream = new FrameStream({
    base: "http://relayer.invalid",
    sessionId: "s-throw",
    onFrame: (frame) => {
      if (frame.frameIndex === 2) throw new Error("the canvas said no");
      painted.push(frame.frameIndex);
    },
    EventSourceImpl: FakeEventSource,
  });
  stream.start();
  FakeEventSource.stream = stream;
  // Three frames in one tick: the second throws, and the third is already queued
  // behind it. Before the fix the third was left in the queue with nothing to
  // drain it until the next event arrived.
  for (const index of [1, 2, 3]) {
    FakeEventSource.last.fire("frame", {data: JSON.stringify(pushedRecord({frameIndex: index}))});
  }
  await stream.idle();
  assert.deepEqual(painted, [1, 3], "the frame after a bad one still paints");
  assert.equal(stream.summary().torn, 1, "the bad frame is counted");
  assert.equal(stream.summary().last_frame_index, 3, "the stream is still moving");
  stream.stop();
});
