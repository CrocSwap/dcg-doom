// The signed input message: the one contract between the browser and the
// relayer.
//
// The page signs this with a throwaway ed25519 key made in the tab and kept
// in localStorage. The relayer verifies it, then maps `commands` onto the
// on-chain input stream. Nothing here is consensus-sensitive: the on-chain
// stream is SIM/2 DEPOSIT, which takes a first-sequence number and up to 64
// four-byte commands, and this message carries exactly that plus enough
// binding to stop a signature being replayed onto another slot.
//
// This module is imported by both web/doom-play/app.js and
// stunts/doom-relayer, so the browser and the relayer cannot drift.

export const INPUT_MAGIC = "DOOM-IN1";
export const INPUT_VERSION = 1;
export const INPUT_SIGN_DOMAIN = "DOOM-INPUT/1\0";
export const INPUT_HEADER_BYTES = 64;
export const COMMAND_BYTES = 4;
export const MAX_COMMANDS_PER_BATCH = 64; // SIM/2 MAX_DEPOSIT_SLOTS
export const MAX_INPUT_BYTES = INPUT_HEADER_BYTES + MAX_COMMANDS_PER_BATCH * COMMAND_BYTES; // 320

// The program refuses any command whose buttons byte has bit 0x80 set
// (DOOM_SIM2_BT_SPECIAL, stunts/doom-sbf/src/doom_sim2_guard.h:33). We
// refuse it here too so a rejected batch never costs a round trip.
export const BT_SPECIAL = 0x80;

// Vanilla Doom button bits. The program only enforces the 0x80 rule; the rest
// are the engine's own BT_* values.
export const BT_ATTACK = 0x01;
export const BT_USE = 0x02;
export const BT_SPEED = 0x04;

// Field offsets in the 64-byte header.
export const OFF_MAGIC = 0; // 8 bytes
export const OFF_VERSION = 8; // u16
export const OFF_COMMAND_COUNT = 10; // u16
export const OFF_SLOT = 12; // u32
export const OFF_FIRST_SEQ = 16; // u32
export const OFF_CLIENT_SEQ = 20; // u32
export const OFF_ISSUED_AT = 24; // u32, unix seconds
export const OFF_SLOT_BINDING = 28; // 32 bytes
export const OFF_PAYLOAD_BYTES = 60; // u32
export const OFF_COMMANDS = 64;

export class InputError extends Error {
  constructor(message, detail = {}) {
    super(message);
    this.name = "InputError";
    Object.assign(this, detail);
  }
}

// The 32-byte value that ties a signed batch to one chain context. Both sides
// derive it the same way; it is not a secret.
export async function slotBinding({context, meta, inputBuffer}) {
  const parts = [new TextEncoder().encode("DOOM-SLOT/1\0")];
  for (const value of [context, meta, inputBuffer]) {
    parts.push(value ? decodeAddress(value) : new Uint8Array(32));
  }
  let total = 0;
  for (const part of parts) total += part.length;
  const joined = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    joined.set(part, at);
    at += part.length;
  }
  return new Uint8Array(await crypto.subtle.digest("SHA-256", joined));
}

const BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

function decodeAddress(text) {
  let value = 0n;
  for (const char of text) {
    const digit = BASE58.indexOf(char);
    if (digit < 0) throw new InputError(`not a base58 address: ${text}`);
    value = value * 58n + BigInt(digit);
  }
  let hex = value.toString(16);
  if (hex.length % 2) hex = `0${hex}`;
  if (hex.length > 64) hex = hex.slice(hex.length - 64);
  const bytes = new Uint8Array(32);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2).padStart(2, "0"), 16);
  }
  return bytes;
}

