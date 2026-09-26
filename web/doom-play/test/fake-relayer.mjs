// A relayer with no chain behind it, for the page tests.
//
// It is the same wire protocol the real relayer speaks -- the same
// `/v1/sessions`, the same status shape, the same server-sent `/frames` stream
// with a four-frame ring and a `since=` cursor -- so the page cannot tell the
// difference, and a fault this fake can produce is a fault the page has.
//
// The frames are the eight real strip accounts from the test fixture with the
// cursor, the gametic and the pixels moved and the strip digest recomputed by
// `stripDigest`, the page's own function. So every frame here passes the same
// eight SHA-256 checks a frame read off the chain passes; the fake cannot hand
// the page a frame the page would not have accepted from the relayer.
//
// The point is that the painting loop can be run for as many frames and as many
// ring turn-overs as a test wants, at no chain cost and in no wall-clock wait.

import {createServer} from "node:http";
import {createHash, createPublicKey, verify as verifySignature} from "node:crypto";
import {gunzipSync} from "node:zlib";
import {readFileSync} from "node:fs";
import {dirname, join, normalize} from "node:path";
import {fileURLToPath} from "node:url";

import {
  STRIP_ACCOUNT_BYTES,
  STRIP_COUNT,
  STRIP_HEADER_BYTES,
  STRIP_PIXELS_BYTES,
  STRIP_HASH_DOMAIN,
  FRAME_WIDTH,
  FRAME_HEIGHT,
  STRIP_WIDTH,
} from "../strip.js";
import {stripDigest} from "../strip.js";
import {decodeInput, signingPayload, validateInput} from "../input.js";

const here = dirname(fileURLToPath(import.meta.url));
export const PAGE_ROOT = join(here, "..");
export const REPO_ROOT = join(here, "..", "..", "..");

// The eight real strip accounts test_push.mjs and test_strip.mjs decode, read
// from Fogo testnet. A fake frame starts from the chain's own bytes.
function fixtureStrips() {
  const meta = JSON.parse(readFileSync(join(here, "fixtures", "frame-cursor-6016.json"), "utf8"));
  const joined = gunzipSync(readFileSync(join(here, "fixtures", "frame-cursor-6016-strips.bin.gz")));
  const strips = [];
  for (let i = 0; i < STRIP_COUNT; i += 1) {
    strips.push(new Uint8Array(joined.subarray(i * STRIP_ACCOUNT_BYTES, (i + 1) * STRIP_ACCOUNT_BYTES)));
  }
  return {meta, strips};
}

const sha256hex = (bytes) => createHash("sha256").update(Buffer.from(bytes)).digest("hex");

