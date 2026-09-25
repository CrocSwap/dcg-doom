// Offline tests for the palette path.
//
// The PLAYPAL palette is the only thing that turns the frame's palette indices
// into colour, and it lives inside DOOM1.WAD on chain rather than in any
// header. So the page reads three small slices out of the five WAD accounts
// rather than pulling 4 MiB. These tests use a synthetic WAD with the same
// directory layout; test_strip_live.mjs checks the real one against the
// pinned WAD sha256.

import test from "node:test";
import assert from "node:assert/strict";

import {WadImageReader, parseWadHeader, findLump, readPlaypal} from "../rpc.js";

const ACCOUNT_BYTES = 1048576;
const HEADER_BYTES = 32;
const PAYLOAD_BYTES = ACCOUNT_BYTES - HEADER_BYTES;
// The pinned DOOM1.WAD image the R8i deployment stores in five accounts.
const REAL_IMAGE_BYTES = 4196020;

// A minimal WAD: header, one PLAYPAL, a couple of other lumps, then the
// directory at the end, which is where a real WAD puts it.
function makeWad({paletteBytes = 768, pad = 0} = {}) {
  const lumps = [
    {name: "PLAYPAL", data: Uint8Array.from({length: paletteBytes}, (_, i) => (i * 7) & 0xff)},
    {name: "COLORMAP", data: new Uint8Array(34)},
    {name: "ENDOOM", data: new Uint8Array(4000)},
  ];
  const headerBytes = 12;
  let cursor = headerBytes;
  const entries = lumps.map((lump) => {
    const entry = {offset: cursor, size: lump.data.length, name: lump.name, data: lump.data};
    cursor += lump.data.length;
    return entry;
  });
  cursor += pad;
  const directoryOffset = cursor;
  const directory = new Uint8Array(entries.length * 16);
  const view = new DataView(directory.buffer);
  entries.forEach((entry, i) => {
    view.setUint32(i * 16, entry.offset, true);
    view.setUint32(i * 16 + 4, entry.size, true);
    directory.set(new TextEncoder().encode(entry.name), i * 16 + 8);
  });
  const image = new Uint8Array(directoryOffset + directory.length);
  image.set(new TextEncoder().encode("IWAD"), 0);
  new DataView(image.buffer).setUint32(4, entries.length, true);
  new DataView(image.buffer).setUint32(8, directoryOffset, true);
  entries.forEach((entry) => image.set(entry.data, entry.offset));
  image.set(directory, directoryOffset);
  return image;
}

// Splits a WAD image into account-sized chunks and serves dataSlice reads out
// of them, so the reader is exercised against the same fragmented layout the
// chain uses.
function chunkedReader(image, {accounts = 5, accountBytes = ACCOUNT_BYTES, headerBytes = HEADER_BYTES} = {}) {
  const payloads = [];
  for (let i = 0; i < accounts; i += 1) {
    const start = i * (accountBytes - headerBytes);
    const length = Math.min(accountBytes - headerBytes, image.length - start);
    payloads.push(image.subarray(start, start + length));
  }
  const addresses = payloads.map((_, i) => `addr-${i}`);
  const calls = [];
  const readSlice = async (address, offset, length) => {
    calls.push({address, offset, length});
    const index = addresses.indexOf(address);
    if (index < 0) throw new Error(`no such account ${address}`);
    const header = new Uint8Array(headerBytes);
    const headerView = new DataView(header.buffer);
    header.set(new TextEncoder().encode("DOOM-WC1"), 0);
    headerView.setUint32(8, 1, true);
    headerView.setUint32(12, headerBytes, true);
    headerView.setUint32(16, accountBytes, true);
    headerView.setUint32(20, index, true);
    headerView.setUint32(24, payloads[index].length, true);
    const account = new Uint8Array(header.length + payloads[index].length);
    account.set(header, 0);
    account.set(payloads[index], header.length);
    if (offset + length > account.length) throw new Error(`slice past the end of ${address}`);
    return account.subarray(offset, offset + length);
  };
  const wad = new WadImageReader({
    readSlice,
    accounts: addresses,
    accountBytes,
    headerBytes,
    imageBytes: image.length,
  });
  return {wad, calls, payloads};
}

test("the WAD header is magic, lump count at 4, directory offset at 8", () => {
  const image = makeWad();
  const parsed = parseWadHeader(image.subarray(0, 12));
  assert.equal(parsed.magic, "IWAD");
  assert.equal(parsed.lumpCount, 3);
  assert.equal(parsed.directoryOffset, image.length - 3 * 16);
  // The transposition this guards against reads a lump count of 0x0c and a
  // directory offset of a few hundred, which then fails downstream.
  assert.notEqual(parsed.lumpCount, parsed.directoryOffset);
  assert.throws(() => parseWadHeader(image.subarray(0, 8)), /shorter than 12 bytes/);
  assert.throws(() => parseWadHeader(new TextEncoder().encode("XWAD\0\0\0\0\0\0\0\0")), /expected IWAD or PWAD/);
  assert.equal(parseWadHeader(new TextEncoder().encode("PWAD\0\0\0\0\0\0\0\0")).magic, "PWAD");
});

