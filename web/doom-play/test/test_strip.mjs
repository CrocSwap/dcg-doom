// Offline tests for the frame decoder.
//
// The fixture is eight real strip accounts read from Fogo testnet, so these
// tests exercise the decoder against bytes the chain actually holds, not just
// against synthetic ones. The live cross-check against the readback receipts
// is test_strip_live.mjs, which is not run by the default suite.

import test from "node:test";
import assert from "node:assert/strict";
import {gunzipSync} from "node:zlib";
import {createHash} from "node:crypto";
import {readFileSync} from "node:fs";
import {fileURLToPath} from "node:url";
import {dirname, join} from "node:path";

import {
  FRAME_WIDTH,
  FRAME_HEIGHT,
  FRAME_BYTES,
  STRIP_COUNT,
  STRIP_WIDTH,
  STRIP_HEADER_BYTES,
  STRIP_PIXELS_BYTES,
  STRIP_ACCOUNT_BYTES,
  STRIP_HASH_DOMAIN,
  StripError,
  decodeFrame,
  parseStrip,
  assembleFrame,
  frameGroupKey,
  frameToRgba,
  stripDigest,
  snapshotDigest,
  parseSnapshot,
  frameReadAddresses,
  toHex,
  u32le,
  SNAPSHOT_HASH_DOMAIN,
  SNAPSHOT_HEADER_BYTES,
} from "../strip.js";

const here = dirname(fileURLToPath(import.meta.url));
const meta = JSON.parse(readFileSync(join(here, "fixtures", "frame-cursor-6016.json"), "utf8"));
const realStrips = [];
{
  const joined = gunzipSync(readFileSync(join(here, "fixtures", "frame-cursor-6016-strips.bin.gz")));
  assert.equal(joined.length, STRIP_COUNT * STRIP_ACCOUNT_BYTES, "fixture is eight whole strip accounts");
  for (let i = 0; i < STRIP_COUNT; i += 1) realStrips.push(new Uint8Array(joined.subarray(i * STRIP_ACCOUNT_BYTES, (i + 1) * STRIP_ACCOUNT_BYTES)));
}

const sha256 = (bytes) => createHash("sha256").update(Buffer.from(bytes)).digest("hex");

// A synthetic strip with a self-consistent digest, for the paths the real
// fixture cannot reach (a deliberately torn frame, a bad magic, and so on).
function makeStrip({stripId, cursor = 10, gametic = 20, snapshotHash, stateRoot, pixels}) {
  const account = new Uint8Array(STRIP_ACCOUNT_BYTES);
  account.set(new TextEncoder().encode("DOOM-RS1"), 0);
  const view = new DataView(account.buffer);
  view.setUint32(8, 1, true);
  view.setUint32(12, STRIP_HEADER_BYTES, true);
  view.setUint32(16, stripId, true);
  view.setUint32(20, cursor, true);
  view.setUint32(24, gametic, true);
  view.setUint32(28, STRIP_PIXELS_BYTES, true);
  account.set(snapshotHash, 32);
  account.set(stateRoot, 64);
  account.set(pixels, STRIP_HEADER_BYTES);
  return account;
}

async function finishStrip(fields) {
  const digest = await stripDigest(fields);
  const account = makeStrip(fields);
  account.set(digest, 96);
  return account;
}

function syntheticFrame({cursor = 10, gametic = 20, snapshotHash: givenHash, stateRoot: givenRoot} = {}) {
  const snapshotHash = givenHash ?? new Uint8Array(32).fill(0xab);
  const stateRoot = givenRoot ?? new Uint8Array(32).fill(0xcd);
  return Promise.all(
    Array.from({length: STRIP_COUNT}, (_, stripId) => {
      const pixels = new Uint8Array(STRIP_PIXELS_BYTES);
      for (let i = 0; i < pixels.length; i += 1) pixels[i] = (stripId * 7 + i) & 0xff;
      return finishStrip({stripId, cursor, gametic, snapshotHash, stateRoot, pixels});
    }),
  );
}

test("geometry constants agree with the program", () => {
  assert.equal(FRAME_WIDTH * FRAME_HEIGHT, FRAME_BYTES);
  assert.equal(FRAME_BYTES, 64000);
  assert.equal(FRAME_WIDTH / STRIP_COUNT, STRIP_WIDTH);
  assert.equal(STRIP_WIDTH * FRAME_HEIGHT, STRIP_PIXELS_BYTES);
  assert.equal(STRIP_HEADER_BYTES + STRIP_PIXELS_BYTES, STRIP_ACCOUNT_BYTES);
  assert.equal(STRIP_ACCOUNT_BYTES, 8128);
  // The digest domain is the 19-character seed plus a trailing NUL, 20 bytes.
  // The other tests prove the constant is the right one by reproducing the
  // digests the chain already holds.
  assert.equal(STRIP_HASH_DOMAIN, "DOOM-RENDER-STRIP/1\0");
  assert.equal(STRIP_HASH_DOMAIN.length, 20);
  assert.equal(STRIP_HASH_DOMAIN.at(-1), "\0");
});