// Frame `index` of a fake render loop: the fixture's eight strips with the
// cursor and gametic advanced, the pixels changed so one frame is visibly not
// the next, and every strip digest recomputed over the new bytes. Nothing here
// is a shortcut past `decodePushedFrame`: if the digest is wrong the page
// refuses the frame, which is what the tests would then see.
export async function makeFrame(index, {strips: base, meta, startCursor = 6016, ticsPerFrame = 8} = {}) {
  const source = base ?? fixtureStrips().strips;
  const cursor = startCursor + index * ticsPerFrame;
  const gametic = cursor;
  const snapshotHash = new Uint8Array(createHash("sha256").update(`snap:${index}`).digest());
  const stateRoot = new Uint8Array(createHash("sha256").update(`root:${index}`).digest());
  const out = [];
  for (let stripId = 0; stripId < STRIP_COUNT; stripId += 1) {
    const account = new Uint8Array(STRIP_ACCOUNT_BYTES);
    account.set(source[stripId].subarray(0, STRIP_HEADER_BYTES));
    const pixels = new Uint8Array(STRIP_PIXELS_BYTES);
    pixels.set(source[stripId].subarray(STRIP_HEADER_BYTES));
    // A moving picture: a bar that walks across the band, so two frames of this
    // fake are not byte-identical and a stalled painter is visible as a stalled
    // picture rather than as a repeated one.
    const column = (index * 3 + stripId * STRIP_WIDTH) % FRAME_WIDTH;
    for (let row = 0; row < FRAME_HEIGHT; row += 1) {
      const at = row * STRIP_WIDTH + ((column - stripId * STRIP_WIDTH + FRAME_WIDTH) % STRIP_WIDTH);
      pixels[at] = (pixels[at] + (index % 251) + 1) & 0xff;
    }
    const view = new DataView(account.buffer);
    view.setUint32(16, stripId, true);
    view.setUint32(20, cursor, true);
    view.setUint32(24, gametic, true);
    account.set(snapshotHash, 32);
    account.set(stateRoot, 64);
    const digest = await stripDigest({stripId, cursor, gametic, snapshotHash, pixels});
    account.set(digest, 96);
    account.set(pixels, STRIP_HEADER_BYTES);
    out.push(account);
  }
  // The 320x200 frame, for the fake chain to hold the same bytes at.
  const framePixels = new Uint8Array(FRAME_WIDTH * FRAME_HEIGHT);
  for (let stripId = 0; stripId < STRIP_COUNT; stripId += 1) {
    const body = out[stripId].subarray(STRIP_HEADER_BYTES);
    for (let row = 0; row < FRAME_HEIGHT; row += 1) {
      framePixels.set(body.subarray(row * STRIP_WIDTH, row * STRIP_WIDTH + STRIP_WIDTH), row * FRAME_WIDTH + stripId * STRIP_WIDTH);
    }
  }
  return {
    index,
    cursor,
    gametic,
    strips: out,
    pixels: framePixels,
    snapshotHash: sha256hex(snapshotHash),
    stateRoot: sha256hex(stateRoot),
    stripHashes: out.map((account) => sha256hex(account.subarray(96, 128))),
    frameSha: sha256hex(framePixels),
    meta,
  };
}

// The minimum WAD that answers `readPlaypal`: a header, a directory of three
// lumps, and a 768-byte PLAYPAL. The palette is a ramp, so a painted frame has
// visible structure and a screenshot is worth taking.
export function makeWad({paletteBytes = 768} = {}) {
  const lumps = [
    {name: "PLAYPAL", data: Uint8Array.from({length: paletteBytes}, (_, i) => (i * 37 + (i >> 8) * 11) & 0xff)},
    {name: "COLORMAP", data: new Uint8Array(34 * 256)},
    {name: "ENDOOM", data: new Uint8Array(4000)},
  ];
  const header = 12;
  let cursor = header;
  const entries = lumps.map((lump) => {
    const entry = {offset: cursor, size: lump.data.length, name: lump.name, data: lump.data};
    cursor += lump.data.length;
    return entry;
  });
  const directoryOffset = cursor;
  const image = new Uint8Array(directoryOffset + entries.length * 16);
  image.set(Buffer.from("IWAD", "ascii"), 0);
  new DataView(image.buffer).setUint32(4, entries.length, true);
  new DataView(image.buffer).setUint32(8, directoryOffset, true);
  // A directory entry is offset, size, then the 8-byte NUL-padded name, which
  // is the order `findLump` in rpc.js reads.
  let at = cursor;
  for (const entry of entries) {
    image.set(entry.data, entry.offset);
    new DataView(image.buffer).setUint32(at, entry.offset, true);
    new DataView(image.buffer).setUint32(at + 4, entry.size, true);
    image.set(Buffer.from(entry.name.padEnd(8, "\0"), "latin1"), at + 8);
    at += 16;
  }
  return image;
}

const ADDRESSES = {
  context: "9kmADZit1MbcEJ2iej61QQBQnZZWM7w1gaPV4ZrtsTBd",
  snapshot: "HvsYLMSa9fpSu78zxf7WJHPXMLqPJBBPzwZL2A1musuX",
  strips: [
    "29weKPwKjfuA3AJhCJAu2FigkctnMYyVnxGNZXt762xQ",
    "6hKZdeQyvCGYnY7NfPyr1C6ee4kx8uHWU98LkggZuN2x",
    "7jwdur3QxB8huSDGxk13zQh48rjZE4BRaxERK8vUCn1B",
    "bWfogeFBiwqSS9RjiSK4x4riiPtMgR1a5nxqWJGD8LX",
    "GHTLwNw9QnfPcJm5bGAJkaEARyirKmPHdp4gGZJ23U",
    "J18sppVmgb3mPEohhu3XyKxnKezKX2YgEMPJ42Fw6KXR",
    "73EXdNGvNPN5rdXhE8XjR6DKYQagUNoCanQxppFSd4by",
    "4TanM7mDQz1YivAtEfwtcfTa1MqkM4P71MZTWGEVR548",
  ],
};

