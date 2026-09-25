// Live cross-check: decode the real R8i strips from the configured network and
// compare against the readback receipts in the main checkout.
//
// This is NOT part of the offline suite. It reads testnet. Run it by hand:
//   node web/doom-play/test/test_strip_live.mjs
//
// The receipts it compares against were produced by
// scripts/doom_r8i_readback.py and scripts/doom_r8b_tokyo_pipeline.py and live
// at out/runs/doom-r8i-20260925/ in the main checkout. The path is a CLI
// argument so this file carries no machine-specific path.

import {readFileSync, existsSync} from "node:fs";
import {join} from "node:path";
import {createHash} from "node:crypto";
import assert from "node:assert/strict";

import {DEFAULT_NETWORK, NETWORKS} from "../network.js";
import {ChainReader} from "../rpc.js";
import {decodeFrame, toHex, FRAME_BYTES} from "../strip.js";

const receipts = process.argv[2];
if (!receipts) {
  console.error("usage: node test_strip_live.mjs <out/runs/doom-r8i-20260925> [network]");
  process.exit(2);
}
const net = NETWORKS[process.argv[3] ?? DEFAULT_NETWORK];

const reader = new ChainReader({network: net.name});
const doom = net.doom;
const buffer = Number(process.argv[4] ?? 0);
const addresses = doom.strips[buffer];

const {slot, payloads} = await reader.getAccounts(addresses);
const frame = await decodeFrame(payloads);

const sha = (bytes) => createHash("sha256").update(Buffer.from(bytes)).digest("hex");
const frameSha = sha(frame.pixels);

console.log(`network           ${net.name} (${net.label})`);
console.log(`rpc slot          ${slot}  commitment ${reader.paintCommitment}`);
console.log(`context           ${doom.context}`);
console.log(`buffer            ${buffer}  snapshot ${doom.snapshots[buffer]}`);
console.log(`frame cursor      ${frame.cursor}  gametic ${frame.gametic}`);
console.log(`snapshot hash     ${frame.snapshotHash}`);
console.log(`state root        ${frame.stateRoot}`);
console.log(`frame bytes       ${frame.pixels.length} (want ${FRAME_BYTES})`);
console.log(`frame sha256      ${frameSha}`);
console.log(`explorer          ${net.explorer.address.replace("{address}", doom.snapshots[buffer])}`);

assert.equal(frame.pixels.length, FRAME_BYTES, "assembled frame is 320x200");
assert.equal(frame.stripHashes.length, 8);
assert.ok(frame.stripHashes.every(Boolean), "all eight strips verified their digest");

// Compare with the receipts, if they are on disk.
const framesPath = join(receipts, "s5-strip64", "frames.jsonl");
if (existsSync(framesPath)) {
  const records = readFileSync(framesPath, "utf8").trim().split("\n").map((line) => JSON.parse(line));
  const match = records.find((record) => record.cursor === frame.cursor && record.gametic === frame.gametic);
  assert.ok(match, `no receipt frame at cursor ${frame.cursor}`);
  assert.equal(match.frame_sha256, frameSha, "assembled frame matches the readback frame sha256");
  assert.equal(match.snapshot_hash, frame.snapshotHash, "snapshot hash matches the readback");
  assert.equal(match.state_root, frame.stateRoot, "state root matches the readback");
  for (let i = 0; i < 8; i += 1) {
    assert.equal(match.strips[i].pixels_sha256, sha(Buffer.from(payloads[i]).subarray(128)), `strip ${i} pixels match`);
  }
  console.log(`\nMATCH             receipts agree on frame sha256, snapshot hash, state root and all 8 strip pixel hashes`);

  // And against the readback's own raw framebuffer file, byte for byte.
  const fbPath = join(receipts, "s5-strip64", match.frame_path);
  if (existsSync(fbPath)) {
    const raw = readFileSync(fbPath);
    assert.equal(raw.length, FRAME_BYTES, "readback framebuffer is 320x200");
    assert.ok(Buffer.from(frame.pixels).equals(raw), "our decode is byte-identical to the readback framebuffer");
    console.log(`MATCH             byte-identical to ${match.frame_path} (${raw.length} bytes)`);
  } else {
    console.log(`SKIP              ${fbPath} is not on disk`);
  }
} else {
  console.log(`SKIP              ${framesPath} is not on disk`);
}

// The palette, straight out of the WAD accounts.
const {WadImageReader, readPlaypal} = await import("../rpc.js");
const wad = new WadImageReader({
  readSlice: (address, offset, length) => reader.getAccount(address, {offset, length}),
  accounts: doom.wadAccounts,
  accountBytes: doom.wadAccountBytes,
  headerBytes: doom.wadHeaderBytes,
  imageBytes: doom.wadImageBytes,
});
const playpal = await readPlaypal(wad);
console.log(`\nPLAYPAL           ${playpal.length} bytes, palette 0 entry 0 = rgb(${playpal[0]}, ${playpal[1]}, ${playpal[2]})`);

const {frameToRgba} = await import("../strip.js");
const rgba = frameToRgba(frame.pixels, playpal);
console.log(`RGBA              ${rgba.length} bytes (${rgba.length / 4} pixels)`);

const distinct = new Set(frame.pixels).size;
console.log(`distinct indices  ${distinct} of 256 palette entries used`);
console.log(`\nOK`);
