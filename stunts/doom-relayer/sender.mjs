// What the relayer needs from the chain, and what it has instead.
//
// The relayer's job on chain is small and specific: for a batch of signed
// inputs, deposit the commands into a slot's SIM2 input buffer, advance the
// tics, and render the frame into the slot's render buffer. That is the whole
// ChainSender contract below.
//
// Two implementations ship here:
//
//   MockSender     walks a virtual cursor and synthesises real, digest-valid
//                  strip accounts. No chain access at all. This is what the
//                  page is exercised against locally, and what the API tests
//                  run against.
//
//   createPipelineSender
//                  the binding to scripts/doom_r8b_tokyo_pipeline.py. It is
//                  deliberately not wired: another worker is changing that
//                  sender's interface right now, and this dispatch must not
//                  edit it. The contract is written out so the binding is one
//                  function to fill in once that interface settles.
//
// Nothing in this file signs or sends anything. MockSender never touches the
// network, and createPipelineSender refuses to start until it is filled in.

import {STRIP_ACCOUNT_BYTES, STRIP_HEADER_BYTES, STRIP_PIXELS_BYTES, stripDigest, toHex} from "../../web/doom-play/strip.js";

export class SenderError extends Error {
  constructor(message, code = "sender_error", detail = {}) {
    super(message);
    this.name = "SenderError";
    this.code = code;
    Object.assign(this, detail);
  }
}

// The contract, as documentation the mock and any future binding both satisfy.
// Every method is async and every method may throw SenderError.
export const CHAIN_SENDER_CONTRACT = {
  // Provision a fresh slot: create and initialise a context, its SIM2 meta and
  // input buffer, its state and mirror, and a render buffer. Returns the slot
  // record that pool.mjs's makeSlot wants. Costs about 73 FOGO of rent, so a
  // relayer does this out of band, not per player.
  async provisionSlot({network, label}) {
    throw new SenderError("not implemented", "not_implemented");
  },

  // Deposit up to 64 four-byte commands at firstSeq. This is SIM2 DEPOSIT.
  async deposit({slot, firstSeq, commands}) {
    throw new SenderError("not implemented", "not_implemented");
  },

  // Advance up to 64 tics. This is SIM2 ADVANCE, optionally bound to an
  // expected cursor so an out-of-order sweep is refused atomically.
  // Returns {cursor, gametic, tics}.
  async advance({slot, maxTics, expectedCursor}) {
    throw new SenderError("not implemented", "not_implemented");
  },

  // Render the current state into the slot's render buffer: one snapshot
  // transaction and eight strip transactions per frame. Returns
  // {cursor, gametic, signature}.
  async renderFrame({slot, bufferId}) {
    throw new SenderError("not implemented", "not_implemented");
  },

  // What the chain currently says, for session status and for reconciling a
  // painted frame at the confirmed commitment. Read-only.
  async readProgress({slot}) {
    throw new SenderError("not implemented", "not_implemented");
  },

  async close() {},
};

// --------------------------------------------------------------- mock

// A deterministic little scene so a mock frame is visibly a frame and not a
// flat fill: a horizon, a floor grid, and a marker that walks with the cursor.
function synthPixels(stripId, cursor) {
  const pixels = new Uint8Array(STRIP_PIXELS_BYTES);
  const globalX = (x) => stripId * 40 + x;
  for (let y = 0; y < 200; y += 1) {
    for (let x = 0; x < 40; x += 1) {
      const gx = globalX(x);
      let index;
      if (y < 96) {
        // Sky, banded like a Doom sky texture.
        index = 104 + ((y >> 3) % 4) * 4 + (gx >> 5);
      } else if (y < 104) {
        index = 4 + ((gx + (cursor >> 2)) % 16 < 8 ? 0 : 3); // a wall band
      } else {
        // Floor, with a grid that scrolls so movement is visible.
        const depth = y - 104;
        const step = depth < 24 ? 1 : depth < 64 ? 4 : 16;
        const shade = depth < 32 ? 96 : depth < 96 ? 100 : 104;
        const cellX = Math.floor(gx / step);
        const cellY = Math.floor(y / step);
        const checker = (cellX + cellY) % 2 === 0 ? 0 : 5;
        index = shade + checker;
      }
      pixels[y * 40 + x] = index & 0xff;
    }
  }
  return pixels;
}

export class MockSender {
  constructor({ticsPerAdvance = 8, framesPerAdvance = 1, seed = "doom-play-mock"} = {}) {
    this.mode = "mock";
    this.ticsPerAdvance = ticsPerAdvance;
    this.framesPerAdvance = framesPerAdvance;
    this.seed = seed;
    this.state = new Map();
    this.calls = [];
    this.sent = 0;
  }