const MIME = {".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".json": "application/json", ".bin": "application/octet-stream", ".gz": "application/gzip"};

// The relayer's own Ed25519 verification, so a page that signs wrongly is
// refused here exactly as it would be by the real one. An Ed25519 SPKI DER
// header is a fixed 12 bytes in front of the 32 raw public key bytes.
const SPKI_ED25519_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

function verifyEd25519(publicKeyHex, message, signature) {
  if (typeof publicKeyHex !== "string" || !/^[0-9a-f]{64}$/i.test(publicKeyHex)) return false;
  const bytes = (value) => value instanceof Uint8Array ? Buffer.from(value.buffer, value.byteOffset, value.byteLength) : null;
  const messageBytes = bytes(message);
  const signatureBytes = bytes(signature);
  if (messageBytes === null || signatureBytes === null || signatureBytes.length !== 64) return false;
  message = messageBytes;
  signature = signatureBytes;
  const key = createPublicKey({
    key: Buffer.concat([SPKI_ED25519_PREFIX, Buffer.from(publicKeyHex, "hex")]),
    format: "der",
    type: "spki",
  });
  return verifySignature(null, message, key, signature);
}

export class FakeRelayer {
  constructor({
    frameIntervalMs = 120,
    ringLimit = 4,
    ticsPerSecond = 35,
    startCursor = 6016,
    leadTics = 61,
    chainLagFrames = 1,
    verifySignatures = true,
    frames = null,
  } = {}) {
    const fixture = fixtureStrips();
    this.meta = fixture.meta;
    this.startCursor = startCursor;
    this.ticsPerSecond = ticsPerSecond;
    this.leadTics = leadTics;
    this.ringLimit = ringLimit;
    this.frameIntervalMs = frameIntervalMs;
    this.chainLagFrames = chainLagFrames;
    this.verifySignatures = verifySignatures;
    this.sessions = new Map();
    this.subscriptions = new Set();
    this.ring = new Map();
    this.frameIndex = 0;
    this.cursor = startCursor;
    this.producerNextCursor = startCursor;
    this.producerGeneratedCursor = startCursor - 4;
    this.staged = new Map();
    // Where this relayer has got to writing input. This is deliberately not the
    // floor a batch is checked against -- the floor is the chain cursor -- so
    // that a page rewriting the tics it is still filling is legal, which is what
    // `server.mjs` does in chain mode.
    this.lastAccepted = startCursor;
    this.stagedTics = 0;
    this.landedTics = 0;
    this.refusals = [];
    this.acceptedBatches = 0;
    this.batches = [];
    this.lastBatch = null;
    this.badSignatures = 0;
    this.pushed = 0;
    this.cache = new Map();
    this.frameCache = frames;
    this.timer = null;
    this.chain = null;
    this.server = createServer((request, response) => this.#handle(request, response));
    this.listeners = [];
  }

  // The frames this relayer will push, built once so every caller sees the same
  // bytes the page will.
  async frameAt(index) {
    if (!this.frameCache) {
      this.frameCache = new Map();
    }
    if (!this.frameCache.has(index)) {
      const fixture = fixtureStrips();
      this.frameCache.set(index, await makeFrame(index, {
        strips: fixture.strips,
        meta: fixture.meta,
        startCursor: this.startCursor,
      }));
    }
    return this.frameCache.get(index);
  }

  // The canned chain, served from this same origin so the page can reach it
  // without a preflight and so "the node never answers" is a socket that stays
  // open, which is what a wedged RPC actually looks like from the page.
  withChain({delayMs = 0, hang = false, fail = false} = {}) {
    this.chain = new FakeChain({relayer: this, delayMs, hang, fail});
    return this.chain;
  }

  async start() {
    await new Promise((resolve) => this.server.listen(0, "127.0.0.1", resolve));
    this.port = this.server.address().port;
    this.base = `http://127.0.0.1:${this.port}`;
    this.timer = setInterval(() => {
      this.tick();
    }, this.frameIntervalMs);
    this.timer.unref?.();
    return this;
  }

  async stop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    for (const subscription of this.subscriptions) subscription.close?.();
    await new Promise((resolve) => this.server.close(resolve));
  }

  onLog(fn) {
    this.listeners.push(fn);
  }

  log(...parts) {
    for (const fn of this.listeners) fn(...parts);
  }

  // One frame's worth of time: the producer's next tic moves, staged input at or
  // below the engine's cursor is consumed, and a frame is pushed. The engine
  // consuming tics is what makes the aim a moving target, which is the whole
  // difficulty of the input path.
  tick() {
    const step = Math.max(1, Math.round((this.ticsPerSecond * this.frameIntervalMs) / 1000));
    this.producerNextCursor += step;
    this.producerGeneratedCursor = this.producerNextCursor - 4;
    this.cursor += step;
    for (const [seq, tic] of this.staged) {
      if (tic < this.cursor - this.leadTics) {
        this.landedTics += 1;
        this.staged.delete(seq);
      }
    }
    void this.publishFrame();
  }

  async publishFrame() {
    const index = this.frameIndex;
    this.frameIndex += 1;
    const frame = await this.frameAt(index);
    const record = {
      frame_index: index,
      buffer: 0,
      cursor: frame.cursor,
      gametic: frame.gametic,
      frame_sha256: frame.frameSha,
      snapshot_hash: frame.snapshotHash,
      state_root: frame.stateRoot,
      completed_seconds: index * (this.frameIntervalMs / 1000),
      frames_completed: index + 1,
      snapshot_account: ADDRESSES.snapshot,
      strip_accounts: ADDRESSES.strips,
      strip_accounts_b64: frame.strips.map((bytes) => Buffer.from(bytes).toString("base64")),
      at: Date.now(),
    };
    this.ring.set(index, record);
    while (this.ring.size > this.ringLimit) this.ring.delete(this.ring.keys().next().value);
    this.pushed += 1;
    for (const subscription of this.subscriptions) {
      for (const listener of subscription.listeners) listener(record);
    }
    return record;
  }

  // The same four-frame ring and the same `since` rule as the real relayer:
  // the newest frame is handed over on subscribe, and a subscriber that says
  // which frame it last painted is not sent one it already has.
  subscribeFrames({since = null} = {}) {
    const newest = [...this.ring.values()].pop() ?? null;
    const pending = newest !== null && (since === null || newest.frame_index > since) ? [newest] : [];
    const subscription = {listeners: new Set(), pending, closed: false};
    subscription.add = (listener) => {
      subscription.listeners.add(listener);
      return () => subscription.listeners.delete(listener);
    };
    subscription.close = () => {
      subscription.closed = true;
      this.subscriptions.delete(subscription);
    };
    this.subscriptions.add(subscription);
    return subscription;
  }

  status(session) {
    const generated = this.producerGeneratedCursor;
    return {
      slot: 0,
      next_seq: this.lastAccepted,
      cursor: this.cursor,
      gametic: this.cursor,
      tics_per_second: this.ticsPerSecond,
      tics_per_second_cap: 35,
      staging_frontier: generated + 64,
      input_window: 60,
      producer: {generated_cursor: generated, stage_limit: generated + 64},
      producer_stage_limit: generated + 64,
      producer_generated_cursor: generated,
      producer_next_cursor: this.producerNextCursor,
      writable_frontier: generated + 64,
      writable_floor: this.cursor,
      input_fate: {deposited: this.stagedTics, landed: this.landedTics},
      pipeline: {state: "running", cursor_stalled_seconds: 0},
      lead_tics: this.leadTics,
      measured_round_trip_seconds: 0.86,
      frame_counters: {frames_submitted: this.pushed, frame_stalls: 0, frame_skips: 0, frame_retries: 0, frame_deadline_seconds: 1},
      latest_frame: {buffer: 0, snapshot_account: ADDRESSES.snapshot, strip_accounts: ADDRESSES.strips, cursor: this.cursor, gametic: this.cursor},
      context: ADDRESSES.context,
      snapshot_account: ADDRESSES.snapshot,
      strip_accounts: ADDRESSES.strips,
      input_capacity: 65536,
      tics_landed: this.landedTics,
      frames_landed: this.pushed,
      input_tics_staged: this.stagedTics,
      frame: {cursor: this.cursor, gametic: this.cursor, signature: null, explorer: null},
      state: session?.state ?? "playing",
    };
  }

  createSession(publicKey) {
    const id = `s-${Math.random().toString(16).slice(2, 10)}`;
    const session = {id, publicKey, state: "playing", createdAt: Date.now(), lastSeenAt: Date.now(), clientSeq: 0};
    this.sessions.set(id, session);
    this.log(`session ${id} created`);
    return {
      id,
      slot: 0,
      label: "fake",
      state: "playing",
      mode: "mock",
      network: "testnet",
      // 32 bytes of hex, which is what `fromHex` on the page insists on.
      binding: "11".repeat(32),
      publicKey,
      reattached: false,
      ...this.status(session),
    };
  }

  postInput(sessionId, body) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      const error = new Error(`no session ${sessionId}`);
      error.status = 404;
      error.code = "unknown_session";
      throw error;
    }
    let message;
    let signature;
    try {
      message = Buffer.from(body.message ?? "", "base64");
      signature = Buffer.from(body.signature ?? "", "base64");
    } catch {
      const error = new Error("message and signature are base64");
      error.status = 400;
      error.code = "bad_encoding";
      throw error;
    }
    if (this.verifySignatures && !verifyEd25519(session.publicKey, signingPayload(message), signature)) {
      this.badSignatures += 1;
      const error = new Error("the signature does not verify against the session's public key");
      error.status = 401;
      error.code = "bad_signature";
      throw error;
    }
    let batch;
    try {
      batch = decodeInput(message);
    } catch (error) {
      const error2 = new Error(`the batch does not decode: ${error.message}`);
      error2.status = 400;
      error2.code = "bad_batch";
      throw error2;
    }
    // The relayer's own rules, the ones `validateInput` is the reference for, so
    // a page that aims badly is refused for the reason it would be refused live.
    // The floor is the chain cursor, not a count of what this relayer has
    // accepted: that is what `server.mjs` does in chain mode
    // (`Math.min(slot.nextSeq, slot.cursor)`), and it is what lets a page
    // rewrite the tics it is still filling.
    const check = validateInput(batch, {
      expectedSlot: 0,
      nowSeconds: Math.floor(Date.now() / 1000),
      nextSeq: this.cursor,
      window: 240,
    });
    if (!check.ok) {
      this.refusals.push({reason: check.reason, firstSeq: batch.firstSeq, at: Date.now()});
      const error = new Error(check.reason);
      error.status = 409;
      error.code = "input_refused";
      error.detail = {next_seq: this.lastAccepted, cursor: this.cursor, staging_frontier: this.producerGeneratedCursor + 64, input_window: 60, producer_generated_cursor: this.producerGeneratedCursor, producer_next_cursor: this.producerNextCursor, producer_stage_limit: this.producerGeneratedCursor + 64};
      throw error;
    }
    this.lastAccepted = Math.max(this.lastAccepted, batch.firstSeq + batch.count);
    this.acceptedBatches += 1;
    // The commands the page actually signed, so a test can ask what a key did
    // rather than asking the page what it believes it did. A turn only lands on
    // every fourth tic (see `heldToCommands`), so the turns are collected across
    // the whole batch rather than read off its first command.
    const view = new DataView(batch.commands.buffer, batch.commands.byteOffset, batch.commands.byteLength);
    const turns = new Set();
    let forward = 0;
    let side = 0;
    let buttons = 0;
    for (let i = 0; i < batch.count; i += 1) {
      if (view.getUint8(i * 4 + 2) !== 0) turns.add(view.getUint8(i * 4 + 2));
      if (view.getUint8(i * 4) !== 0) forward = view.getUint8(i * 4);
      if (view.getUint8(i * 4 + 1) !== 0) side = view.getUint8(i * 4 + 1);
      if (view.getUint8(i * 4 + 3) !== 0) buttons = view.getUint8(i * 4 + 3);
    }
    this.lastBatch = {
      clientSeq: batch.clientSeq,
      firstSeq: batch.firstSeq,
      count: batch.count,
      forward,
      side,
      // 4 is one angleturn step counter-clockwise (left) and 252 is one step
      // clockwise (right); see `ticcmd`, which is the engine's byte << 8.
      turns: [...turns].sort((a, b) => a - b),
      buttons,
    };
    this.batches.push(this.lastBatch);
    for (let i = 0; i < batch.count; i += 1) {
      this.staged.set(batch.firstSeq + i, this.producerGeneratedCursor);
      this.stagedTics += 1;
    }
    return {
      accepted: batch.count,
      accepted_from: batch.firstSeq,
      trimmed_from_seq: null,
      queued_above_stage_limit: null,
      client_seq: batch.clientSeq,
      ...this.status(session),
    };
  }

  async #handle(request, response) {
    const url = new URL(request.url, "http://relayer.invalid");
    const cors = {
      "access-control-allow-origin": request.headers.origin ?? "*",
      "access-control-allow-headers": "content-type",
      "access-control-allow-methods": "GET,POST,DELETE,OPTIONS",
    };
    if (request.method === "OPTIONS") {
      response.writeHead(204, cors);
      response.end();
      return;
    }
    const json = (status, payload) => {
      const body = JSON.stringify(payload);
      response.writeHead(status, {"content-type": "application/json", "content-length": Buffer.byteLength(body), ...cors});
      response.end(body);
    };
    const readBody = () => new Promise((resolve) => {
      const chunks = [];
      request.on("data", (chunk) => chunks.push(chunk));
      request.on("end", () => {
        const text = Buffer.concat(chunks).toString("utf8");
        try {
          resolve(text ? JSON.parse(text) : {});
        } catch {
          resolve({});
        }
      });
    });

    if (url.pathname === "/" && request.method === "POST") {
      const body = await readBody();
      const chain = this.chain ?? this.withChain();
      if (chain.hang) {
        // A node that accepts the request and never answers. Left open on
        // purpose: this is the shape of fault the painted path has to survive,
        // and the page's RPC client has no timeout of its own, so without a fix
        // in the page this wedges the paint loop for good.
        this.log("chain: hanging one request");
        return;
      }
      if (chain.delayMs) await new Promise((resolve) => setTimeout(resolve, chain.delayMs));
      try {
        const answer = await chain.answer(body);
        json(answer.status, JSON.parse(answer.body));
      } catch (error) {
        json(200, {jsonrpc: "2.0", id: body?.id ?? null, error: {code: -32000, message: error.message}});
      }
      return;
    }
    if (url.pathname === "/healthz") {
      json(200, {
        ok: true,
        mode: "mock",
        frame_push: {
          frames_seen: this.pushed,
          frames_pushed: this.pushed,
          frames_without_bytes: 0,
          ring: this.ring.size,
          subscribers: this.subscriptions.size,
        },
        input: {
          batches: this.acceptedBatches,
          tics_staged: this.stagedTics,
          tics_landed: this.landedTics,
          refusals: this.refusals.length,
          bad_signatures: this.badSignatures,
        },
        producer: {
          next_cursor: this.producerNextCursor,
          generated_cursor: this.producerGeneratedCursor,
          lead_tics: this.leadTics,
        },
      });
      return;
    }
    if (url.pathname === "/v1/sessions" && request.method === "POST") {
      const body = await readBody();
      json(200, this.createSession(body.publicKey ?? "anonymous"));
      return;
    }
    const sessionMatch = /^\/v1\/sessions\/([^/]+)(\/[a-z]+)?$/.exec(url.pathname);
    if (sessionMatch) {
      const id = decodeURIComponent(sessionMatch[1]);
      const tail = sessionMatch[2] ?? "";
      const session = this.sessions.get(id);
      if (!session) {
        json(404, {error: `no session ${id}`, code: "unknown_session"});
        return;
      }
      if (tail === "/frames" && request.method === "GET") {
        const since = url.searchParams.get("since");
        response.writeHead(200, {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-store",
          connection: "keep-alive",
          "x-accel-buffering": "no",
          ...cors,
        });
        response.write("retry: 1000\n\n");
        const subscription = this.subscribeFrames({since: since === null ? null : Number(since)});
        for (const record of subscription.pending) {
          response.write(`event: frame\nid: ${record.frame_index}\ndata: ${JSON.stringify(record)}\n\n`);
        }
        const keepAlive = setInterval(() => response.write(": keep-alive\n\n"), 5_000);
        keepAlive.unref?.();
        const stop = subscription.add((record) => {
          response.write(`event: frame\nid: ${record.frame_index}\ndata: ${JSON.stringify(record)}\n\n`);
        });
        const close = () => {
          clearInterval(keepAlive);
          stop();
          subscription.close();
        };
        request.on("close", close);
        request.on("error", close);
        return;
      }
      if (tail === "/input" && request.method === "POST") {
        const body = await readBody();
        try {
          json(200, this.postInput(id, body));
        } catch (error) {
          json(error.status ?? 500, {error: error.message, code: error.code ?? "error", detail: error.detail ?? null});
        }
        return;
      }
      if (!tail && request.method === "GET") {
        session.lastSeenAt = Date.now();
        json(200, {id, ...this.status(session)});
        return;
      }
      if (!tail && request.method === "DELETE") {
        this.sessions.delete(id);
        json(200, {ended: id});
        return;
      }
    }
    if (url.pathname.startsWith("/web/doom-play/")) {
      this.#static(url.pathname, response);
      return;
    }
    json(404, {error: `no route ${request.method} ${url.pathname}`});
  }

  #static(pathname, response) {
    const relative = normalize(pathname.replace("/web/doom-play/", ""));
    if (relative.startsWith("..")) {
      response.writeHead(403).end();
      return;
    }
    const file = join(PAGE_ROOT, relative);
    let bytes;
    try {
      bytes = readFileSync(file);
    } catch {
      response.writeHead(404, {"content-type": "text/plain"}).end(`not found: ${pathname}`);
      return;
    }
    const dot = file.lastIndexOf(".");
    response.writeHead(200, {
      "content-type": MIME[file.slice(dot)] ?? "application/octet-stream",
      "content-length": bytes.length,
      "cache-control": "no-store",
    });
    response.end(bytes);
  }

  // The bytes the fake chain holds for the cross-check: a frame `chainLagFrames`
  // behind the newest, which is the lag a public node really has (measured on
  // R13: the node read three frames behind the relayer).
  async chainFrame() {
    const lag = Math.max(0, this.frameIndex - 1 - this.chainLagFrames);
    if (lag < 0) return null;
    return this.frameAt(lag);
  }
}