// commands: Uint8Array of count * 4 bytes, or an array of [fwd, side, turn, buttons].
export function encodeInput({slot, firstSeq, commands, clientSeq, issuedAt, binding}) {
  const bytes = commands instanceof Uint8Array ? commands : new Uint8Array(commands);
  if (bytes.length % COMMAND_BYTES) throw new InputError(`commands are ${bytes.length} bytes, not a multiple of 4`);
  const count = bytes.length / COMMAND_BYTES;
  if (count < 1 || count > MAX_COMMANDS_PER_BATCH) {
    throw new InputError(`a batch carries 1..${MAX_COMMANDS_PER_BATCH} commands, got ${count}`, {count});
  }
  if (!(binding instanceof Uint8Array) || binding.length !== 32) {
    throw new InputError("slot binding must be 32 bytes");
  }
  const message = new Uint8Array(INPUT_HEADER_BYTES + bytes.length);
  const view = new DataView(message.buffer);
  message.set(new TextEncoder().encode(INPUT_MAGIC), OFF_MAGIC);
  view.setUint16(OFF_VERSION, INPUT_VERSION, true);
  view.setUint16(OFF_COMMAND_COUNT, count, true);
  view.setUint32(OFF_SLOT, slot >>> 0, true);
  view.setUint32(OFF_FIRST_SEQ, firstSeq >>> 0, true);
  view.setUint32(OFF_CLIENT_SEQ, clientSeq >>> 0, true);
  view.setUint32(OFF_ISSUED_AT, issuedAt >>> 0, true);
  message.set(binding, OFF_SLOT_BINDING);
  view.setUint32(OFF_PAYLOAD_BYTES, bytes.length, true);
  message.set(bytes, OFF_COMMANDS);
  return message;
}

export function decodeInput(message) {
  const bytes = message instanceof Uint8Array ? message : new Uint8Array(message);
  if (bytes.length < INPUT_HEADER_BYTES) {
    throw new InputError(`message is ${bytes.length} bytes, shorter than the ${INPUT_HEADER_BYTES}-byte header`);
  }
  const magic = new TextDecoder().decode(bytes.subarray(OFF_MAGIC, OFF_MAGIC + 8));
  if (magic !== INPUT_MAGIC) throw new InputError(`magic is ${JSON.stringify(magic)}`, {magic});
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const version = view.getUint16(OFF_VERSION, true);
  if (version !== INPUT_VERSION) throw new InputError(`version ${version}, expected ${INPUT_VERSION}`, {version});
  const count = view.getUint16(OFF_COMMAND_COUNT, true);
  const payloadBytes = view.getUint32(OFF_PAYLOAD_BYTES, true);
  if (payloadBytes !== count * COMMAND_BYTES) {
    throw new InputError(`payload bytes ${payloadBytes} disagrees with count ${count}`, {count, payloadBytes});
  }
  if (bytes.length !== INPUT_HEADER_BYTES + payloadBytes) {
    throw new InputError(`message is ${bytes.length} bytes, header says ${INPUT_HEADER_BYTES + payloadBytes}`, {
      length: bytes.length,
    });
  }
  if (count < 1 || count > MAX_COMMANDS_PER_BATCH) {
    throw new InputError(`command count ${count} is outside 1..${MAX_COMMANDS_PER_BATCH}`, {count});
  }
  return {
    slot: view.getUint32(OFF_SLOT, true),
    firstSeq: view.getUint32(OFF_FIRST_SEQ, true),
    clientSeq: view.getUint32(OFF_CLIENT_SEQ, true),
    issuedAt: view.getUint32(OFF_ISSUED_AT, true),
    binding: bytes.slice(OFF_SLOT_BINDING, OFF_SLOT_BINDING + 32),
    count,
    commands: bytes.slice(OFF_COMMANDS),
  };
}

// What the burner key actually signs: a domain tag plus the message, so a
// signature over an input batch can never be presented as a signature over
// something else this codebase cares about.
export function signingPayload(message) {
  const tag = new TextEncoder().encode(INPUT_SIGN_DOMAIN);
  const bytes = message instanceof Uint8Array ? message : new Uint8Array(message);
  const out = new Uint8Array(tag.length + bytes.length);
  out.set(tag, 0);
  out.set(bytes, tag.length);
  return out;
}

export async function signInput(privateKey, message) {
  return new Uint8Array(await crypto.subtle.sign({name: "Ed25519"}, privateKey, signingPayload(message)));
}

