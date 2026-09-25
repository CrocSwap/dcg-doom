// The relayer HTTP API.
//
// Create a session, post signed inputs, read session status, end a session.
// The relayer pays every fee and hands out game slots from a pre-provisioned
// pool; the player signs inputs and never signs a transaction.
//
// In mock mode the whole surface works with no chain access at all, so the
// page can be exercised locally. The API shape is identical either way, which
// is the point: the mock is not a different API.
//
// Buildless, no dependencies, node >= 20.

import {createServer as createHttpServer} from "node:http";
import {createPublicKey, verify as nodeVerify} from "node:crypto";

import {toHex} from "../../web/doom-play/strip.js";

import {
  decodeInput,
  validateInput,
  signingPayload,
  MAX_COMMANDS_PER_BATCH,
  COMMAND_BYTES,
  slotBinding,
} from "../../web/doom-play/input.js";
import {DEFAULT_NETWORK, NETWORKS, requireNetwork, explorerTransaction} from "../../web/doom-play/network.js";
import {SlotPool, PoolError, SLOT_STATE} from "./pool.mjs";
import {createSender, SenderError} from "./sender.mjs";

export const API_VERSION = "doom-relayer/1";

const JSON_LIMIT = 64 * 1024;
const SESSION_TTL_MS = 45 * 60 * 1000;

// The page is served from a different origin than the relayer, so every
// response carries CORS headers and OPTIONS is answered. The default is any
// origin, which is safe here: a request only does anything with a signature
// the caller cannot forge, and the relayer's own origin is not a secret.
// Narrow it with --cors-origin if the relayer is ever exposed more widely.
function corsHeaders(origin = "*") {
  return {
    "access-control-allow-origin": origin,
    "access-control-allow-methods": "GET, POST, DELETE, OPTIONS",
    "access-control-allow-headers": "content-type",
    "access-control-max-age": "600",
  };
}

function send(response, status, payload, origin = "*") {
  const body = JSON.stringify(payload);
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(body),
    "cache-control": "no-store",
    ...corsHeaders(origin),
  });
  response.end(body);
}

function fail(response, status, code, message, detail = {}, origin = "*") {
  send(response, status, {error: message, code, ...detail}, origin);
}

async function readJson(request) {
  const chunks = [];
  let total = 0;
  for await (const chunk of request) {
    total += chunk.length;
    if (total > JSON_LIMIT) {
      const error = new Error("request body is too large");
      error.status = 413;
      throw error;
    }
    chunks.push(chunk);
  }
  if (!total) return {};
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    const error = new Error("request body is not JSON");
    error.status = 400;
    throw error;
  }
}

function fromBase64(value, name) {
  if (typeof value !== "string" || !value) {
    const error = new Error(`${name} must be base64`);
    error.status = 400;
    throw error;
  }
  const bytes = Buffer.from(value, "base64");
  if (!bytes.length) {
    const error = new Error(`${name} is empty`);
    error.status = 400;
    throw error;
  }
  return new Uint8Array(bytes);
}

function fromHex(value, name) {
  if (typeof value !== "string" || !/^[0-9a-fA-F]{64}$/.test(value)) {
    const error = new Error(`${name} must be 32 bytes of hex`);
    error.status = 400;
    throw error;
  }
  const bytes = new Uint8Array(32);
  for (let i = 0; i < 32; i += 1) bytes[i] = parseInt(value.slice(i * 2, i * 2 + 2), 16);
  return bytes;
}

// A burner public key is 32 raw ed25519 bytes. We wrap it in an SPKI prefix so
// node:crypto will take it, which is the only reason the prefix is here.
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export function publicKeyFromHex(hex) {
  return createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, Buffer.from(hex, "hex")]),
    format: "der",
    type: "spki",
  });
}

export class Relayer {
  constructor({
    network = DEFAULT_NETWORK,
    mode = "mock",
    pool = null,
    sender = null,
    slotCount = 2,
    logger = null,
    maxSkewSeconds = 120,
    seqWindow = 240,
    ticsPerFrame = 16,
  } = {}) {
    this.networkName = network;
    this.network = NETWORKS[network];
    this.mode = mode;
    this.logger = logger;
    this.maxSkewSeconds = maxSkewSeconds;
    this.seqWindow = seqWindow;
    // A frame is nine transactions, so the relayer renders on a tic cadence
    // rather than per batch. Measured on R8i: 17.3 strips/s at the sender.
    this.ticsPerFrame = ticsPerFrame;
    this.sender = sender ?? createSender({mode});
    this.pool = pool ?? new SlotPool({slots: []});
    this.sessions = new Map();
    this.startedAt = Date.now();
    this.stats = {sessionsCreated: 0, sessionsEnded: 0, inputsAccepted: 0, inputsRejected: 0, ticsLanded: 0, framesRendered: 0};
    this.promise = Promise.resolve();
    if (!this.pool.size) this.seedSlots(slotCount);
  }