  #slot(key) {
    const record = this.state.get(key);
    if (!record) throw new SenderError(`mock has no slot ${key}`, "unknown_slot");
    return record;
  }

  async provisionSlot({network, label, slot: slotIndex, accounts = null, inputCapacity = 6016}) {
    const key = label ?? `mock-${this.state.size}`;
    if (this.state.has(key)) throw new SenderError(`mock slot ${key} exists`, "duplicate_slot");
    // Deterministic, obviously-fake addresses. A mock must never be mistaken
    // for a real deployment, so they are derived from a fixed label and are
    // not valid base58 account addresses on any network.
    const address = (role, index = 0) => `MOCK-${key}-${role}${index ? `-${index}` : ""}`;
    const record = {
      key,
      network,
      label,
      accounts: accounts ?? {
        context: address("context"),
        meta: address("meta"),
        inputBuffer: address("buffer"),
        state: address("state"),
        mirror: address("mirror"),
        authority: address("authority"),
        snapshot: address("snapshot"),
      },
      strips: Array.from({length: 8}, (_, i) => address("strip", i)),
      inputCapacity,
      cursor: 0,
      gametic: 0,
      firstTic: 0,
      deposits: [],
      advances: 0,
      frames: 0,
      lastSignature: null,
      halted: false,
    };
    this.state.set(key, record);
    return {
      slot: slotIndex ?? this.state.size - 1,
      label: key,
      ...record.accounts,
      strips: record.strips,
      inputCapacity,
    };
  }

  async deposit({slot, firstSeq, commands}) {
    const record = this.#slot(slot);
    this.calls.push({op: "deposit", slot, firstSeq, bytes: commands.length});
    this.sent += 1;
    if (record.halted) throw new SenderError(`slot ${slot} has halted`, "halted");
    if (firstSeq + commands.length / 4 > record.inputCapacity) {
      throw new SenderError(
        `slot ${slot} has ${record.inputCapacity} input slots and this batch ends at ${firstSeq + commands.length / 4}`,
        "capacity",
        {capacity: record.inputCapacity},
      );
    }
    // The program refuses a batch that leaves a gap in the staged slots.
    if (firstSeq > record.cursor) {
      throw new SenderError(`slot ${slot} is at ${record.cursor}, deposit starts at ${firstSeq}`, "slot_gap", {
        cursor: record.cursor,
        firstSeq,
      });
    }
    for (let i = 0; i < commands.length; i += 4) {
      if (commands[i + 3] & 0x80) {
        throw new SenderError(`BT_SPECIAL at sequence ${firstSeq + i / 4}`, "command_refused");
      }
    }
    record.deposits.push({firstSeq, bytes: Buffer.from(commands).toString("hex")});
    return {accepted: commands.length / 4, firstSeq, signature: `mock-deposit-${this.sent}`};
  }

  async advance({slot, maxTics = 0, expectedCursor = null}) {
    const record = this.#slot(slot);
    this.calls.push({op: "advance", slot, maxTics, expectedCursor});
    this.sent += 1;
    if (record.halted) throw new SenderError(`slot ${slot} has halted`, "halted");
    if (expectedCursor !== null && expectedCursor !== record.cursor) {
      throw new SenderError(`slot ${slot} is at cursor ${record.cursor}, not ${expectedCursor}`, "cursor_mismatch", {
        cursor: record.cursor,
        expectedCursor,
      });
    }
    const budget = record.inputCapacity - record.cursor;
    if (budget <= 0) throw new SenderError(`slot ${slot} is out of input capacity`, "capacity");
    const requested = maxTics === 0 ? this.ticsPerAdvance : maxTics;
    const tics = Math.min(requested, this.ticsPerAdvance, 64, budget);
    if (tics <= 0) throw new SenderError(`slot ${slot} cannot advance`, "no_tics");
    record.cursor += tics;
    record.gametic += tics;
    record.advances += 1;
    return {cursor: record.cursor, gametic: record.gametic, tics, signature: `mock-advance-${this.sent}`};
  }

  // Builds eight real strip accounts, digests and all, so the viewer's decode
  // path is genuinely exercised rather than stubbed.
  async renderFrame({slot, bufferId = 0}) {
    const record = this.#slot(slot);
    this.calls.push({op: "renderFrame", slot, bufferId});
    this.sent += 1;
    const cursor = record.cursor;
    const gametic = record.gametic;
    const snapshotHash = new Uint8Array(32);
    const stateRoot = new Uint8Array(32);
    for (let i = 0; i < 32; i += 1) {
      snapshotHash[i] = (cursor * 7 + i * 13) & 0xff;
      stateRoot[i] = (gametic * 11 + i * 5) & 0xff;
    }
    const strips = [];
    for (let stripId = 0; stripId < 8; stripId += 1) {
      const pixels = synthPixels(stripId, cursor);
      const digest = await stripDigest({stripId, cursor, gametic, snapshotHash, pixels});
      const account = new Uint8Array(STRIP_ACCOUNT_BYTES);
      const view = new DataView(account.buffer);
      account.set(new TextEncoder().encode("DOOM-RS1"), 0);
      view.setUint32(8, 1, true);
      view.setUint32(12, STRIP_HEADER_BYTES, true);
      view.setUint32(16, stripId, true);
      view.setUint32(20, cursor, true);
      view.setUint32(24, gametic, true);
      view.setUint32(28, STRIP_PIXELS_BYTES, true);
      account.set(snapshotHash, 32);
      account.set(stateRoot, 64);
      account.set(digest, 96);
      account.set(pixels, STRIP_HEADER_BYTES);
      strips.push(account);
    }
    record.frames += 1;
    record.lastSignature = `mock-frame-${record.frames}-${cursor}`;
    return {
      cursor,
      gametic,
      snapshotHash: toHex(snapshotHash),
      stateRoot: toHex(stateRoot),
      signature: record.lastSignature,
      strips,
      bufferId,
    };
  }

  async readProgress({slot}) {
    const record = this.#slot(slot);
    return {cursor: record.cursor, gametic: record.gametic, capacity: record.inputCapacity, halted: record.halted};
  }

  // Stops a mock slot, standing in for the latched M_HALT that makes a real
  // context permanently dead. Used by the tests to check the surface.
  halt(slot, {capacity = null} = {}) {
    const record = this.#slot(slot);
    record.halted = true;
    if (capacity !== null) record.inputCapacity = capacity;
    return record;
  }

  // Strips as a JSON-friendly shape, for the mock transport.
  async readStrips({slot, bufferId = 0}) {
    const frame = await this.renderFrame({slot, bufferId});
    return frame.strips.map((bytes) => Buffer.from(bytes).toString("base64"));
  }

  async close() {}
}