// The page's JSON-RPC, answered from the same frame bytes the stream pushed.
// This is the half of the cross-check that reads the chain, and it is the half
// that can be made slow or made to hang on purpose.
export class FakeChain {
  constructor({relayer, fail = false, sliceHeaderOnly = true} = {}) {
    this.relayer = relayer;
    this.fail = fail;
    this.sliceHeaderOnly = sliceHeaderOnly;
    this.calls = 0;
    this.multiCalls = 0;
    this.inFlight = 0;
    this.slowestMs = 0;
    this.wad = makeWad();
  }

  // One `getMultipleAccounts`. Three kinds of account live on this fake node:
  // the eight strips and the snapshot of the newest frame, the five WAD accounts
  // the palette is read out of, and nothing else. The page's cross-check asks
  // for nine addresses sliced to 192 bytes; the palette asks for four tiny
  // slices; the direct read asks for nine whole accounts. All three land here.
  async getMultipleAccounts(addresses, config = {}) {
    this.multiCalls += 1;
    const slice = config.dataSlice ?? null;
    const frame = await this.relayer.chainFrame();
    const out = [];
    for (const address of addresses) {
      let bytes = null;
      if (WAD_ADDRESSES.includes(address)) {
        bytes = await this.#wadSlice(address, slice ? slice.offset : 32, slice ? slice.length : 1048576 - 32);
      } else if (frame !== null && address === ADDRESSES.snapshot) {
        bytes = this.#snapshotHeader(frame);
      } else if (frame !== null && ADDRESSES.strips.includes(address)) {
        const index = ADDRESSES.strips.indexOf(address);
        bytes = slice
          ? frame.strips[index].subarray(slice.offset, slice.offset + slice.length)
          : frame.strips[index];
      }
      if (bytes !== null && slice !== null && bytes.length !== slice.length) {
        // The page asks for 192 bytes of a 128-byte header plus the first 64
        // pixel bytes. Hand back exactly the length it asked for.
        const padded = new Uint8Array(slice.length);
        padded.set(bytes.subarray(0, Math.min(bytes.length, slice.length)));
        bytes = padded;
      }
      out.push(bytes === null ? null : new Uint8Array(bytes));
    }
    return out;
  }