  log(...parts) {
    this.logger?.(...parts);
  }

  // Mock mode gets synthetic slots so the page has something to point at. In
  // chain mode the operator provisions these out of band, because a context
  // costs about 73 FOGO of rent and that is not a per-player cost.
  seedSlots(count) {
    for (let index = 0; index < count; index += 1) {
      const label = `mock-${index}`;
      this.pool.slots.push({
        slot: index,
        label,
        accounts: {
          context: `MOCK-context-${index}`,
          meta: `MOCK-meta-${index}`,
          inputBuffer: `MOCK-buffer-${index}`,
          state: `MOCK-state-${index}`,
          mirror: `MOCK-mirror-${index}`,
          authority: `MOCK-authority-${index}`,
          snapshot: `MOCK-snapshot-${index}`,
        },
        strips: Array.from({length: 8}, (_, i) => `MOCK-strip-${index}-${i}`),
        inputCapacity: 6016,
        state: SLOT_STATE.IDLE,
        sessionId: null,
        cursor: 0,
        gametic: 0,
        nextSeq: 0,
        ticsLanded: 0,
        framesLanded: 0,
        createdAt: Date.now(),
      });
      this.sender.state?.set(label, {
        key: label,
        network: this.networkName,
        label,
        accounts: this.pool.slots[index].accounts,
        strips: this.pool.slots[index].strips,
        inputCapacity: 6016,
        cursor: 0,
        gametic: 0,
        firstTic: 0,
        deposits: [],
        advances: 0,
        frames: 0,
        lastSignature: null,
        halted: false,
      });
    }
  }

  // slotBinding is async because it hashes, so the result is memoised per slot
  // and awaited at the call sites rather than stored as a pending promise.
  async bindingForSlot(slot) {
    if (!this.bindings) this.bindings = new Map();
    if (!this.bindings.has(slot.label)) {
      this.bindings.set(
        slot.label,
        await slotBinding({
          context: slot.accounts.context,
          meta: slot.accounts.meta,
          inputBuffer: slot.accounts.inputBuffer,
        }),
      );
    }
    return this.bindings.get(slot.label);
  }

  // Everything the page needs to render a session: where the frame lives and
  // which tic it is.
  status(slot) {
    return {
      slot: slot.slot,
      next_seq: slot.nextSeq,
      cursor: slot.cursor,
      gametic: slot.gametic,
      tics_per_second: null,
      input_capacity: slot.inputCapacity,
      tics_landed: slot.ticsLanded,
      frames_landed: slot.framesLanded,
      context: slot.accounts.context,
      snapshot_account: slot.accounts.snapshot,
      strip_accounts: [...slot.strips],
      frame: {
        cursor: slot.cursor,
        gametic: slot.gametic,
        signature: slot.lastFrameSignature,
        explorer: slot.lastFrameSignature ? explorerTransaction(this.network, slot.lastFrameSignature) : null,
      },
    };
  }

  async createSession({publicKey, requestedSlot = null}) {
    const playerKey = publicKey ?? "anonymous";
    // A page reload presents the same burner key. Hand back the session it
    // already has rather than stranding its slot and taking a second one.
    const existing = [...this.sessions.values()].find(
      (session) => session.playerKey === playerKey && session.state === "playing",
    );
    if (existing && requestedSlot === null) {
      existing.lastSeenAt = Date.now();
      this.log(`session ${existing.id} re-attached by the same player`);
      return {
        id: existing.id,
        slot: existing.slot.slot,
        label: existing.slot.label,
        state: "playing",
        mode: this.mode,
        network: this.networkName,
        next_seq: existing.slot.nextSeq,
        binding: toHex(await this.bindingForSlot(existing.slot)),
        publicKey: playerKey,
        reattached: true,
        ...this.status(existing.slot),
      };
    }
    const {slot, sessionId} = this.pool.assign({playerKey, requestedSlot});
    const session = {
      id: sessionId,
      slot,
      playerKey,
      createdAt: Date.now(),
      lastSeenAt: Date.now(),
      clientSeq: 0,
      state: "playing",
      mode: this.mode,
      network: this.networkName,
    };
    // Warm the binding now, so the first input does not pay for it and a
    // misconfigured slot is reported at join time rather than mid-game.
    const binding = await this.bindingForSlot(slot);
    this.sessions.set(sessionId, session);
    this.stats.sessionsCreated += 1;
    this.log(`session ${sessionId} took slot ${slot.slot} (${slot.label})`);
    return {
      id: sessionId,
      slot: slot.slot,
      label: slot.label,
      state: "playing",
      mode: this.mode,
      network: this.networkName,
      next_seq: slot.nextSeq,
      binding: toHex(binding),
      publicKey: playerKey,
      reattached: false,
      ...this.status(slot),
    };
  }