// --------------------------------------------------------------- pipeline

// The interface the Python side has to expose, written out so it can be
// reviewed rather than guessed at. The relayer needs five things; the existing
// script already has four of them as functions and one that does not exist yet.
//
//   deposit        SIM2 DEPOSIT, firstSeq + up to 64 commands.  Already
//                  derivable from the buffer and authority in scripts/
//                  doom_sweep_testnet.py:1450 (deposit_ix).
//   advance        SIM2 ADVANCE, optionally expected-cursor bound.
//                  doom_sweep_testnet.py:1454 (advance_ix).
//   snapshot       RSN/3 SNAPSHOT-LIVE into the buffer's snapshot account.
//                  doom_r8b_tokyo_pipeline.py:973 (snapshot_instruction).
//   renderStrip    RENDER for one strip id.  doom_r8b_tokyo_pipeline.py:987
//                  (strip_instruction).
//   longLived      NEW. A way to keep one context advancing from an external
//                  source of commands. `run` today takes --seconds and a
//                  scripted command table (key_command, line 573), so it is a
//                  batch driver, not a server.
//
// Until (5) exists, a relayer cannot play: it would have to shell out to `run`
// per batch, which would restart the sender each time and lose the cursor
// binding that ADVANCE_EXPECTED_CURSOR exists to provide.
export const PIPELINE_REQUIREMENTS = {
  module: "scripts/doom_r8b_tokyo_pipeline.py",
  note: "do not edit that file from this dispatch; another worker owns it",
  operations: ["deposit", "advance", "snapshot", "renderStrip", "longLived"],
  existing: {
    deposit: "scripts/doom_sweep_testnet.py:1450 deposit_ix",
    advance: "scripts/doom_sweep_testnet.py:1454 advance_ix",
    snapshot: "scripts/doom_r8b_tokyo_pipeline.py:973 FramePipeline.snapshot_instruction",
    renderStrip: "scripts/doom_r8b_tokyo_pipeline.py:987 FramePipeline.strip_instruction",
  },
  missing: {
    longLived: "a command source the relayer can push signed batches into, with a stable cursor binding",
  },
  transport: "JSON lines on stdin and stdout, one request and one response per line, so the relayer can hold the process open for the life of a session",
  request: {op: "deposit", slot: "slot label", firstSeq: 0, commands: "base64"},
  response: {ok: true, signature: "base58", cursor: 0, gametic: 0, tics: 8},
};

export function createPipelineSender(options = {}) {
  throw new SenderError(
    "the chain sender is not wired. " +
      `It needs ${PIPELINE_REQUIREMENTS.missing.longLived} from ${PIPELINE_REQUIREMENTS.module}, ` +
      `over ${PIPELINE_REQUIREMENTS.transport}. ` +
      "Run the relayer in mock mode until that binding exists.",
    "not_wired",
    {requirements: PIPELINE_REQUIREMENTS, options},
  );
}

export function createSender({mode = "mock", ...options} = {}) {
  if (mode === "mock") return new MockSender(options);
  if (mode === "chain") return createPipelineSender(options);
  throw new SenderError(`unknown sender mode ${mode}`, "bad_mode", {mode});
}