export async function verifyInput(publicKey, message, signature) {
  try {
    return await crypto.subtle.verify({name: "Ed25519"}, publicKey, signingPayload(message), signature);
  } catch {
    return false;
  }
}

// Rules the relayer enforces, independent of the chain. Returns
// {ok: true} or {ok: false, reason} with a reason the API can hand back.
export function validateInput(input, {expectedSlot, expectedBinding, nowSeconds, maxSkewSeconds = 120, nextSeq = 0, window = 240} = {}) {
  if (expectedSlot !== undefined && input.slot !== expectedSlot) {
    return {ok: false, reason: `batch is for slot ${input.slot}, session is slot ${expectedSlot}`};
  }
  if (expectedBinding && !equalBytes(input.binding, expectedBinding)) {
    return {ok: false, reason: "slot binding does not match this session's context"};
  }
  if (nowSeconds !== undefined && Math.abs(nowSeconds - input.issuedAt) > maxSkewSeconds) {
    return {ok: false, reason: `issued_at is ${input.issuedAt}, more than ${maxSkewSeconds}s from now`};
  }
  if (input.firstSeq + input.count > 0xffffffff) return {ok: false, reason: "sequence range overflows u32"};
  if (input.firstSeq < nextSeq) {
    return {ok: false, reason: `first_seq ${input.firstSeq} is behind the slot's next_seq ${nextSeq}`};
  }
  if (input.firstSeq > nextSeq + window) {
    return {ok: false, reason: `first_seq ${input.firstSeq} is more than ${window} ahead of next_seq ${nextSeq}`};
  }
  for (let i = 0; i < input.count; i += 1) {
    const buttons = input.commands[i * COMMAND_BYTES + 3];
    if (buttons & BT_SPECIAL) {
      return {ok: false, reason: `command ${i} sets BT_SPECIAL (0x80), which the program refuses`};
    }
  }
  return {ok: true};
}

function equalBytes(a, b) {
  if (!a || !b || a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a[i] ^ b[i];
  return diff === 0;
}

// ticcmd packs the engine's four-byte demo command: forwardmove and sidemove
// as signed bytes, angleturn as a signed int16 that the program reads as
// (unsigned char) << 8, and the BT_* button bits. Values are masked into their
// byte rather than thrown on, so a held key can never produce a message the
// relayer would have to special-case.
export const TICCMD_HORIZONTAL = 4;

export function ticcmd({forward = 0, side = 0, turn = 0, buttons = 0} = {}) {
  const wrap = (value) => (((value | 0) & 0xff) + 0x100) & 0xff;
  return [wrap(forward), wrap(side), wrap(Math.round(turn / 256)), buttons & 0xff];
}

// Held keys to a per-tic command, given the tic the batch starts at.
// A keypress is held across several tics, so the page turns a held arrow key
// into a ticcmd stream rather than one command per key event. Vanilla Doom
// accumulates angleturn over four tics (build.rs:1512 keeps the per-tic table
// at 640/1280/320), so a held turn key contributes on every fourth tic; that
// is what makes the on-chain turn rate match a real keyboard.
export function heldToCommands(keys, startTic, count) {
  const out = new Uint8Array(count * COMMAND_BYTES);
  for (let i = 0; i < count; i += 1) {
    const tic = startTic + i;
    const turn = tic % TICCMD_HORIZONTAL === 0 ? (keys.right ? 1 : 0) - (keys.left ? 1 : 0) : 0;
    const command = ticcmd({
      forward: (keys.forward ? 50 : 0) - (keys.back ? 50 : 0),
      side: (keys.strafeRight ? 50 : 0) - (keys.strafeLeft ? 50 : 0),
      // ticcmd takes the engine's angleturn, which is byte << 8, so one
      // direction step is +/-1024 and lands in byte 2 as 4 or 252.
      turn: turn * 1024,
      buttons: (keys.attack ? BT_ATTACK : 0) | (keys.use ? BT_USE : 0) | (keys.run ? BT_SPEED : 0),
    });
    out.set(command, i * COMMAND_BYTES);
  }
  return out;
}
