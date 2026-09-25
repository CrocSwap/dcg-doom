// Read-only JSON-RPC client for the strip accounts.
//
// The page never signs or sends a transaction. It only reads, at the paint
// commitment level from the network config. Mainnet is refused unless the
// caller passes allowMainnet, so a half-filled mainnet entry cannot be
// exercised by accident.

import {isMainnet, requireNetwork} from "./network.js";

const BASE58 = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

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
  constructor({network, allowMainnet = false, fetchImpl = null} = {}) {
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
  }

  get paintCommitment() {
    return this.commitment;
  }

  async call(method, params) {
    this.requestId += 1;
    const body = JSON.stringify({jsonrpc: "2.0", id: this.requestId, method, params});
    let lastError = null;
    for (const endpoint of [this.endpoint, ...this.endpoints.filter((value) => value !== this.endpoint)]) {
      try {
        const response = await this.fetch(endpoint, {
          method: "POST",
          headers: {"content-type": "application/json"},
          body,
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
        lastError = error;
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
  async getAccounts(addresses, {dataSlice = null} = {}) {
    const config = {encoding: "base64"};
    if (dataSlice) config.dataSlice = dataSlice;
    const result = await this.call("getMultipleAccounts", [addresses, config]);
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
}