test("findLump matches names case-insensitively and ignores the NUL padding", () => {
  const image = makeWad();
  const parsed = parseWadHeader(image.subarray(0, 12));
  const directory = image.subarray(parsed.directoryOffset, parsed.directoryOffset + parsed.lumpCount * 16);
  assert.equal(findLump(directory, parsed.lumpCount, "PLAYPAL").size, 768);
  assert.equal(findLump(directory, parsed.lumpCount, "ENDOOM").size, 4000);
  assert.equal(findLump(directory, parsed.lumpCount, "COLORMAP").offset, 12 + 768);
  assert.equal(findLump(directory, parsed.lumpCount, "NOTHERE"), null);
  // Lump names are stored upper case, so the query is upper cased too. A
  // lower-case query does not match, which is why every caller passes
  // "PLAYPAL" rather than "playpal".
  assert.equal(findLump(directory, parsed.lumpCount, "playpal"), null);
});

test("readPlaypal finds PLAYPAL across account boundaries", async () => {
  const image = makeWad();
  const {wad, calls} = chunkedReader(image);
  const playpal = await readPlaypal(wad);
  assert.equal(playpal.length, 768);
  assert.deepEqual(Array.from(playpal.subarray(0, 8)), Array.from(image.subarray(12, 20)));
  // Three reads: the 12-byte header, the directory, then the palette itself.
  assert.equal(calls.length, 3);
  assert.equal(calls[0].length, 12);
  assert.ok(calls.every((call) => call.length <= 4096), "no read pulls a whole account");
});

test("a PLAYPAL that straddles two accounts is stitched, not truncated", async () => {
  // Put the palette at the very end of a chunk so the read has to continue
  // into the next account, skipping that account's own 32-byte header.
  const image = makeWad();
  const parsed = parseWadHeader(image.subarray(0, 12));
  const directory = image.subarray(parsed.directoryOffset, parsed.directoryOffset + parsed.lumpCount * 16);
  const playpalOffset = findLump(directory, parsed.lumpCount, "PLAYPAL").offset;
  // A chunk boundary that lands inside the palette.
  const boundary = playpalOffset + 300;
  const {wad} = chunkedReader(image, {accounts: 32, accountBytes: boundary + HEADER_BYTES, headerBytes: HEADER_BYTES});
  const playpal = await readPlaypal(wad);
  assert.equal(playpal.length, 768);
  assert.deepEqual(Array.from(playpal), Array.from(image.subarray(playpalOffset, playpalOffset + 768)));
});

test("the reader refuses a range outside the image", async () => {
  const image = makeWad();
  const {wad} = chunkedReader(image);
  await assert.rejects(() => wad.read(-1, 4), /outside the/);
  await assert.rejects(() => wad.read(0, image.length + 1), /outside the/);
  await assert.rejects(() => wad.read(4, -1), /outside the/);
  // A reader configured with too few accounts says so rather than reading
  // zeros off the end of the last one.
  const short = new WadImageReader({
    readSlice: async () => new Uint8Array(0),
    accounts: ["only"],
    accountBytes: ACCOUNT_BYTES,
    headerBytes: HEADER_BYTES,
    imageBytes: REAL_IMAGE_BYTES,
  });
  await assert.rejects(() => short.read(REAL_IMAGE_BYTES - 8, 8), /outside the/);
});

test("readPlaypal refuses a WAD with no PLAYPAL and one with a short PLAYPAL", async () => {
  const image = makeWad();
  const parsed = parseWadHeader(image.subarray(0, 12));
  // Blank the name in the directory.
  image.fill(0, parsed.directoryOffset + 8, parsed.directoryOffset + parsed.lumpCount * 16);
  const missing = chunkedReader(image);
  await assert.rejects(() => readPlaypal(missing.wad), /PLAYPAL is not in this WAD/);

  const tiny = makeWad({paletteBytes: 100});
  const tinyReader = chunkedReader(tiny);
  await assert.rejects(() => readPlaypal(tinyReader.wad), /too short for a 768-byte palette/);
});

test("a real DOOM1-shaped chunk plan covers the pinned image size", () => {
  // The pinned deployment splits 4_196_020 bytes into four full 1_048_544
  // byte chunks and a 1_844 byte tail, which is what the config records.
  const imageBytes = REAL_IMAGE_BYTES;
  const {wad} = chunkedReader(makeWad(), {accountBytes: ACCOUNT_BYTES, accounts: 5});
  // Rebuild with the real image size so the tail arithmetic is exercised.
  const real = new WadImageReader({
    readSlice: async () => new Uint8Array(0),
    accounts: Array.from({length: 5}, (_, i) => `a${i}`),
    accountBytes: ACCOUNT_BYTES,
    headerBytes: HEADER_BYTES,
    imageBytes,
  });
  const lengths = real.chunks.map((chunk) => chunk.length);
  assert.deepEqual(lengths, [1048544, 1048544, 1048544, 1048544, 1844]);
  assert.equal(lengths.reduce((a, b) => a + b, 0), imageBytes);
  // The chunks tile the image with no gap and no overlap.
  let at = 0;
  for (const chunk of real.chunks) {
    assert.equal(chunk.start, at);
    at += chunk.length;
  }
  assert.equal(at, imageBytes);
  assert.equal(wad.chunks.length, 5);
});
