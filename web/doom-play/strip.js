// Decoder for Doom-on-chain render strip accounts.
//
// Layout is transcribed from stunts/doom-sbf/src/render_snapshot_v3.rs and
// stunts/doom-sbf/src/doom_sbf_resource_ri2.c, and is byte-checked against
// scripts/doom_r8i_readback.py output by web/doom-play/test/test_strip.mjs
// and test/test_strip_live.mjs.
//
// One frame is eight strip accounts. The frame is cut into eight vertical
// bands of 40 columns; within a band the pixels are row-major with the
// origin at the top left. Strip bytes are final PLAYPAL palette indices, not
// texture pairs: lighting and the damage flash are already baked in.

export const FRAME_WIDTH = 320;
export const FRAME_HEIGHT = 200;
export const FRAME_BYTES = FRAME_WIDTH * FRAME_HEIGHT; // 64_000

export const STRIP_COUNT = 8;
export const STRIP_WIDTH = FRAME_WIDTH / STRIP_COUNT; // 40
export const STRIP_HEADER_BYTES = 128;
export const STRIP_PIXELS_BYTES = STRIP_WIDTH * FRAME_HEIGHT; // 8_000
export const STRIP_ACCOUNT_BYTES = STRIP_HEADER_BYTES + STRIP_PIXELS_BYTES; // 8_128

export const STRIP_MAGIC = "DOOM-RS1";
export const STRIP_VERSION = 1;

// The buffered snapshot a frame's eight strips are bound to. Transcribed from
// stunts/doom-sbf/src/render_snapshot_v3.rs and checked field by field against
// scripts/doom_r8b_tokyo_pipeline.py's `validate_buffered_snapshot`, which is
// the cross-implementation reference for this layout.
export const SNAPSHOT_MAGIC = "DOOM-RSN";
export const SNAPSHOT_VERSION = 3;
export const SNAPSHOT_HEADER_BYTES = 192;

// The digest domain is the 22-character ASCII seed with a trailing NUL, 23
// bytes, and it covers the header with the hash field itself zeroed -- the
// program computes the hash it then writes into that field.
export const SNAPSHOT_HASH_DOMAIN = "DOOM-RENDER-SNAPSHOT/3\0";

export const OFF_SNAP_MAGIC = 0; // 8 bytes
export const OFF_SNAP_VERSION = 8; // u32
export const OFF_SNAP_HEADER_BYTES = 12; // u32
export const OFF_SNAP_TOTAL_BYTES = 16; // u32
export const OFF_SNAP_CURSOR = 20; // u32
export const OFF_SNAP_GAMETIC = 24; // u32
export const OFF_SNAP_PAYLOAD_BYTES = 28; // u32
export const OFF_SNAP_STATE_ROOT = 32; // 32 bytes
export const OFF_SNAP_HASH = 128; // 32 bytes
export const OFF_SNAP_ABI_ID = 160; // 32 bytes

// The digest domain is the 19-character ASCII seed with a trailing NUL, 20 bytes.
// Reproduces the strip_hash the program writes, which the tests check against
// real testnet accounts.
export const STRIP_HASH_DOMAIN = "DOOM-RENDER-STRIP/1\0";

// Header field offsets, all integers little-endian.
export const OFF_MAGIC = 0; // 8 bytes
export const OFF_VERSION = 8; // u32
export const OFF_HEADER_BYTES = 12; // u32
export const OFF_STRIP_ID = 16; // u32
export const OFF_CURSOR = 20; // u32, the SIM2 cursor / frame id
export const OFF_GAMETIC = 24; // u32
export const OFF_PIXELS_BYTES = 28; // u32
export const OFF_SNAPSHOT_HASH = 32; // 32 bytes
export const OFF_STATE_ROOT = 64; // 32 bytes
export const OFF_STRIP_HASH = 96; // 32 bytes

export class StripError extends Error {
  constructor(message, detail = {}) {
    super(message);
    this.name = "StripError";
    Object.assign(this, detail);
  }
}

export function hex(bytes) {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

export function u32le(bytes, offset) {
  return (
    (bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16) | (bytes[offset + 3] << 24)) >>> 0
  );
}

export function toHex(bytes) {
  return hex(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
}

// Compares two byte runs without leaking length through an early return.
function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a[i] ^ b[i];
  return diff === 0;
}

export async function sha256(bytes) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
}