  // The snapshot's own 192-byte header. The strips name it by hash and the page
  // only reads the cursor out of it, so the hash field is left as the program
  // wrote it: this is a stand-in for an account, not a claim about one.
  #snapshotHeader(frame) {
    const header = new Uint8Array(192);
    const view = new DataView(header.buffer);
    header.set(Buffer.from("DOOM-RSN", "ascii"), 0);
    view.setUint32(8, 3, true);
    view.setUint32(12, 192, true);
    view.setUint32(20, frame.cursor, true);
    view.setUint32(24, frame.gametic, true);
    return header;
  }

  async #wadSlice(address, offset, length) {
    const index = WAD_ADDRESSES.indexOf(address);
    if (index < 0) return null;
    const accountBytes = 1048576;
    const headerBytes = 32;
    const imageBytes = 4196020;
    const start = index * accountBytes;
    const from = offset - headerBytes;
    if (from < 0 || from >= imageBytes) return null;
    const take = Math.min(length, imageBytes - from);
    if (from + take > this.wad.length) return null;
    return this.wad.subarray(from, from + take);
  }

  // One JSON-RPC request, answered the way a node answers it. The delay and the
  // hang are the server's, so `answer` is only what a node that did answer
  // would have said.
  async answer(request) {
    this.calls += 1;
    const started = Date.now();
    this.inFlight += 1;
    try {
      if (this.fail) throw new Error("the fake chain is set to fail");
      const result = await this.#dispatch(request);
      const body = JSON.stringify({jsonrpc: "2.0", id: request.id, result});
      return {status: 200, headers: {"content-type": "application/json"}, body};
    } finally {
      this.inFlight -= 1;
      this.slowestMs = Math.max(this.slowestMs, Date.now() - started);
    }
  }

  async #dispatch(request) {
    const {method, params} = request;
    if (method === "getSlot") return {slot: 1000 + this.calls};
    if (method === "getSignatureStatuses") return {value: []};
    if (method === "getMultipleAccounts") {
      const [addresses, config] = params ?? [];
      const payloads = await this.getMultipleAccounts(addresses ?? [], config ?? {});
      return {
        context: {slot: 1000 + this.calls},
        value: payloads.map((bytes) => (bytes === null ? null : {data: [Buffer.from(bytes).toString("base64"), "base64"]})),
      };
    }
    if (method === "getAccountInfo") {
      const [address, config] = params ?? [];
      const payloads = await this.getMultipleAccounts([address], config ?? {});
      const bytes = payloads[0];
      return {context: {slot: 1000 + this.calls}, value: bytes === null ? null : {data: [Buffer.from(bytes).toString("base64"), "base64"]}};
    }
    throw new Error(`the fake chain does not implement ${method}`);
  }
}

const WAD_ADDRESSES = [
  "FcxYBH7PGaotLCyjitGvTvBVzJEuzLTiF73iic2Y2mbX",
  "G9FFjAAq44k88irK7AMJJKCbv5djaV2wVApGUEes3mRA",
  "8xpPSXWdgzYMNZikHjhnzusauuvhGyUvvoXExThBZiAn",
  "Frr6BJzhumKngybNCWHycjbyve2NJ351dHGUSYHpBFFy",
  "7o9eaUTDVNTJXGSk1s1Uhu1dbPv4E7L2jQEcw7L3Zz3Z",
];

export {ADDRESSES, STRIP_HASH_DOMAIN};