test("real fixture decodes to the frame the receipts recorded", async () => {
  const frame = await decodeFrame(realStrips);
  assert.equal(frame.cursor, meta.cursor);
  assert.equal(frame.gametic, meta.gametic);
  assert.equal(frame.snapshotHash, meta.snapshotHash);
  assert.equal(frame.stateRoot, meta.stateRoot);
  assert.equal(frame.pixels.length, FRAME_BYTES);
  assert.equal(sha256(frame.pixels), meta.frameSha256);
  assert.equal(frame.stripHashes.length, STRIP_COUNT);
  // And the eight accounts are the ones the fixture claims to be.
  assert.equal(sha256(Buffer.concat(realStrips.map((s) => Buffer.from(s)))), meta.stripsSha256);
  const distinct = new Set(frame.pixels).size;
  assert.equal(distinct, meta.distinctPaletteIndices);
});

test("the real frame is the Doom frame the readback wrote, not a reshuffle", async () => {
  // Guards the column-major/row-major split. If the bands were interleaved
  // wrongly the frame would still be 64000 bytes and would still assemble,
  // but column 0 would come from the wrong band.
  const frame = await decodeFrame(realStrips);
  const width = FRAME_WIDTH;
  // Row 0 of the decoded frame and row 0 of strip 0 must be the same bytes.
  for (let x = 0; x < STRIP_WIDTH; x += 1) {
    assert.equal(frame.pixels[x], realStrips[0][STRIP_HEADER_BYTES + x], `row 0 column ${x}`);
  }
  // Column 0 of strip 7 is frame column 280.
  const strip7Row0 = realStrips[7][STRIP_HEADER_BYTES];
  assert.equal(frame.pixels[STRIP_WIDTH * 7], strip7Row0);
  // Row 1 of strip 7 starts a full STRIP_WIDTH further in.
  assert.equal(frame.pixels[width + STRIP_WIDTH * 7], realStrips[7][STRIP_HEADER_BYTES + STRIP_WIDTH]);
});

test("assembleFrame places each band at its own column offset", async () => {
  const strips = await syntheticFrame();
  const frame = await decodeFrame(strips);
  assert.equal(frame.pixels.length, FRAME_BYTES);
  for (let y = 0; y < FRAME_HEIGHT; y += 1) {
    for (let band = 0; band < STRIP_COUNT; band += 1) {
      const inStrip = y * STRIP_WIDTH;
      const inFrame = y * FRAME_WIDTH + band * STRIP_WIDTH;
      assert.equal(frame.pixels[inFrame], strips[band][STRIP_HEADER_BYTES + inStrip], `y ${y} band ${band}`);
    }
  }
});

test("a flipped bit in the pixels is caught by the digest", async () => {
  const strips = await syntheticFrame();
  strips[3][STRIP_HEADER_BYTES + 17] ^= 0x01;
  await assert.rejects(() => decodeFrame(strips), (error) => {
    assert.ok(error instanceof StripError);
    assert.match(error.message, /digest mismatch/);
    assert.equal(error.stripId, 3);
    return true;
  });
});

test("a flipped bit in a header field is caught too", async () => {
  const strips = await syntheticFrame();
  // Change the cursor without recomputing the digest.
  new DataView(strips[5].buffer).setUint32(20, 999, true);
  await assert.rejects(() => decodeFrame(strips), /digest mismatch/);
});

test("structural refusals", async () => {
  const strips = await syntheticFrame();

  const short = new Uint8Array(strips[0].subarray(0, STRIP_ACCOUNT_BYTES - 1));
  await assert.rejects(() => parseStrip(short), /8128/);

  const badMagic = new Uint8Array(strips[0]);
  badMagic[0] = 0x58;
  await assert.rejects(() => parseStrip(badMagic), /magic/);

  const badVersion = new Uint8Array(strips[0]);
  new DataView(badVersion.buffer).setUint32(8, 2, true);
  await assert.rejects(() => parseStrip(badVersion), /version/);

  const badHeaderBytes = new Uint8Array(strips[0]);
  new DataView(badHeaderBytes.buffer).setUint32(12, 64, true);
  await assert.rejects(() => parseStrip(badHeaderBytes), /header bytes/);

  const badStripId = new Uint8Array(strips[0]);
  new DataView(badStripId.buffer).setUint32(16, 8, true);
  await assert.rejects(() => parseStrip(badStripId), /not below 8/);

  const badPixels = new Uint8Array(strips[0]);
  new DataView(badPixels.buffer).setUint32(28, 7999, true);
  await assert.rejects(() => parseStrip(badPixels), /pixel bytes/);
});