// strip_hash = SHA256(DOMAIN || u32le(strip_id) || u32le(cursor) || u32le(gametic)
//                      || snapshot_hash || pixels[0..8000])
export async function stripDigest({stripId, cursor, gametic, snapshotHash, pixels}) {
  const parts = [];
  parts.push(new TextEncoder().encode(STRIP_HASH_DOMAIN));
  const head = new Uint8Array(12);
  new DataView(head.buffer).setUint32(0, stripId >>> 0, true);
  new DataView(head.buffer).setUint32(4, cursor >>> 0, true);
  new DataView(head.buffer).setUint32(8, gametic >>> 0, true);
  parts.push(head);
  parts.push(snapshotHash);
  parts.push(pixels);
  let total = 0;
  for (const part of parts) total += part.length;
  const joined = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    joined.set(part, at);
    at += part.length;
  }
  return sha256(joined);
}

// Structural parse plus digest verification. `expectStripId` is the index the
// caller asked for; a strip that carries a different id is a torn read, not a
// frame. Pass null to accept whatever the account claims.
export async function parseStrip(accountBytes, {expectStripId = null, verifyDigest = true} = {}) {
  if (!(accountBytes instanceof Uint8Array)) accountBytes = new Uint8Array(accountBytes);
  if (accountBytes.length !== STRIP_ACCOUNT_BYTES) {
    throw new StripError(`strip account is ${accountBytes.length} bytes, expected ${STRIP_ACCOUNT_BYTES}`, {
      length: accountBytes.length,
    });
  }
  const magic = new TextDecoder().decode(accountBytes.subarray(OFF_MAGIC, OFF_MAGIC + 8));
  if (magic !== STRIP_MAGIC) {
    throw new StripError(`strip magic is ${JSON.stringify(magic)}, expected ${JSON.stringify(STRIP_MAGIC)}`, {magic});
  }
  const version = u32le(accountBytes, OFF_VERSION);
  if (version !== STRIP_VERSION) throw new StripError(`strip version ${version}, expected ${STRIP_VERSION}`, {version});
  const headerBytes = u32le(accountBytes, OFF_HEADER_BYTES);
  if (headerBytes !== STRIP_HEADER_BYTES) {
    throw new StripError(`strip header bytes ${headerBytes}, expected ${STRIP_HEADER_BYTES}`, {headerBytes});
  }
  const stripId = u32le(accountBytes, OFF_STRIP_ID);
  if (stripId >= STRIP_COUNT) throw new StripError(`strip id ${stripId} is not below ${STRIP_COUNT}`, {stripId});
  if (expectStripId !== null && stripId !== expectStripId) {
    throw new StripError(`strip account at index ${expectStripId} carries id ${stripId}`, {stripId, expectStripId});
  }
  const pixelsBytes = u32le(accountBytes, OFF_PIXELS_BYTES);
  if (pixelsBytes !== STRIP_PIXELS_BYTES) {
    throw new StripError(`strip pixel bytes ${pixelsBytes}, expected ${STRIP_PIXELS_BYTES}`, {pixelsBytes});
  }

  const strip = {
    stripId,
    cursor: u32le(accountBytes, OFF_CURSOR),
    gametic: u32le(accountBytes, OFF_GAMETIC),
    snapshotHash: accountBytes.slice(OFF_SNAPSHOT_HASH, OFF_SNAPSHOT_HASH + 32),
    stateRoot: accountBytes.slice(OFF_STATE_ROOT, OFF_STATE_ROOT + 32),
    declaredHash: accountBytes.slice(OFF_STRIP_HASH, OFF_STRIP_HASH + 32),
    pixels: accountBytes.slice(STRIP_HEADER_BYTES),
  };

  if (verifyDigest) {
    const expected = await stripDigest(strip);
    if (!bytesEqual(expected, strip.declaredHash)) {
      throw new StripError(
        `strip ${stripId} digest mismatch: declared ${toHex(strip.declaredHash).slice(0, 16)}…, ` +
          `computed ${toHex(expected).slice(0, 16)}…`,
        {stripId, declared: toHex(strip.declaredHash), computed: toHex(expected)},
      );
    }
  }
  strip.digest = toHex(strip.declaredHash);
  return strip;
}

// The parent snapshot account does not commit to the strips. Each strip
// commits to the snapshot one way, in its own header. So the viewer groups by
// this key and paints only when all eight agree, which is what stops a
// snapshot landing between two strip reads from producing a torn frame.
export function frameGroupKey(strip) {
  return [strip.cursor, strip.gametic, toHex(strip.snapshotHash), toHex(strip.stateRoot)].join(":");
}