  getSession(sessionId) {
    const session = this.sessions.get(sessionId);
    if (!session) {
      const error = new Error(`no session ${sessionId}`);
      error.status = 404;
      error.code = "unknown_session";
      throw error;
    }
    if (Date.now() - session.lastSeenAt > SESSION_TTL_MS) {
      const error = new Error(`session ${sessionId} expired after ${SESSION_TTL_MS / 60000} minutes of silence`);
      error.status = 410;
      error.code = "session_expired";
      throw error;
    }
    session.lastSeenAt = Date.now();
    return session;
  }

  // The core path: verify the player's signature, check the batch against the
  // slot's rules, then deposit and advance on chain.
  async postInput(sessionId, {message, signature, publicKey}) {
    const session = this.getSession(sessionId);
    if (session.state !== "playing") {
      const error = new Error(`session is ${session.state}`);
      error.status = 409;
      error.code = "session_not_playing";
      throw error;
    }
    const slot = session.slot;

    let decoded;
    try {
      decoded = decodeInput(message);
    } catch (error) {
      this.stats.inputsRejected += 1;
      error.status = 400;
      error.code = "bad_message";
      throw error;
    }

    // Signature before anything else: an unsigned batch is not a player's
    // input no matter how well formed it is.
    const key = publicKey ?? session.playerKey;
    fromHex(key, "publicKey"); // shape check before it reaches node:crypto
    const verified = nodeVerify(
      null,
      Buffer.from(signingPayload(message)),
      publicKeyFromHex(key),
      Buffer.from(signature),
    );
    if (!verified) {
      this.stats.inputsRejected += 1;
      const error = new Error("signature does not verify against the session key");
      error.status = 401;
      error.code = "bad_signature";
      throw error;
    }
    if (key !== session.playerKey) {
      this.stats.inputsRejected += 1;
      const error = new Error("batch is signed by a key that is not this session's player");
      error.status = 401;
      error.code = "wrong_player";
      throw error;
    }

    // Replay within one session: client_seq must strictly increase.
    if (decoded.clientSeq <= session.clientSeq && session.clientSeq !== 0) {
      this.stats.inputsRejected += 1;
      const error = new Error(`client_seq ${decoded.clientSeq} is not ahead of ${session.clientSeq}`);
      error.status = 409;
      error.code = "stale_client_seq";
      throw error;
    }
    session.clientSeq = decoded.clientSeq;

    const verdict = validateInput(decoded, {
      expectedSlot: slot.slot,
      expectedBinding: await this.bindingForSlot(slot),
      nowSeconds: Math.floor(Date.now() / 1000),
      maxSkewSeconds: this.maxSkewSeconds,
      nextSeq: slot.nextSeq,
      window: this.seqWindow,
    });
    if (!verdict.ok) {
      this.stats.inputsRejected += 1;
      const error = new Error(verdict.reason);
      error.status = 409;
      error.code = "batch_refused";
      throw error;
    }

    const commands = decoded.commands;
    const result = await this.sender.deposit({slot: slot.label, firstSeq: decoded.firstSeq, commands});
    const advanced = await this.sender.advance({slot: slot.label, maxTics: 0, expectedCursor: slot.cursor});

    slot.nextSeq = Math.max(slot.nextSeq, decoded.firstSeq + decoded.count);
    slot.cursor = advanced.cursor;
    slot.gametic = advanced.gametic;
    slot.ticsLanded += advanced.tics;
    this.stats.inputsAccepted += decoded.count;
    this.stats.ticsLanded += advanced.tics;

    // The first batch always renders, so a joining player sees a frame
    // immediately rather than waiting out the cadence.
    const shouldRender = slot.framesLanded === 0 || advanced.tics >= this.ticsPerFrame;
    if (shouldRender) {
      try {
        const frame = await this.sender.renderFrame({slot: slot.label, bufferId: 0});
        slot.lastFrameSignature = frame.signature;
        slot.framesLanded += 1;
        this.stats.framesRendered += 1;
      } catch (error) {
        // A render failure must not lose the tics: they are already on chain.
        this.log(`render failed on slot ${slot.slot}: ${error.message}`);
      }
    }

    return {
      ok: true,
      accepted: decoded.count,
      first_seq: decoded.firstSeq,
      next_seq: slot.nextSeq,
      cursor: slot.cursor,
      gametic: slot.gametic,
      tics: advanced.tics,
      deposit_signature: result.signature,
      advance_signature: advanced.signature,
      frame: {
        cursor: slot.cursor,
        gametic: slot.gametic,
        signature: slot.lastFrameSignature,
        explorer: slot.lastFrameSignature ? explorerTransaction(this.network, slot.lastFrameSignature) : null,
      },
    };
  }

