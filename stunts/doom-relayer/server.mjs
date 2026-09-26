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
import {SlotPool, PoolError, SLOT_STATE, makeSlot} from "./pool.mjs";
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
    advanceTics = 4,
    inputLeadTics = 96,
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
    this.startedAt = Date.now();
    this.stats = {sessionsCreated: 0, sessionsEnded: 0, inputsAccepted: 0, inputsRejected: 0, ticsLanded: 0, framesRendered: 0};
    this.promise = Promise.resolve();
    // Mock mode is synchronous and self-contained: the synthetic slots are what
    // the page is developed against. Chain mode has to ask a process what
    // context it is driving, so that waits for init().
    if (!this.pool.size && !this.sender.background) this.seedSlots(slotCount);
  }

  // A background sender is already driving a provisioned context, so the pool
  // is seeded from what it reports rather than from synthetic mock addresses.
  // In mock mode the constructor has already done it and this is a no-op.
  async init() {
    if (this.pool.size || !this.sender.background) return this;
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
    // A context resumes where it stopped, so the player's first batch is
    // staged at the chain's cursor, not at zero.
    record.nextSeq = slot.nextSeq ?? 0;
    this.log(`slot 0 is context ${record.accounts.context} at cursor ${record.cursor} of ${record.inputCapacity}`);
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
    // Read the cursor before judging the batch, not after staging it. The page's
    // own next_seq is only as fresh as its last answer, and the engine runs at
    // about 35 tics/s, so between two player batches the chain can consume
    // hundreds of tics the page still believes it owns.
    await this.refreshSlot(slot);

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

    return {
      ok: true,
      accepted,
      offered: decoded.count,
      first_seq: decoded.firstSeq,
      next_seq: slot.nextSeq,
      cursor: slot.cursor,
      gametic: slot.gametic,
      staging_frontier: frontier,
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
    // A status poll is the only thing a player with no keys down is sending, so
    // it is the only thing keeping the reported cursor fresh. Without this the
    // panel shows the cursor as of the last keystroke, which reads as a hung
    // session while the engine runs on.
    await relayer.refreshSlot(session.slot);
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
  "keys-dir", "payer-key", "python", "script", "run-dir", "rpc-a", "rpc-b",
  "image-sha256", "image-bytes", "seconds", "strip-rate", "render-buffers",
  "advance-tics", "advance-pipeline-depth", "sweep-rate", "input-tics-per-second",
  "input-stage-lead-tics", "start-cursor", "spend-cap-fogo", "payer-reserve-fogo",
  "render", "reset-start-account", "frame-deadline-seconds", "rpc-lag-drop-slots",
  "rpc-lag-check-seconds",
]);

function parseArgs(argv) {
  const args = new Map();
  for (let i = 2; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith("--")) continue;
    const name = token.slice(2);
    if (CHAIN_FLAGS.has(name)) {
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
  requireNetwork(network, ["doom.program"]);
  const logger = (...parts) => console.error(new Date().toISOString(), ...parts);
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
    imageSha256: args.get("image-sha256") ?? undefined,
    imageBytes: args.get("image-bytes") ? Number(args.get("image-bytes")) : undefined,
    seconds: args.get("seconds") ? Number(args.get("seconds")) : undefined,
    stripRate: args.get("strip-rate") ? Number(args.get("strip-rate")) : undefined,
    renderBuffers: args.get("render-buffers") ? Number(args.get("render-buffers")) : undefined,
    advanceTics: args.get("advance-tics") ? Number(args.get("advance-tics")) : undefined,
    advancePipelineDepth: args.get("advance-pipeline-depth") ? Number(args.get("advance-pipeline-depth")) : undefined,
    sweepRate: args.get("sweep-rate") ? Number(args.get("sweep-rate")) : undefined,
    inputTicsPerSecond: args.get("input-tics-per-second") ? Number(args.get("input-tics-per-second")) : undefined,
    inputStageLeadTics: args.get("input-stage-lead-tics") ? Number(args.get("input-stage-lead-tics")) : undefined,
    startCursor: args.get("start-cursor") ? Number(args.get("start-cursor")) : undefined,
    spendCapFogo: args.get("spend-cap-fogo") ? Number(args.get("spend-cap-fogo")) : undefined,
    payerReserveFogo: args.get("payer-reserve-fogo") ? Number(args.get("payer-reserve-fogo")) : undefined,
    resetStartAccount: args.get("reset-start-account") ?? undefined,
    frameDeadlineSeconds: args.get("frame-deadline-seconds") ? Number(args.get("frame-deadline-seconds")) : undefined,
    rpcLagDropSlots: args.get("rpc-lag-drop-slots") ? Number(args.get("rpc-lag-drop-slots")) : undefined,
    rpcLagCheckSeconds: args.get("rpc-lag-check-seconds") ? Number(args.get("rpc-lag-check-seconds")) : undefined,
  });
  const relayer = createRelayer({
    network,
    mode,
    slotCount: Number(args.get("slots") ?? 2),
    logger,
    sender,
    advanceTics: args.get("advance-tics") ? Number(args.get("advance-tics")) : 4,
    inputLeadTics: args.get("input-lead-tics") ? Number(args.get("input-lead-tics")) : 96,
  });
  const corsOrigin = args.get("cors-origin") ?? "*";
  const server = createServer(relayer, {onLog: (line) => logger(line), corsOrigin});
  // The listen address is named rather than left to node, because the relayer
  // is reached through an SSH tunnel and must not be on every interface by
  // accident.
  const host = args.get("host") ?? "127.0.0.1";
  let stopping = false;
  const shutdown = async (signal) => {
    if (stopping) return;
    stopping = true;
    logger(`${signal}: stopping; the pipeline writes its receipt before it exits`);
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