// snapshot_hash = SHA256(DOMAIN || header[0..128] || zeros[32] || header[160..192]
//                      || payload[0..payload_bytes])
//
// The hash field is zeroed in its own input, because it is where the result
// goes. Everything else in the account is covered, so a snapshot that verifies
// is the one the program wrote, and the strips' own copy of that hash is what
// binds them to it.
export async function snapshotDigest({header, abiId, payloadBytes}) {
  const domain = new TextEncoder().encode(SNAPSHOT_HASH_DOMAIN);
  const zeros = new Uint8Array(32);
  const payload = header.subarray(SNAPSHOT_HEADER_BYTES, SNAPSHOT_HEADER_BYTES + payloadBytes);
  let total = domain.length + 128 + 32 + 32 + payload.length;
  const joined = new Uint8Array(total);
  let at = 0;
  joined.set(domain, at);
  at += domain.length;
  joined.set(header.subarray(0, 128), at);
  at += 128;
  joined.set(zeros, at);
  at += 32;
  joined.set(abiId, at);
  at += 32;
  joined.set(payload, at);
  return sha256(joined);
}

// Parse one buffered snapshot account. The hash is verified by default: it is
// what makes a frame provably whole, and a viewer that skipped it would paint
// strips bound to a snapshot nobody checked.
export async function parseSnapshot(accountBytes, {verifyDigest = true} = {}) {
  if (!(accountBytes instanceof Uint8Array)) accountBytes = new Uint8Array(accountBytes);
  if (accountBytes.length < SNAPSHOT_HEADER_BYTES) {
    throw new StripError(`snapshot account is ${accountBytes.length} bytes, shorter than its ${SNAPSHOT_HEADER_BYTES}-byte header`, {
      length: accountBytes.length,
    });
  }
  const magic = new TextDecoder().decode(accountBytes.subarray(OFF_SNAP_MAGIC, OFF_SNAP_MAGIC + 8));
  if (magic !== SNAPSHOT_MAGIC) {
    throw new StripError(`snapshot magic is ${JSON.stringify(magic)}, expected ${JSON.stringify(SNAPSHOT_MAGIC)}`, {magic});
  }
  const version = u32le(accountBytes, OFF_SNAP_VERSION);
  if (version !== SNAPSHOT_VERSION) {
    throw new StripError(`snapshot version ${version}, expected ${SNAPSHOT_VERSION}`, {version});
  }
  const headerBytes = u32le(accountBytes, OFF_SNAP_HEADER_BYTES);
  if (headerBytes !== SNAPSHOT_HEADER_BYTES) {
    throw new StripError(`snapshot header bytes ${headerBytes}, expected ${SNAPSHOT_HEADER_BYTES}`, {headerBytes});
  }
  const payloadBytes = u32le(accountBytes, OFF_SNAP_PAYLOAD_BYTES);
  if (payloadBytes > accountBytes.length - SNAPSHOT_HEADER_BYTES) {
    throw new StripError(`snapshot payload ${payloadBytes} bytes exceeds the ${accountBytes.length - SNAPSHOT_HEADER_BYTES} bytes after its header`, {
      payloadBytes,
    });
  }
  const snapshot = {
    cursor: u32le(accountBytes, OFF_SNAP_CURSOR),
    gametic: u32le(accountBytes, OFF_SNAP_GAMETIC),
    payloadBytes,
    stateRoot: accountBytes.slice(OFF_SNAP_STATE_ROOT, OFF_SNAP_STATE_ROOT + 32),
    snapshotHash: accountBytes.slice(OFF_SNAP_HASH, OFF_SNAP_HASH + 32),
    abiId: accountBytes.slice(OFF_SNAP_ABI_ID, OFF_SNAP_ABI_ID + 32),
  };
  if (verifyDigest) {
    const expected = await snapshotDigest({header: accountBytes, abiId: snapshot.abiId, payloadBytes});
    if (!bytesEqual(expected, snapshot.snapshotHash)) {
      throw new StripError(
        `snapshot digest mismatch: declared ${toHex(snapshot.snapshotHash).slice(0, 16)}…, ` +
          `computed ${toHex(expected).slice(0, 16)}…`,
        {declared: toHex(snapshot.snapshotHash), computed: toHex(expected)},
      );
    }
  }
  snapshot.digest = toHex(snapshot.snapshotHash);
  return snapshot;
}

// pixels[k][y * 40 + x] is frame pixel (x_global = 40 * k + x, y_global = y).
// There is no inter-strip padding and no frame-level header.
export function assembleFrame(strips) {
  if (strips.length !== STRIP_COUNT) {
    throw new StripError(`a frame needs ${STRIP_COUNT} strips, got ${strips.length}`, {count: strips.length});
  }
  const frame = new Uint8Array(FRAME_BYTES);
  for (const strip of strips) {
    if (strip.pixels.length !== STRIP_PIXELS_BYTES) {
      throw new StripError(`strip ${strip.stripId} carries ${strip.pixels.length} pixel bytes`, {stripId: strip.stripId});
    }
    const origin = strip.stripId * STRIP_WIDTH;
    for (let row = 0; row < FRAME_HEIGHT; row += 1) {
      const source = row * STRIP_WIDTH;
      const target = row * FRAME_WIDTH + origin;
      frame.set(strip.pixels.subarray(source, source + STRIP_WIDTH), target);
    }
  }
  return frame;
}