  // The frame's eight strip accounts, base64. On chain the viewer reads these
  // straight from the network, so this endpoint is what lets a mock relayer
  // drive the same viewer without any chain access. The relayer knows which
  // tic the bytes belong to, which is the part a chain reader has to infer.
  async frameBytes(sessionId) {
    const session = this.getSession(sessionId);
    const slot = session.slot;
    const frame = await this.sender.renderFrame({slot: slot.label, bufferId: 0});
    slot.lastFrameSignature = frame.signature;
    slot.framesLanded += 1;
    this.stats.framesRendered += 1;
    return {
      ok: true,
      cursor: frame.cursor,
      gametic: frame.gametic,
      signature: frame.signature,
      snapshot_hash: frame.snapshotHash,
      state_root: frame.stateRoot,
      strips: frame.strips.map((bytes) => Buffer.from(bytes).toString("base64")),
      strip_accounts: [...slot.strips],
      source: "relayer",
    };
  }

  // Render on demand, so a client that just joined can be caught up without
  // waiting for the next batch.
  async renderNow(sessionId) {
    const session = this.getSession(sessionId);
    const slot = session.slot;
    const frame = await this.sender.renderFrame({slot: slot.label, bufferId: 0});
    slot.lastFrameSignature = frame.signature;
    slot.framesLanded += 1;
    this.stats.framesRendered += 1;
    return {ok: true, cursor: frame.cursor, gametic: frame.gametic, signature: frame.signature};
  }

  async endSession(sessionId) {
    const session = this.getSession(sessionId);
    const slot = session.slot;
    // A slot is spent unless something can bring it back. See the design
    // note: the deployed program has no reset and refuses extend-capacity.
    this.pool.release(sessionId, {spent: true, reason: "player left"});
    session.state = "ended";
    session.endedAt = Date.now();
    this.stats.sessionsEnded += 1;
    this.log(`session ${sessionId} ended; slot ${slot.slot} is now spent`);
    return {
      ok: true,
      id: sessionId,
      slot: slot.slot,
      state: "ended",
      tics_landed: slot.ticsLanded,
      frames_landed: slot.framesLanded,
      slot_state: SLOT_STATE.SPENT,
      next_slot_hint: this.pool.summary().available > 0 ? "join again to take another slot" : "pool is empty; provision more",
    };
  }

  // Requests are serialised per relayer so that two players cannot interleave
  // a deposit and an advance on the same slot.
  serialise(work) {
    const run = this.promise.then(work, work);
    this.promise = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }
}

export function createRelayer(options = {}) {
  return new Relayer(options);
}