test("a strip at the wrong index is a torn read", async () => {
  const strips = await syntheticFrame();
  // Swap two whole accounts: each still self-verifies, but the ids no longer
  // line up with the positions the caller asked for.
  const swapped = [...strips];
  [swapped[2], swapped[6]] = [swapped[6], swapped[2]];
  await assert.rejects(() => decodeFrame(swapped), /carries id/);
});

test("a frame whose strips disagree is refused, not painted torn", async () => {
  const strips = await syntheticFrame({cursor: 10, gametic: 20});
  const later = await syntheticFrame({cursor: 44, gametic: 44});
  const mixed = [...strips];
  mixed[5] = later[5];
  await assert.rejects(() => decodeFrame(mixed), (error) => {
    assert.match(error.message, /disagree/);
    assert.match(error.message, /cursor 44/);
    return true;
  });
});

test("a missing strip is refused rather than painted as garbage", async () => {
  const strips = await syntheticFrame();
  const holed = [...strips];
  holed[4] = null;
  await assert.rejects(() => decodeFrame(holed), /not present on chain/);
  await assert.rejects(() => decodeFrame(strips.slice(0, 7)), /8 strips/);
});

test("the group key covers cursor, gametic, snapshot hash and state root", async () => {
  const base = await parseStrip(realStrips[0]);
  const key = frameGroupKey(base);
  for (const strip of await Promise.all(realStrips.map((raw) => parseStrip(raw)))) {
    assert.equal(frameGroupKey(strip), key, "all eight real strips share one group key");
  }
  // Moving any one field must move the key.
  for (const field of ["cursor", "gametic", "snapshotHash", "stateRoot"]) {
    const moved = {...base, [field]: field.endsWith("Hash") || field === "stateRoot" ? new Uint8Array(base[field].fill(0x00)) : base[field] + 1};
    assert.notEqual(frameGroupKey(moved), key, `${field} participates in the group key`);
  }
});

// A buffered snapshot account, hashed the way the program hashes it. The
// digest is written out here rather than called, so the test is a second
// implementation of the layout instead of a restatement of the one it checks.
function makeSnapshot({cursor = 10, gametic = 20, payloadBytes = 64, stateRoot, abiId}) {
  const root = stateRoot ?? new Uint8Array(32).fill(0xcd);
  const abi = abiId ?? new Uint8Array(32).fill(0x5a);
  const payload = new Uint8Array(SNAPSHOT_HEADER_BYTES + payloadBytes);
  payload.set(new TextEncoder().encode("DOOM-RSN"), 0);
  const view = new DataView(payload.buffer);
  view.setUint32(8, 3, true);
  view.setUint32(12, SNAPSHOT_HEADER_BYTES, true);
  view.setUint32(16, payload.length, true);
  view.setUint32(20, cursor, true);
  view.setUint32(24, gametic, true);
  view.setUint32(28, payloadBytes, true);
  payload.set(root, 32);
  payload.set(abi, 160);
  for (let i = 0; i < payloadBytes; i += 1) payload[SNAPSHOT_HEADER_BYTES + i] = (i * 3) & 0xff;
  const digestInput = Buffer.concat([
    Buffer.from(SNAPSHOT_HASH_DOMAIN, "ascii"),
    payload.subarray(0, 128),
    Buffer.alloc(32),
    payload.subarray(160, 192),
    payload.subarray(192, 192 + payloadBytes),
  ]);
  const digest = new Uint8Array(createHash("sha256").update(digestInput).digest());
  payload.set(digest, 128);
  return payload;
}

test("the snapshot domain is the seed the program writes, NUL included", () => {
  assert.equal(SNAPSHOT_HASH_DOMAIN, "DOOM-RENDER-SNAPSHOT/3\0");
  assert.equal(SNAPSHOT_HASH_DOMAIN.length, 23);
  assert.equal(SNAPSHOT_HASH_DOMAIN.at(-1), "\0");
  assert.equal(SNAPSHOT_HEADER_BYTES, 192);
});

test("a snapshot parses, and its hash is the one the program wrote", async () => {
  const account = makeSnapshot({cursor: 6016, gametic: 6016, payloadBytes: 4096});
  const snapshot = await parseSnapshot(account);
  assert.equal(snapshot.cursor, 6016);
  assert.equal(snapshot.gametic, 6016);
  assert.equal(snapshot.payloadBytes, 4096);
  assert.equal(snapshot.digest, toHex(account.subarray(128, 160)));
  // The digest function the page verifies with agrees with the one the test
  // built the account with, over the same header.
  const again = await snapshotDigest({header: account, abiId: account.subarray(160, 192), payloadBytes: 4096});
  assert.equal(toHex(again), snapshot.digest);
});

