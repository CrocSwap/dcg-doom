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
import {readFileSync, rmSync, rmdirSync} from "node:fs";

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
import {SlotPool, PoolError, SLOT_STATE, makeSlot} from "./pool.mjs";
import {createSender, SenderError} from "./sender.mjs";
import {PublicMode, PublicModeError} from "./public-mode.mjs";

export const API_VERSION = "doom-relayer/1";

const JSON_LIMIT = 64 * 1024;
const SESSION_TTL_MS = 45 * 60 * 1000;

// The page is served from a different origin than the relayer, so every
// response carries CORS headers and OPTIONS is answered. The default is any
// origin, which is safe here: a request only does anything with a signature
// the caller cannot forge, and the relayer's own origin is not a secret.
// Narrow it with --cors-origin if the relayer is ever exposed more widely.
// A route that answers on the response itself -- the frame stream, which never
// finishes -- returns this instead of a payload. Every other route returns JSON
// and the server below writes it, so this is the only escape hatch and it is
// named.
const RAW_RESPONSE = Symbol("doom-relayer.raw-response");

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
    advanceTics = 4,
    inputLeadTics = 96,
    publicMode = false,
    publicStatePath = null,
    publicMaxViewers = 200,
    publicSpendCapFogo = 300,
    publicNow = undefined,
    publicRandom = undefined,
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
    // How wide the live input window is: one advance packet's worth of tics.
    this.advanceTics = advanceTics;
    // How far past the staging frontier a page may aim, in tics. A page that
    // aims at the frontier is aiming below the producer by the time its batch
    // lands: the round trip to this process is 0.3-0.4 s measured, which is
    // 7-11 tics at the measured 23.8-26.3 tics/s, and the frontier is about
    // four tics wide. So the page leads the window by its own round trip and
    // its batches arrive above the frontier, and this is the bound that keeps
    // "ahead by one round trip" from becoming a backlog.
    this.inputLeadTics = inputLeadTics;
    this.sender = sender ?? createSender({mode});
    this.pool = pool ?? new SlotPool({slots: []});
    this.sessions = new Map();
    this.publicSession = null;
    this.startedAt = Date.now();
    this.stats = {sessionsCreated: 0, sessionsEnded: 0, inputsAccepted: 0, inputsRejected: 0, ticsLanded: 0, framesRendered: 0};
    this.promise = Promise.resolve();
    // The frame push. `frameStream` holds the newest few complete frames, keyed
    // by frame_index, and `subscriptions` is who is currently painting from
    // them. The bytes come from the pipeline process, which read them off the
    // chain and already verified every strip digest; a subscriber that gets a
    // frame from here gets the program's own bytes, not this process's opinion
    // of them.
    this.frameStream = new Map();
    this.frameStreamLimit = 4;
    this.frameStreamSeen = 0;
    this.subscriptions = new Set();
    // A frame the pipeline announced but could not attach bytes to (the ring
    // turned over, or a mock relayer). Counted, because "the push path silently
    // stopped pushing" has to be a number on the receipt, not a quiet gap.
    this.framesWithoutBytes = 0;
    this.sender.onFrame = (frame) => this.onFrame(frame);
    this.publicMode = publicMode ? new PublicMode({
      relayer: this,
      statePath: publicStatePath,
      maxViewers: publicMaxViewers,
      spendCapFogo: publicSpendCapFogo,
      now: publicNow,
      random: publicRandom,
    }) : null;
    if (this.publicMode) {
      this.sender.onEvent = (event) => this.publicMode.onPipelineEvent(event);
      this.sender.onProgress = (event) => this.publicMode.onPipelineEvent(event);
    }
    // Mock mode is synchronous and self-contained: the synthetic slots are what
    // the page is developed against. Chain mode has to ask a process what
    // context it is driving, so that waits for init().
    if (!this.pool.size && !this.sender.background) this.seedSlots(slotCount);
  }

  // One complete frame from the pipeline process, forwarded to whoever is
  // painting. This is the whole of the push path: the page is sent the frame
  // instead of going to look for it, which is the difference between a picture
  // that keeps up and one that catches a frame mid-write and refuses it.
  onFrame(frame) {
    if (!frame || frame.event !== "frame") return;
    this.frameStreamSeen += 1;
    const bytes = frame.strip_accounts_b64;
    if (!Array.isArray(bytes) || bytes.length !== 8) {
      this.framesWithoutBytes += 1;
      return;
    }
    const record = {
      frame_index: frame.frame_index ?? null,
      buffer: frame.buffer ?? null,
      cursor: frame.cursor ?? null,
      gametic: frame.gametic ?? null,
      frame_sha256: frame.frame_sha256 ?? null,
      snapshot_hash: frame.snapshot_hash ?? null,
      state_root: frame.state_root ?? null,
      completed_seconds: frame.completed_seconds ?? null,
      frames_completed: frame.frames_completed ?? null,
      snapshot_account: frame.snapshot_account ?? null,
      strip_accounts: Array.isArray(frame.strip_accounts) ? frame.strip_accounts : [],
      strip_accounts_b64: bytes,
      at: Date.now(),
    };
    this.frameStream.set(record.frame_index, record);
    while (this.frameStream.size > this.frameStreamLimit) {
      this.frameStream.delete(this.frameStream.keys().next().value);
    }
    // A status poll also answers with the newest frame, so a page that has not
    // subscribed yet is not told about a frame it cannot see yet.
    for (const subscription of this.subscriptions) {
      for (const listener of subscription.listeners) listener(record);
    }
    this.publicMode?.onFrame(record);
    return record;
  }

  // Subscribe to the frame stream. The newest frame is handed over immediately
  // so a page that joins between frames does not sit on a stale picture for a
  // whole cadence; `since` lets a reconnecting page say which frame it last
  // painted, and it is answered with *every* frame this relayer still holds that
  // is newer than that, oldest first.
  //
  // It used to be answered with the newest frame alone, which is a page that
  // loses every frame completed while its stream was down and no counter
  // anywhere says so -- nothing torn, nothing refused, the picture simply
  // further behind. Measured on R15, 2026-09-26: 68 frames painted against the
  // ~86 the chain completed in the same 47 s. A frame outside the ring is gone
  // for good, which is the ring's bound and the operator's policy; a frame
  // inside it does not have to be.
  subscribeFrames({since = null} = {}) {
    const held = [...this.frameStream.values()];
    const newest = held.pop() ?? null;
    const pending = since === null
      ? (newest === null ? [] : [newest])
      : held.concat(newest === null ? [] : [newest]).filter((record) => (record.frame_index ?? 0) > since);
    const subscription = {listeners: new Set(), pending};
    this.subscriptions.add(subscription);
    return {
      pending,
      add: (listener) => {
        subscription.listeners.add(listener);
        return () => subscription.listeners.delete(listener);
      },
      close: () => this.subscriptions.delete(subscription),
    };
  }

  // A background sender is already driving a provisioned context, so the pool
  // is seeded from what it reports rather than from synthetic mock addresses.
  // In mock mode the constructor has already done it and this is a no-op.
  async init() {
    if (!this.pool.size && this.sender.background) {
      const slot = await this.sender.provisionSlot({network: this.networkName, label: null});
      this.pool.slots.push(makeSlot({
        slot: 0,
        label: slot.label,
        context: slot.context,
        meta: slot.meta,
        inputBuffer: slot.inputBuffer,
        state: slot.state,
        mirror: slot.mirror,
        authority: slot.authority,
        snapshot: slot.snapshot,
        strips: slot.strips,
        inputCapacity: slot.inputCapacity,
      }));
      const record = this.pool.slots[0];
      record.cursor = slot.cursor ?? 0;
      record.gametic = slot.gametic ?? 0;
      record.nextSeq = slot.nextSeq ?? 0;
      record.latestFrame = slot.latestFrame ?? null;
      this.log(`slot 0 is context ${record.accounts.context} at cursor ${record.cursor} of ${record.inputCapacity}`);
    }
    this.publicMode?.start();
    return this;
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

  // Ask the sender where the chain is, and fold it into the slot.
  //
  // The cursor is the only authority on where input may be written: a tic the
  // chain has already consumed is gone, and a DEPOSIT into that slot is
  // invisible to the game. `nextSeq` is therefore anchored to the cursor rather
  // than left to count only the tics this relayer has accepted. A relayer that
  // lets nextSeq lag the cursor accepts input into dead slots and reports
  // inputsAccepted while the player stands still, which is the R8l6 failure.
  //
  // Mock mode is exempt: there the cursor is what the player's own batches
  // advance, so there is nothing external to read.
  async refreshSlot(slot) {
    if (!this.sender.background) return slot;
    let progress;
    try {
      progress = await this.sender.readProgress({slot: slot.label});
    } catch (error) {
      // The child is between requests, or the context is dead. A status that
      // is a little stale beats a status that is an error.
      this.log(`cursor read failed on slot ${slot.slot}: ${error.message}`);
      return slot;
    }
    slot.cursor = progress.cursor;
    slot.gametic = progress.gametic;
    slot.ticsPerSecond = progress.ticsPerSecond ?? null;
    slot.stagingFrontier = progress.stagingFrontier ?? null;
    slot.producer = progress.producer ?? slot.producer ?? null;
    slot.writableFrontier = progress.writableFrontier ?? null;
    slot.writableFloor = progress.writableFloor ?? null;
    slot.producerNextCursor = progress.producerNextCursor ?? null;
    // The lead the producer resolved, and the round trip it was read from.
    // `readProgress` has asked the child for both since 2026-09-26 and this is
    // where they were dropped on the floor: fourteen fields were copied onto the
    // slot and these two were not, so `status` published `null` for the lead and
    // a page that aims with the producer's own number never saw it. Measured on
    // R15, 2026-09-26, at a live page still aiming its own 17 tics while the
    // child was publishing 61 in the very same answer, one field down.
    slot.inputStageLeadTics = progress.inputStageLeadTics ?? null;
    slot.inputRoundTripSeconds = progress.inputRoundTripSeconds ?? null;
    slot.inputFate = progress.inputFate ?? slot.inputFate ?? null;
    slot.pipeline = progress.pipeline ?? slot.pipeline ?? null;
    slot.ticsPerSecondCap = progress.ticsPerSecondCap ?? slot.ticsPerSecondCap ?? null;
    slot.frameCounters = progress.frameCounters ?? slot.frameCounters ?? null;
    slot.latestFrame = progress.latestFrame ?? slot.latestFrame ?? null;
    slot.rpcUrls = progress.rpcUrls ?? slot.rpcUrls ?? null;
    slot.nextSeq = Math.max(slot.nextSeq, slot.cursor);
    return slot;
  }

  // The tics a keystroke can still reach: the last `advance-tics` before the
  // staging frontier. The producer has already deposited everything below its
  // own stage limit, which is the same reach and lead the frontier is built
  // from, so the rest of the window is a dead zone -- a write there is accepted,
  // reported and never read by the game. The page aims here; the relayer is the
  // only side that knows both numbers.
  inputWindow(slot) {
    if (!this.sender.background || slot.stagingFrontier === null || slot.stagingFrontier === undefined) return null;
    return Math.max(1, Math.min(this.advanceTics, slot.stagingFrontier - slot.cursor));
  }

  // Everything the page needs to render a session: where the frame lives and
  // which tic it is. `next_seq` and `staging_frontier` bound where the page may
  // write: at or after the first, at or before the second.
  status(slot) {
    return {
      slot: slot.slot,
      next_seq: slot.nextSeq,
      cursor: slot.cursor,
      gametic: slot.gametic,
      tics_per_second: slot.ticsPerSecond ?? null,
      staging_frontier: slot.stagingFrontier ?? null,
      input_window: this.inputWindow(slot),
      // The producer's own two cursors. `producer_stage_limit` is the sequence
      // it will deposit next and `producer_generated_cursor` the last one it
      // has, so the page can aim at a window that is still writable instead of
      // guessing from a cursor the producer does not use.
      producer: slot.producer ?? null,
      producer_stage_limit: slot.producer?.stage_limit ?? null,
      producer_generated_cursor: slot.producer?.generated_cursor ?? null,
      // The first input slot the chain has not fixed, and the floor a staged
      // batch is refused below. Named here so a page (and a receipt) can see
      // which of the two numbers a refusal came from.
      writable_frontier: slot.writableFrontier ?? null,
      writable_floor: slot.writableFloor ?? null,
      // The producer's next tic to generate: the aim point a page needs and the
      // one number that is not `producer_generated_cursor`. Without it a page
      // leads by its round trip measured from a counter that already runs a
      // batch ahead, and the lead is gone before the batch is written.
      producer_next_cursor: slot.producerNextCursor ?? null,
      // The lead the producer resolved from its own measured round trip, and the
      // round trip it was read from. The child publishes both; this is where they
      // stop being the pipeline's private numbers and become the viewer's, which
      // is the only way a page can aim with the producer's own arithmetic instead
      // of re-deriving one from its own POST latency.
      input_stage_lead_tics: slot.inputStageLeadTics ?? null,
      input_round_trip_seconds: slot.inputRoundTripSeconds ?? null,
      // Per-deposit fate for the player's commands: the bytes the chain was
      // given and the transaction that carried them. It rides on `status`
      // because the status poll already asks the child, so it is free.
      input_fate: slot.inputFate ?? null,
      // Whether keystrokes can still reach the engine, and how long the chain's
      // cursor has stood still. A page that cannot see this shows a frozen
      // picture and takes keys anyway.
      pipeline: slot.pipeline ?? null,
      tics_per_second_cap: slot.ticsPerSecondCap ?? null,
      frame_counters: slot.frameCounters ?? null,
      // Which render buffer holds the newest COMPLETE frame, and that buffer's
      // accounts. The child read the frame back before counting it, so it knows
      // which buffer is whole; the viewer does not, and reading the buffer being
      // written is what made it refuse to paint at strip rate 64.
      latest_frame: slot.latestFrame ?? null,
      rpc_urls: slot.rpcUrls ?? null,
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
    if (!session.public && Date.now() - session.lastSeenAt > SESSION_TTL_MS) {
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
  async postInput(sessionId, {message, signature, publicKey}, {publicToken = null} = {}) {
    const postReceivedAtMs = Date.now();
    const session = this.getSession(sessionId);
    if (session.state !== "playing") {
      const error = new Error(`session is ${session.state}`);
      error.status = 409;
      error.code = "session_not_playing";
      throw error;
    }
    const slot = session.slot;
    if (this.publicMode) this.publicMode.assertCanInput(publicToken, publicKey, {rateLimit: false});
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

    // V2 carries a full key state, not an absolute sequence. The pipeline owns
    // the frontier and stamps it at its next free tic, so do not spend an RPC
    // read refreshing a cursor the browser has no reason to know.
    if (decoded.sequenceMode === "now") {
      if (session.clientSeq !== 0 && decoded.clientSeq <= session.clientSeq) {
        this.stats.inputsRejected += 1;
        const error = new Error(`client_seq ${decoded.clientSeq} is not ahead of ${session.clientSeq}`);
        error.status = 409;
        error.code = "stale_client_seq";
        throw error;
      }
      session.clientSeq = decoded.clientSeq;
      const binding = await this.bindingForSlot(slot);
      const verdict = validateInput(decoded, {
        expectedSlot: slot.slot,
        expectedBinding: binding,
        nowSeconds: Math.floor(Date.now() / 1000),
        maxSkewSeconds: this.maxSkewSeconds,
      });
      if (!verdict.ok) {
        this.stats.inputsRejected += 1;
        const error = new Error(verdict.reason);
        error.status = 409;
        error.code = "batch_refused";
        throw error;
      }
      this.publicMode?.notePendingInput(publicToken);
      let result;
      try {
        result = await this.sender.keyState({slot: slot.label, keyMask: decoded.keyMask});
        if (this.publicMode) await this.publicMode.resumeForInput(publicToken);
      } catch (error) {
        this.stats.inputsRejected += 1;
        throw error;
      }
      slot.cursor = result.cursor ?? slot.cursor;
      slot.gametic = result.gametic ?? slot.gametic;
      slot.producerNextCursor = result.producerNextCursor ?? slot.producerNextCursor;
      slot.pipeline = result.pipeline ?? slot.pipeline;
      this.stats.inputsAccepted += 1;
      this.publicMode?.noteAcceptedInput(publicToken);
      return {
        ok: true,
        accepted: 1,
        offered: 1,
        first_seq: 0,
        accepted_from: result.acceptedFrom,
        stamp_tic: result.acceptedFrom,
        stamp_gametic: result.stampGametic ?? result.acceptedFrom,
        next_seq: result.acceptedFrom + 1,
        cursor: slot.cursor,
        gametic: slot.gametic,
        key_mask: decoded.keyMask,
        post_received_at_ms: postReceivedAtMs,
        tic_assigned_at_ms: result.ticAssignedAtMs ?? null,
        key_state: result.keyStateEvent ?? null,
        producer_next_cursor: result.producerNextCursor ?? null,
        pipeline: result.pipeline ?? slot.pipeline ?? null,
        latest_frame: slot.latestFrame ?? null,
      };
    }

    // V1 still carries an explicit range. Read the cursor before validating it;
    // its sequence can have gone stale while the page was signing or sending.
    await this.refreshSlot(slot);

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
      // The floor is the chain's committed cursor, floored by the highest
      // sequence this relayer has accepted. A tic below the cursor has been
      // read by the engine, so writing it is a no-op the chain cannot undo; and
      // a page aiming at the live staging window has to be allowed to rewrite
      // the tics it is still filling, or it drifts behind the engine at its own
      // tic rate and its keystrokes land in slots the game has already read.
      // Mock mode has no frontier and no drift, so it keeps the strict floor
      // that stops two racing batches depositing the same tics twice.
      nextSeq: this.sender.background ? Math.min(slot.nextSeq, slot.cursor) : slot.nextSeq,
      window: this.seqWindow,
    });
    if (!verdict.ok) {
      this.stats.inputsRejected += 1;
      const error = new Error(verdict.reason);
      error.status = 409;
      error.code = "batch_refused";
      // Where to resume. A client whose first_seq has fallen behind the cursor
      // cannot fix that by retrying the same batch, so the refusal carries the
      // cursor, the resume point and the frontier rather than only prose.
      error.detail = {
        first_seq: decoded.firstSeq,
        next_seq: slot.nextSeq,
        cursor: slot.cursor,
        staging_frontier: slot.stagingFrontier ?? null,
      };
      throw error;
    }

    // Backpressure. A player holding a key offers 8 tics per animation frame,
    // which is 480 tics/s against an engine that consumes about 35. Staging all
    // of it would work and would be a trap: the queue drains at the engine's
    // rate, so the keystroke that started it would be seconds behind the one
    // that ended it, and that gap grows through a session. So a batch is
    // accepted only as far as the sender says it can stage soon, and the page
    // re-offers the rest with whatever the keys are by then. Each sequence is
    // written exactly once, by the batch that was accepted for it.
    let commands = decoded.commands;
    let accepted = decoded.count;
    let frontier = null;
    if (this.sender.background) {
      frontier = await this.sender.stagingFrontier();
      // A page aims ahead of the producer by its own round trip, so a batch
      // routinely lands above the frontier: the producer reads the command
      // stream by absolute sequence and reaches those tics as its in-flight
      // packets land. What is refused is a batch so far ahead that it is a
      // backlog rather than a keystroke, which is the R8l5 failure this check
      // was written for and still is, one lead further out.
      const ceiling = frontier + this.inputLeadTics;
      const room = ceiling - decoded.firstSeq;
      if (room <= 0) {
        this.stats.inputsRejected += 1;
        const error = new Error(
          `input window is full: ${decoded.firstSeq} is ${decoded.firstSeq - ceiling} tics past the ` +
            `staging frontier ${frontier} plus the ${this.inputLeadTics}-tic lead`,
        );
        error.status = 409;
        error.code = "input_window_full";
        error.detail = {
          first_seq: decoded.firstSeq,
          next_seq: slot.nextSeq,
          cursor: slot.cursor,
          staging_frontier: frontier,
        };
        throw error;
      }
      if (room < accepted) {
        accepted = room;
        commands = decoded.commands.subarray(0, accepted * COMMAND_BYTES);
        this.log(`slot ${slot.slot} accepted ${accepted} of ${decoded.count} tics; the rest waits for the frontier`);
      }
    }

    let result;
    try {
      result = await this.sender.deposit({slot: slot.label, firstSeq: decoded.firstSeq, commands});
      // The producer's own bound is the last word on how much of the batch is
      // still writable, and it can be tighter than the frontier this method
      // trimmed against: the producer is the side that reads the command
      // stream, so its staging limit is where a tic stops being the page's to
      // choose. Trim again rather than let a short deposit throw.
      accepted = result.accepted;
      commands = decoded.commands.subarray(0, accepted * COMMAND_BYTES);
    } catch (error) {
      // A refusal from the pipeline is a resume point, not a server fault: it
      // names the sequence to write next. Without the 409 and the detail the
      // page sees a 500 and can only retry the same batch, which is the
      // accepted-and-dropped input path all over again.
      this.stats.inputsRejected += 1;
      const refusal = new Error(error.message);
      refusal.status = 409;
      refusal.code = error.code === "pipeline_refused" ? "input_stale" : (error.code ?? "batch_refused");
      // The pipeline's refusal detail: where to resume, and the producer's own
      // two cursors, which are the numbers a page aims with.
      const resume = error.detail ?? {};
      refusal.detail = {
        first_seq: decoded.firstSeq,
        next_seq: resume.next_seq ?? slot.nextSeq,
        cursor: resume.cursor ?? slot.cursor,
        staging_frontier: resume.staging_frontier ?? slot.stagingFrontier ?? null,
        producer_generated_cursor: resume.producer_generated_cursor ?? null,
        producer_stage_limit: resume.producer_stage_limit ?? null,
      };
      throw refusal;
    }
    // A background sender is already advancing and rendering on its own
    // schedule, so a batch does not drive either. It reports where the chain
    // has got to, which is what the page's status line wants.
    const advanced = this.sender.background
      ? await this.sender.advance({slot: slot.label, maxTics: 0, expectedCursor: null})
      : await this.sender.advance({slot: slot.label, maxTics: 0, expectedCursor: slot.cursor});

    slot.nextSeq = Math.max(slot.nextSeq, decoded.firstSeq + accepted);
    slot.cursor = advanced.cursor;
    slot.gametic = advanced.gametic;
    slot.ticsLanded += advanced.tics;
    this.stats.inputsAccepted += accepted;
    this.stats.ticsLanded += advanced.tics;

    // The first batch always renders, so a joining player sees a frame
    // immediately rather than waiting out the cadence. With a background
    // sender the render loop is already running, so there is nothing to force:
    // the newest frame it knows about is recorded instead, which is what the
    // page's frame line shows.
    if (this.sender.background) {
      try {
        const frame = await this.sender.renderFrame({slot: slot.label, bufferId: 0});
        if (frame.buffer !== undefined) slot.latestFrame = frame;
        if (frame.signature && frame.signature !== slot.lastFrameSignature) {
          slot.lastFrameSignature = frame.signature;
          slot.framesLanded += 1;
        }
      } catch {
        // No frame has landed yet. The tics are on chain either way.
      }
    } else if (slot.framesLanded === 0 || advanced.tics >= this.ticsPerFrame) {
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

    slot.producerNextCursor = result.producerNextCursor ?? slot.producerNextCursor ?? null;
    slot.pipeline = result.pipeline ?? slot.pipeline ?? null;

    return {
      ok: true,
      accepted,
      offered: decoded.count,
      first_seq: decoded.firstSeq,
      // The first sequence the producer actually took, which is not the one
      // offered when the head of the batch was already on chain. The page aims
      // its next batch from this.
      accepted_from: result.firstSeq,
      next_seq: slot.nextSeq,
      cursor: slot.cursor,
      gametic: slot.gametic,
      staging_frontier: frontier,
      producer_generated_cursor: result.producerGeneratedCursor ?? null,
      producer_next_cursor: result.producerNextCursor ?? null,
      producer_stage_limit: result.producerStageLimit ?? null,
      queued_above_stage_limit: result.queuedAboveStageLimit ?? null,
      trimmed_from_seq: result.trimmedFromSeq ?? null,
      pipeline: result.pipeline ?? slot.pipeline ?? null,
      tics: advanced.tics,
      deposit_signature: result.signature,
      advance_signature: advanced.signature,
      // The frame pointer rides on the input answer too: a held key is the
      // moment the page most wants to be reading the buffer that is not being
      // written, and the status poll is up to two seconds behind.
      latest_frame: slot.latestFrame ?? null,
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
    if (this.sender.background) {
      const error = new Error(
        "in chain mode the frame pipeline writes the strip accounts and the viewer reads them " +
          "from the network; the relayer holds no copy of the bytes",
      );
      error.status = 501;
      error.code = "no_frame_bytes_in_chain_mode";
      throw error;
    }
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
  // waiting for the next batch. A background sender renders on its own
  // schedule, so this reports the newest frame rather than asking for a new
  // one: forcing a render here would spend a frame's compute on a client that
  // is about to be sent the newest one anyway.
  async renderNow(sessionId) {
    const session = this.getSession(sessionId);
    const slot = session.slot;
    const frame = await this.sender.renderFrame({slot: slot.label, bufferId: 0});
    if (frame.signature && frame.signature !== slot.lastFrameSignature) {
      slot.lastFrameSignature = frame.signature;
      slot.framesLanded += 1;
    }
    if (!this.sender.background) {
      // Only a forced render is this relayer's own work; a background sender's
      // frames are its render loop's, and counting them here would say the
      // relayer painted them.
      slot.framesLanded += 1;
      this.stats.framesRendered += 1;
    }
    return {ok: true, cursor: frame.cursor, gametic: frame.gametic, signature: frame.signature, forced: !this.sender.background};
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
    // The push path's own counters. `frames_seen` is every frame the pipeline
    // announced, `frames_pushed` the ones that carried bytes, and
    // `subscribers` how many pages are painting from the stream right now. A
    // `frames_seen` that has stopped growing while `frames_pushed` has not is
    // the shape of a dead push, and it is only visible as a number.
    frame_push: {
      frames_seen: relayer.frameStreamSeen,
      frames_pushed: relayer.frameStream.size >= 0 ? relayer.frameStreamSeen - relayer.framesWithoutBytes : 0,
      frames_without_bytes: relayer.framesWithoutBytes,
      held: relayer.frameStream.size,
      subscribers: relayer.subscriptions.size,
    },
    public: relayer.publicMode ? relayer.publicMode.state() : null,
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
  {method: "POST", pattern: /^\/v1\/public\/join$/, handler: async (relayer, request) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    return relayer.publicMode.join(await readJson(request));
  }},
  {method: "GET", pattern: /^\/v1\/public\/state$/, handler: async (relayer, request) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    return relayer.publicMode.state(new URL(request.url, "http://relayer.invalid").searchParams.get("token"));
  }},
  {method: "POST", pattern: /^\/v1\/public\/heartbeat$/, handler: async (relayer, request) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    return relayer.publicMode.heartbeat((await readJson(request)).token);
  }},
  {method: "POST", pattern: /^\/v1\/public\/offer\/accept$/, handler: async (relayer, request) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    return relayer.publicMode.acceptOffer((await readJson(request)).token);
  }},
  {method: "POST", pattern: /^\/v1\/public\/input$/, handler: async (relayer, request) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    const body = await readJson(request);
    const message = fromBase64(body.message, "message");
    const signature = fromBase64(body.signature, "signature");
    if (message.length > 64 + MAX_COMMANDS_PER_BATCH * COMMAND_BYTES) {
      throw new PublicModeError("message is longer than a full batch", 400, "message_too_long");
    }
    const client = relayer.publicMode.assertCanInput(body.token, body.publicKey);
    if (!relayer.publicSession) throw new PublicModeError("the shared game session is not ready", 503, "game_not_ready");
    return relayer.postInput(relayer.publicSession.id, {message, signature, publicKey: client.publicKey}, {publicToken: body.token});
  }},
  {method: "DELETE", pattern: /^\/v1\/public\/leave$/, handler: async (relayer, request) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    return relayer.publicMode.leave(new URL(request.url, "http://relayer.invalid").searchParams.get("token"));
  }},
  {method: "GET", pattern: /^\/v1\/public\/palette$/, handler: async (relayer) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    return {palette: await relayer.publicMode.palette()};
  }},
  {method: "GET", pattern: /^\/v1\/public\/stream$/, handler: async (relayer, request, _params, response, {corsOrigin}) => {
    if (!relayer.publicMode) throw new PublicModeError("public mode is disabled", 404, "public_mode_disabled");
    const url = new URL(request.url, "http://relayer.invalid");
    const token = url.searchParams.get("token");
    relayer.publicMode.state(token);
    const sinceText = url.searchParams.get("since");
    const since = sinceText === null ? null : Number(sinceText);
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-store",
      connection: "keep-alive",
      "x-accel-buffering": "no",
      ...corsHeaders(corsOrigin),
    });
    response.write("retry: 1000\n\n");
    const frames = relayer.subscribeFrames({since});
    const writeState = () => response.write(`event: state\ndata: ${JSON.stringify(relayer.publicMode.state(token))}\n\n`);
    const writeFrame = (record) => response.write(`event: frame\nid: ${record.frame_index ?? ""}\ndata: ${JSON.stringify(record)}\n\n`);
    const stopState = relayer.publicMode.subscribe(token, writeState);
    const stopFrame = frames.add(writeFrame);
    writeState();
    for (const record of frames.pending) writeFrame(record);
    const keepAlive = setInterval(() => response.write(": keep-alive\n\n"), 15_000);
    const close = () => {
      clearInterval(keepAlive);
      stopState();
      stopFrame();
      frames.close();
    };
    response.on("close", close);
    request.on("error", close);
    return RAW_RESPONSE;
  }, stream: true},
  {method: "POST", pattern: /^\/v1\/sessions$/, handler: async (relayer, request) => {
    if (relayer.publicMode) throw new PublicModeError("use the public lobby", 404, "public_mode_only");
    const body = await readJson(request);
    return relayer.createSession({publicKey: body.publicKey ?? null, requestedSlot: body.slot ?? null});
  }},
  {method: "GET", pattern: /^\/v1\/sessions\/([^/]+)$/, handler: async (relayer, request, [sessionId]) => {
    const session = relayer.getSession(decodeURIComponent(sessionId));
    // A status poll is the only thing a player with no keys down is sending, so
    // it is the only thing keeping the reported cursor fresh. Without this the
    // panel shows the cursor as of the last keystroke, which reads as a hung
    // session while the engine runs on.
    await relayer.refreshSlot(session.slot);
    return {id: session.id, slot: session.slot.slot, state: session.state, ...relayer.status(session.slot)};
  }},
  {method: "POST", pattern: /^\/v1\/sessions\/([^/]+)\/input$/, handler: async (relayer, request, [sessionId]) => {
    if (relayer.publicMode) throw new PublicModeError("use the public lobby input route", 403, "public_mode_only");
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
  // The frame push. Server-sent events rather than a WebSocket, for one
  // reason that matters here: a frame is 65 KB of base64 arriving several times
  // a second, one way, and a browser reconnects an EventSource by itself. A
  // socket would need its own reconnect, its own ordering and its own framing
  // for no gain. The oldest frame the stream will hold is four, so a page that
  // falls behind loses frames rather than buffering without bound -- the same
  // choice a live viewer makes when it skips to the newest picture.
  {method: "GET", pattern: /^\/v1\/sessions\/([^/]+)\/frames$/, handler: async (relayer, request, [sessionId], response) => {
    const id = decodeURIComponent(sessionId);
    // A session is required, and looked up, so a push stream is scoped to a
    // player the relayer has actually admitted rather than open to anything
    // that can reach the port.
    relayer.getSession(id);
    const url = new URL(request.url, "http://relayer.invalid");
    const since = url.searchParams.get("since");
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-store",
      connection: "keep-alive",
      "x-accel-buffering": "no",
      ...corsHeaders(request.headers.origin ?? "*"),
    });
    // Without this a proxy waits for the buffer to fill before forwarding.
    response.write("retry: 1000\n\n");
    const subscription = relayer.subscribeFrames({since: since === null ? null : Number(since)});
    const write = (record) => {
      response.write(`event: frame\nid: ${record.frame_index ?? ""}\ndata: ${JSON.stringify(record)}\n\n`);
    };
    for (const record of subscription.pending) write(record);
    // One keep-alive comment every 15 s: an idle tunnel is a closed tunnel, and
    // a stream that only looks alive when a frame lands cannot tell a quiet
    // chain from a dead pipe.
    const keepAlive = setInterval(() => response.write(": keep-alive\n\n"), 15_000);
    const stopListening = subscription.add(write);
    const close = () => {
      clearInterval(keepAlive);
      stopListening();
      subscription.close();
    };
    response.on("close", close);
    request.on("error", close);
    return RAW_RESPONSE;
  }, stream: true},
  {method: "DELETE", pattern: /^\/v1\/sessions\/([^/]+)$/, handler: async (relayer, request, [sessionId]) =>
    relayer.endSession(decodeURIComponent(sessionId)),
  },
  {method: "GET", pattern: /^\/v1\/slots$/, handler: async (relayer) => {
    for (const slot of relayer.pool.slots) await relayer.refreshSlot(slot);
    return {pool: relayer.pool.summary(), slots: relayer.pool.list()};
  }},
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
    // One request at a time, so a deposit and an advance cannot interleave. A
    // stream route is the exception and must be one: it never returns, so
    // queueing it would hold every later request behind a page that is watching
    // a picture. Streams do not touch the slot, so there is nothing to
    // interleave.
    const dispatch = (work) => (route.stream ? work() : relayer.serialise(work));
    void dispatch(async () => {
      try {
        const payload = await route.handler(relayer, request, params, response, {corsOrigin});
        if (payload === RAW_RESPONSE) {
          onLog?.(`${request.method} ${url.pathname} stream`);
          return;
        }
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
//
// node stunts/doom-relayer/server.mjs --mode chain --port 8787 \
//   --keys-dir /home/ec2-user/doom-keys/r8l4-fresh \
//   --payer-key /home/ec2-user/doom-keys/tokyo-testnet-payer.json \
//   --python /home/ec2-user/basanos-main/.venv/bin/python \
//   --image-sha256 67038f62... --seconds 900 --strip-rate 16
//
// Chain mode holds one long-lived pipeline process, which is what keeps the
// cursor binding between player batches. It is the one thing in this file that
// is allowed to be long-running, and it is stopped with SIGINT/SIGTERM, which
// asks the pipeline to write its receipt and exit.
function isMain() {
  return process.argv[1] && import.meta.url === `file://${process.argv[1]}`;
}

// Chain-mode flags, named here so a mistyped one is a startup error rather
// than a silently default pipeline: every one of these changes what the sender
// does on chain, and a default that looks like a choice is worse than a refusal.
const CHAIN_FLAGS = new Set([
  "keys-dir", "payer-key", "python", "script", "run-dir", "rpc-a", "rpc-b", "rpc-c",
  "send-pacer-rate",
  "image-sha256", "image-bytes", "seconds", "strip-rate", "render-buffers",
  "estimate-horizon-seconds", "payer-baseline-lamports", "public-state", "public-max-viewers",
  "public-rollover-threshold", "public-lock-owner", "stage-cap-script", "r14-keys-dir",
  "advance-tics", "advance-pipeline-depth", "sweep-rate", "input-tics-per-second",
  "input-stage-lead-tics", "start-cursor", "spend-cap-fogo", "payer-reserve-fogo",
  "render", "reset-start-account", "frame-deadline-seconds", "rpc-lag-drop-slots",
  "rpc-lag-check-seconds", "min-cursor-step-tics", "render-pool-workers",
  "batch-render-sends",
]);

// Chain-mode flags that are switches rather than values. They are listed rather
// than left to the generic branch, because the generic branch consumes the token
// after the flag as its value: `--pre-read-snapshot --overlap-readback` would set
// the first to the *text* of the second and then skip it, so the second would
// never reach the pipeline and the session would quietly run the serial path
// while the runbook said otherwise.
const BOOLEAN_CHAIN_FLAGS = new Set(["pre-read-snapshot", "overlap-readback", "strips-behind-snapshot", "atomic-live-inputs", "batch-render-sends", "start-paused", "public-mode"]);

export function parseArgs(argv) {
  const args = new Map();
  for (let i = 2; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith("--")) continue;
    const name = token.slice(2);
    if (BOOLEAN_CHAIN_FLAGS.has(name)) {
      args.set(name, "1");
    } else if (CHAIN_FLAGS.has(name)) {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) {
        throw new Error(`--${name} needs a value`);
      }
      args.set(name, value);
      i += 1;
    } else {
      args.set(name, argv[i + 1]);
      i += 1;
    }
  }
  return args;
}

if (isMain()) {
  const args = parseArgs(process.argv);
  const mode = args.get("mode") ?? "mock";
  const port = Number(args.get("port") ?? 8787);
  const network = args.get("network") ?? DEFAULT_NETWORK;
  const spendCapFogo = Number(args.get("spend-cap-fogo") ?? (args.has("public-mode") ? 300 : 10));
  requireNetwork(network, ["doom.program"]);
  const logger = (...parts) => console.error(new Date().toISOString(), ...parts);
  const publicStatePath = args.get("public-state") ?? null;
  let publicRun = {};
  if (args.get("public-mode") && publicStatePath) {
    try { publicRun = JSON.parse(readFileSync(publicStatePath, "utf8")); } catch { publicRun = {}; }
  }
  const sender = createSender({
    mode,
    logger,
    keysDir: args.get("keys-dir") ?? null,
    payerKey: args.get("payer-key") ?? null,
    python: args.get("python") ?? undefined,
    script: args.get("script") ?? undefined,
    runDir: args.get("run-dir") ?? undefined,
    rpcA: args.get("rpc-a") ?? undefined,
    rpcB: args.get("rpc-b") ?? undefined,
    rpcC: args.get("rpc-c") ?? undefined,
    sendPacerRate: args.get("send-pacer-rate") ?? undefined,
    publicMode: args.has("public-mode"),
    publicRolloverThreshold: args.get("public-rollover-threshold") ? Number(args.get("public-rollover-threshold")) : undefined,
    publicMaintenanceLockOwner: args.get("public-lock-owner") ?? undefined,
    stageCapScript: args.get("stage-cap-script") ?? undefined,
    r14KeysDir: args.get("r14-keys-dir") ?? undefined,
    imageSha256: args.get("image-sha256") ?? undefined,
    imageBytes: args.get("image-bytes") ? Number(args.get("image-bytes")) : undefined,
    startPaused: args.has("start-paused") || args.has("public-mode"),
    estimateHorizonSeconds: args.get("estimate-horizon-seconds") ? Number(args.get("estimate-horizon-seconds")) : undefined,
    payerBaselineLamports: publicRun.payer_baseline_lamports ?? (args.get("payer-baseline-lamports") ? Number(args.get("payer-baseline-lamports")) : null),
    seconds: args.get("seconds") ? Number(args.get("seconds")) : undefined,
    stripRate: args.get("strip-rate") ? Number(args.get("strip-rate")) : undefined,
    renderBuffers: args.get("render-buffers") ? Number(args.get("render-buffers")) : undefined,
    advanceTics: args.get("advance-tics") ? Number(args.get("advance-tics")) : undefined,
    advancePipelineDepth: args.get("advance-pipeline-depth") ? Number(args.get("advance-pipeline-depth")) : undefined,
    sweepRate: args.get("sweep-rate") ? Number(args.get("sweep-rate")) : undefined,
    inputTicsPerSecond: args.get("input-tics-per-second") ? Number(args.get("input-tics-per-second")) : undefined,
    inputStageLeadTics: args.get("input-stage-lead-tics") ? Number(args.get("input-stage-lead-tics")) : undefined,
    startCursor: args.get("start-cursor") ? Number(args.get("start-cursor")) : undefined,
    spendCapFogo,
    payerReserveFogo: args.get("payer-reserve-fogo") ? Number(args.get("payer-reserve-fogo")) : undefined,
    resetStartAccount: args.get("reset-start-account") ?? undefined,
    atomicLiveInputs: args.get("atomic-live-inputs") != null,
    frameDeadlineSeconds: args.get("frame-deadline-seconds") ? Number(args.get("frame-deadline-seconds")) : undefined,
    minCursorStepTics: args.get("min-cursor-step-tics") ? Number(args.get("min-cursor-step-tics")) : undefined,
    renderPoolWorkers: args.get("render-pool-workers") != null ? Number(args.get("render-pool-workers")) : undefined,
    stripCuLimit: args.get("strip-cu-limit") ? Number(args.get("strip-cu-limit")) : undefined,
    statusBatch: args.get("status-batch") ? Number(args.get("status-batch")) : undefined,
    statusMinInterval: args.get("status-min-interval") !== null ? Number(args.get("status-min-interval")) : undefined,
    rpcLagDropSlots: args.get("rpc-lag-drop-slots") ? Number(args.get("rpc-lag-drop-slots")) : undefined,
    rpcLagCheckSeconds: args.get("rpc-lag-check-seconds") ? Number(args.get("rpc-lag-check-seconds")) : undefined,
    preReadSnapshot: args.get("pre-read-snapshot") != null,
    overlapReadback: args.get("overlap-readback") != null,
    // The early-strip experiment. Off by default; the receipt carries the
    // refusal rate, which is the number that decides whether it can ever be on.
    stripsBehindSnapshot: args.get("strips-behind-snapshot") != null,
    batchRenderSends: args.get("batch-render-sends") != null,
  });
  const relayer = createRelayer({
    network,
    mode,
    slotCount: Number(args.get("slots") ?? 2),
    logger,
    sender,
    advanceTics: args.get("advance-tics") ? Number(args.get("advance-tics")) : 4,
    inputLeadTics: args.get("input-lead-tics") ? Number(args.get("input-lead-tics")) : 96,
    publicMode: args.has("public-mode"),
    publicStatePath,
    publicMaxViewers: args.get("public-max-viewers") ? Number(args.get("public-max-viewers")) : 200,
    publicSpendCapFogo: spendCapFogo,
  });
  const corsOrigin = args.get("cors-origin") ?? "*";
  const server = createServer(relayer, {onLog: (line) => logger(line), corsOrigin});
  // The listen address is named rather than left to node, because the relayer
  // is reached through an SSH tunnel and must not be on every interface by
  // accident.
  const host = args.get("host") ?? "127.0.0.1";
  let stopping = false;
  let rolloverFailureStopping = false;
  if (relayer.publicMode) {
    relayer.publicMode.onRolloverFailure = async () => {
      if (rolloverFailureStopping) return;
      rolloverFailureStopping = true;
      const owner = args.get("public-lock-owner") ?? "";
      const lockDir = "/tmp/doom-play-maintenance.lock";
      // Let the SSE stopped state reach both browsers before dropping streams.
      await new Promise((resolve) => setTimeout(resolve, 750));
      relayer.publicMode.stop();
      try { await relayer.sender.close(); } catch (error) { logger(`failed rollover sender close: ${error.message}`); }
      server.close();
      server.closeAllConnections?.();
      try {
        if (readFileSync(`${lockDir}/owner`, "utf8").trim() === owner && owner.startsWith("doom-public:")) {
          for (const name of ["owner", "run_name", "created_utc"]) rmSync(`${lockDir}/${name}`, {force: true});
          rmdirSync(lockDir);
        } else {
          logger("failed rollover left the maintenance lock untouched because its owner changed");
        }
      } catch (error) {
        logger(`failed rollover could not release its maintenance lock: ${error.message}`);
      }
      process.exit(1);
    };
  }
  const shutdown = async (signal) => {
    if (stopping) return;
    stopping = true;
    logger(`${signal}: stopping; the pipeline writes its receipt before it exits`);
    relayer.publicMode?.stop();
    server.close();
    try {
      await relayer.sender.close();
    } catch (error) {
      logger(`sender close failed: ${error.message}`);
    }
    process.exit(0);
  };
  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => void shutdown(signal));
  }
  try {
    await relayer.init();
  } catch (error) {
    logger(`could not start the ${mode} sender: ${error.message}`);
    process.exit(1);
  }
  server.listen(port, host, () => {
    logger(`doom relayer ${API_VERSION} on http://${host}:${port} mode=${mode} network=${network} cors=${corsOrigin}`);
    logger(`pool: ${JSON.stringify(relayer.pool.summary())}`);
    if (mode === "chain") {
      logger(`strip rate ${args.get("strip-rate") ?? 16}/s, depth ${args.get("advance-pipeline-depth") ?? 4}, ` +
        `stop with SIGINT or SIGTERM, or kill this process and the pipeline with it`);
    }
  });
}