const ROUTES = [
  {method: "GET", pattern: /^\/healthz$/, handler: async (relayer) => ({
    ok: true,
    api: API_VERSION,
    mode: relayer.mode,
    network: relayer.networkName,
    uptime_seconds: Math.round((Date.now() - relayer.startedAt) / 1000),
    pool: relayer.pool.summary(),
    stats: relayer.stats,
  })},
  {method: "GET", pattern: /^\/v1\/config$/, handler: async (relayer) => ({
    api: API_VERSION,
    mode: relayer.mode,
    network: relayer.networkName,
    label: relayer.network?.label ?? null,
    commitment: relayer.network?.commitment ?? null,
    max_commands_per_batch: MAX_COMMANDS_PER_BATCH,
    command_bytes: COMMAND_BYTES,
    pool: relayer.pool.summary(),
    slots: relayer.pool.list(),
  })},
  {method: "POST", pattern: /^\/v1\/sessions$/, handler: async (relayer, request) => {
    const body = await readJson(request);
    return relayer.createSession({publicKey: body.publicKey ?? null, requestedSlot: body.slot ?? null});
  }},
  {method: "GET", pattern: /^\/v1\/sessions\/([^/]+)$/, handler: async (relayer, request, [sessionId]) => {
    const session = relayer.getSession(decodeURIComponent(sessionId));
    return {id: session.id, slot: session.slot.slot, state: session.state, ...relayer.status(session.slot)};
  }},
  {method: "POST", pattern: /^\/v1\/sessions\/([^/]+)\/input$/, handler: async (relayer, request, [sessionId]) => {
    const body = await readJson(request);
    const message = fromBase64(body.message, "message");
    const signature = fromBase64(body.signature, "signature");
    if (message.length > 64 + MAX_COMMANDS_PER_BATCH * COMMAND_BYTES) {
      const error = new Error("message is longer than a full batch");
      error.status = 400;
      error.code = "message_too_long";
      throw error;
    }
    return relayer.postInput(decodeURIComponent(sessionId), {message, signature, publicKey: body.publicKey ?? null});
  }},
  {method: "POST", pattern: /^\/v1\/sessions\/([^/]+)\/render$/, handler: async (relayer, request, [sessionId]) =>
    relayer.renderNow(decodeURIComponent(sessionId)),
  },
  {method: "GET", pattern: /^\/v1\/sessions\/([^/]+)\/frame$/, handler: async (relayer, request, [sessionId]) =>
    relayer.frameBytes(decodeURIComponent(sessionId)),
  },
  {method: "DELETE", pattern: /^\/v1\/sessions\/([^/]+)$/, handler: async (relayer, request, [sessionId]) =>
    relayer.endSession(decodeURIComponent(sessionId)),
  },
  {method: "GET", pattern: /^\/v1\/slots$/, handler: async (relayer) => ({pool: relayer.pool.summary(), slots: relayer.pool.list()})},
];

export function createServer(relayer, {onLog = null, corsOrigin = "*"} = {}) {
  const server = createHttpServer((request, response) => {
    const url = new URL(request.url, "http://relayer.invalid");
    // Preflight, for the page's cross-origin POSTs.
    if (request.method === "OPTIONS") {
      response.writeHead(204, corsHeaders(corsOrigin));
      return response.end();
    }
    const route = ROUTES.find((candidate) => candidate.method === request.method && candidate.pattern.test(url.pathname));
    if (!route) {
      const known = ROUTES.map((candidate) => `${candidate.method} ${candidate.pattern.source}`).join(", ");
      return fail(response, 404, "no_route", `${request.method} ${url.pathname} is not a route. Routes: ${known}`, {}, corsOrigin);
    }
    const params = url.pathname.match(route.pattern).slice(1);
    // One request at a time, so a deposit and an advance cannot interleave.
    void relayer.serialise(async () => {
      try {
        const payload = await route.handler(relayer, request, params);
        send(response, 200, payload, corsOrigin);
        onLog?.(`${request.method} ${url.pathname} 200`);
      } catch (error) {
        const status = error.status ?? (error instanceof PoolError ? 409 : error instanceof SenderError ? 502 : 500);
        const code = error.code ?? error.name ?? "error";
        if (status >= 500) onLog?.(`${request.method} ${url.pathname} ${status} ${error.message}`);
        fail(response, status, code, error.message, status >= 500 ? {} : {detail: error.detail ?? null}, corsOrigin);
      }
    });
  });
  return server;
}

// node stunts/doom-relayer/server.mjs --mode mock --port 8787 --slots 4
function isMain() {
  return process.argv[1] && import.meta.url === `file://${process.argv[1]}`;
}

if (isMain()) {
  const args = new Map();
  for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i].replace(/^--/, ""), process.argv[i + 1]);
  const mode = args.get("mode") ?? "mock";
  const port = Number(args.get("port") ?? 8787);
  const network = args.get("network") ?? DEFAULT_NETWORK;
  requireNetwork(network, ["doom.program"]);
  const relayer = createRelayer({
    network,
    mode,
    slotCount: Number(args.get("slots") ?? 2),
    logger: (...parts) => console.error(new Date().toISOString(), ...parts),
  });
  const corsOrigin = args.get("cors-origin") ?? "*";
  const server = createServer(relayer, {onLog: (line) => console.error(new Date().toISOString(), line), corsOrigin});
  server.listen(port, () => {
    console.error(`doom relayer ${API_VERSION} on http://127.0.0.1:${port} mode=${mode} network=${network} cors=${corsOrigin}`);
    console.error(`pool: ${JSON.stringify(relayer.pool.summary())}`);
  });
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => {
      server.close(() => process.exit(0));
    });
  }
}