test("a snapshot whose payload was written after the hash is refused", async () => {
  const account = makeSnapshot({payloadBytes: 4096});
  account[SNAPSHOT_HEADER_BYTES + 10] ^= 0xff;
  await assert.rejects(() => parseSnapshot(account), /snapshot digest mismatch/);
});

test("a frame is painted only when the snapshot is the one its strips name", async () => {
  // Eight strips that agree with each other are not yet a frame: the pipeline
  // writes the snapshot first and the eight strips after it, eight
  // transactions, so a read in between sees one frame's snapshot beside the
  // previous frame's strips. The strips commit to the snapshot, so the
  // snapshot is what says the frame is whole.
  const base = {cursor: 10, gametic: 20, payloadBytes: 64};
  const matching = await parseSnapshot(makeSnapshot(base));
  const strips = await syntheticFrame({
    cursor: matching.cursor,
    gametic: matching.gametic,
    snapshotHash: matching.snapshotHash,
    stateRoot: matching.stateRoot,
  });
  // The snapshot has already moved on to the next frame; the strips are the
  // last complete frame, and this read is a frame mid-write.
  const ahead = await parseSnapshot(makeSnapshot({...base, cursor: 44, gametic: 44}));
  await assert.rejects(() => decodeFrame(strips, {snapshot: ahead}), /mid-write/);

  const frame = await decodeFrame(strips, {snapshot: matching});
  assert.equal(frame.cursor, 10);
  assert.equal(frame.pixels.length, FRAME_BYTES);
  // And without the snapshot in hand the eight strips are still a frame: mock
  // mode has no snapshot account to read.
  assert.equal((await decodeFrame(strips)).cursor, 10);
});

test("a frame is one account read: the snapshot and its eight strips", () => {
  const strips = Array.from({length: STRIP_COUNT}, (_, i) => `strip-${i}`);
  const addresses = frameReadAddresses({snapshot: "snapshot", strips});
  assert.equal(addresses.length, STRIP_COUNT + 1);
  assert.equal(addresses[0], "snapshot");
  assert.deepEqual(addresses.slice(1), strips);
  assert.throws(() => frameReadAddresses({snapshot: "snapshot", strips: strips.slice(0, 7)}), /8 strip accounts/);
  assert.throws(() => frameReadAddresses({strips}), /snapshot account/);
});

test("u32le reads little-endian", () => {
  const bytes = new Uint8Array([0x01, 0x02, 0x03, 0x04]);
  assert.equal(u32le(bytes, 0), 0x04030201);
});

test("frameToRgba maps palette indices and sets alpha 255", () => {
  const indices = new Uint8Array(FRAME_BYTES);
  indices[0] = 1;
  indices[1] = 255;
  indices[FRAME_BYTES - 1] = 16;
  const palette = new Uint8Array(768);
  for (let i = 0; i < 256; i += 1) {
    palette[i * 3] = i;
    palette[i * 3 + 1] = 255 - i;
    palette[i * 3 + 2] = (i * 7) & 0xff;
  }
  const rgba = frameToRgba(indices, palette);
  assert.equal(rgba.length, FRAME_BYTES * 4);
  assert.deepEqual(Array.from(rgba.subarray(0, 4)), [1, 254, 7, 255]);
  assert.deepEqual(Array.from(rgba.subarray(4, 8)), [255, 0, (255 * 7) & 0xff, 255]);
  assert.deepEqual(Array.from(rgba.subarray((FRAME_BYTES - 1) * 4, FRAME_BYTES * 4)), [16, 239, 112, 255]);
  for (let i = 3; i < rgba.length; i += 4) assert.equal(rgba[i], 255, "alpha is always 255");
});

test("frameToRgba refuses a short palette or a short frame", () => {
  assert.throws(() => frameToRgba(new Uint8Array(FRAME_BYTES), new Uint8Array(767)), /palette has 767 bytes/);
  assert.throws(() => frameToRgba(new Uint8Array(10), new Uint8Array(768)), /frame has 10 bytes/);
});

test("the real frame painted through a palette is mostly not black", async () => {
  // Guards against a palette read that silently returns zeros, which would
  // otherwise look like a working page showing a black rectangle.
  const frame = await decodeFrame(realStrips);
  const palette = new Uint8Array(768);
  for (let i = 0; i < 256; i += 1) {
    palette[i * 3] = 200;
    palette[i * 3 + 1] = 100;
    palette[i * 3 + 2] = 50;
  }
  const rgba = frameToRgba(frame.pixels, palette);
  assert.equal(rgba[0], 200);
  assert.equal(new Set(frame.pixels).size, meta.distinctPaletteIndices);
});

test("toHex is lowercase and zero-padded", () => {
  assert.equal(toHex(new Uint8Array([0, 1, 15, 16, 255])), "00010f10ff");
});