// Takes the eight account payloads in strip order and returns a paintable
// frame, or throws with the reason. Nothing is painted from a partial read:
// the caller should keep showing the last good frame instead.
//
// `snapshot` is the same read's snapshot account, when the caller took one
// (see readFrameAccounts). The strips land in eight transactions, so a read
// that catches a frame in progress sees strips from two frames; requiring the
// snapshot to be the one the strips name is what turns "eight strips that
// happen to agree" into "a frame, with a hash the program wrote".
export async function decodeFrame(accountPayloads, {snapshot = null} = {}) {
  const parsed = [];
  for (let index = 0; index < accountPayloads.length; index += 1) {
    const payload = accountPayloads[index];
    if (!payload) throw new StripError(`strip ${index} is not present on chain yet`, {stripId: index});
    parsed.push(await parseStrip(payload, {expectStripId: index}));
  }
  const keys = new Set(parsed.map(frameGroupKey));
  if (keys.size !== 1) {
    const groups = parsed.map((strip) => `strip ${strip.stripId}: cursor ${strip.cursor}, gametic ${strip.gametic}, snapshot ${toHex(strip.snapshotHash).slice(0, 12)}…`);
    throw new StripError(`strips disagree, refusing to paint a torn frame\n  ${groups.join("\n  ")}`, {groups: [...keys]});
  }
  const first = parsed[0];
  if (snapshot) {
    if (first.cursor !== snapshot.cursor || !bytesEqual(first.snapshotHash, snapshot.snapshotHash)) {
      throw new StripError(
        `strips name frame ${first.cursor} (snapshot ${toHex(first.snapshotHash).slice(0, 12)}…) but the ` +
          `snapshot account holds frame ${snapshot.cursor} (snapshot ${toHex(snapshot.snapshotHash).slice(0, 12)}…); ` +
          `a frame is mid-write`,
        {stripCursor: first.cursor, snapshotCursor: snapshot.cursor},
      );
    }
  }
  const frame = assembleFrame(parsed);
  return {
    cursor: first.cursor,
    gametic: first.gametic,
    snapshotHash: toHex(first.snapshotHash),
    stateRoot: toHex(first.stateRoot),
    stripHashes: parsed.map((strip) => strip.digest),
    pixels: frame,
  };
}

// One account read for a whole frame: the snapshot and its eight strips, in
// that order. `getMultipleAccounts` answers at one slot, so a frame is either
// wholly visible or not visible at all -- reading the strips in two calls is
// what let a snapshot land between them.
export function frameReadAddresses({snapshot, strips}) {
  if (!Array.isArray(strips) || strips.length !== STRIP_COUNT) {
    throw new StripError(`a frame needs ${STRIP_COUNT} strip accounts, got ${strips?.length ?? 0}`, {
      count: strips?.length ?? 0,
    });
  }
  if (!snapshot) throw new StripError("a frame read needs the snapshot account as well as its strips");
  return [snapshot, ...strips];
}

// 320x200 palette indices to 320x200 RGBA. Alpha is 255 everywhere: the frame
// path has no alpha channel. Palette 0 is the right choice because the
// on-chain profile is shareware, where vanilla selects palette 0 and
// I_SetPalette is stubbed out, so nothing on chain can select another.
export const PALETTE_INDEX = 0;
export const PALETTE_ENTRY_BYTES = 3;
export const PALETTE_BYTES = 256 * PALETTE_ENTRY_BYTES; // 768

export function frameToRgba(indices, palette) {
  if (palette.length < PALETTE_BYTES) {
    throw new StripError(`palette has ${palette.length} bytes, need ${PALETTE_BYTES}`, {length: palette.length});
  }
  if (indices.length !== FRAME_BYTES) {
    throw new StripError(`frame has ${indices.length} bytes, need ${FRAME_BYTES}`, {length: indices.length});
  }
  const base = PALETTE_INDEX * PALETTE_BYTES;
  const rgba = new Uint8ClampedArray(FRAME_BYTES * 4);
  for (let i = 0, out = 0; i < FRAME_BYTES; i += 1, out += 4) {
    const entry = base + indices[i] * PALETTE_ENTRY_BYTES;
    rgba[out] = palette[entry];
    rgba[out + 1] = palette[entry + 1];
    rgba[out + 2] = palette[entry + 2];
    rgba[out + 3] = 255;
  }
  return rgba;
}
