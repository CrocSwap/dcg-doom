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
//   PipelineSender   the binding to scripts/doom_relayer_server.py, which
//                    holds the R8m sender (scripts/doom_r7_render_loop.py
//                    fire_blind) and the R8b frame pipeline open for the life
//                    of a session and takes the player's input as a stream.
//                    See PIPELINE_REQUIREMENTS below.
//
// Nothing in this file signs or sends anything itself. MockSender never touches
// the network; PipelineSender spawns a process that holds the keys, and the
// relayer itself never sees one.

import {spawn} from "node:child_process";
import {existsSync} from "node:fs";
import {createInterface} from "node:readline";
import {dirname, resolve} from "node:path";
import {fileURLToPath} from "node:url";

import {STRIP_ACCOUNT_BYTES, STRIP_HEADER_BYTES, STRIP_PIXELS_BYTES, stripDigest, toHex} from "../../web/doom-play/strip.js";
import {decodeKeyState, heldToCommands} from "../../web/doom-play/input.js";

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

  async keyState({slot, keyMask}) {
    const record = this.#slot(slot);
    const firstSeq = record.cursor;
    const commands = heldToCommands(decodeKeyState(keyMask), firstSeq, this.ticsPerAdvance);
    const deposit = await this.deposit({slot, firstSeq, commands});
    const advanced = await this.advance({slot, maxTics: this.ticsPerAdvance, expectedCursor: firstSeq});
    return {
      accepted: 1,
      acceptedFrom: firstSeq,
      cursor: advanced.cursor,
      gametic: advanced.gametic,
      keyStateEvent: {seq: firstSeq, mask: keyMask, assigned_at_ms: Date.now()},
      deposit,
      advanced,
    };
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
// reviewed rather than guessed at. Four of the five operations already existed
// as functions somewhere in scripts/; the fifth, a way to keep one context
// advancing from an external source of commands, is
// `scripts/doom_relayer_server.py`.
//
//   deposit        SIM2 DEPOSIT, firstSeq + up to 64 commands.  Already
//                  derivable from the buffer and authority in scripts/
//                  doom_sweep_testnet.py:1450 (deposit_ix).
//   advance        SIM2 ADVANCE, optionally expected-cursor bound.
//                  doom_sweep_testnet.py:1454 (advance_ix).
//   snapshot       RSN/3 SNAPSHOT-LIVE into the buffer's snapshot account.
//                  doom_r8b_tokyo_pipeline.py:1015 (snapshot_instruction).
//   renderStrip    RENDER for one strip id.  doom_r8b_tokyo_pipeline.py:1029
//                  (strip_instruction).
//   longLived      scripts/doom_relayer_server.py: one process for the life of
//                  a session, JSON lines on stdin and stdout, so the cursor
//                  binding that ADVANCE_EXPECTED_CURSOR provides survives from
//                  one player batch to the next.
export const PIPELINE_REQUIREMENTS = {
  module: "scripts/doom_relayer_server.py",
  note: "do not edit doom_r7_render_loop.py or doom_r8b_tokyo_pipeline.py from this dispatch",
  operations: ["deposit", "advance", "snapshot", "renderStrip", "longLived"],
  existing: {
    deposit: "scripts/doom_sweep_testnet.py:1450 deposit_ix",
    advance: "scripts/doom_sweep_testnet.py:1454 advance_ix",
    snapshot: "scripts/doom_r8b_tokyo_pipeline.py:1015 FramePipeline.snapshot_instruction",
    renderStrip: "scripts/doom_r8b_tokyo_pipeline.py:1029 FramePipeline.strip_instruction",
    longLived: "scripts/doom_relayer_server.py",
  },
  missing: {},
  transport: "JSON lines on stdin and stdout, one request and one response per line, so the relayer can hold the process open for the life of a session",
  request: {op: "deposit", first_seq: 0, commands: "base64"},
  response: {ok: true, signature: "base58", cursor: 0, gametic: 0, tics: 8},
};

// --------------------------------------------------------------- pipeline

// A relayer holding one long-lived sender process, speaking JSON lines to it.
//
// The division of labour is the whole point. The advance pipeline, the deposit
// pacer, the render loop and every refund/reconciliation rule stay in the
// Python sender that R8m measured: this class never signs, never sends and
// never computes. It does three things the batch driver could not do:
//
//   1. it holds the process open for the life of a session rather than for
//      --seconds, so the cursor binding survives between player batches;
//   2. it stages the player's key state as a command stream, so a keypress is
//      a range of tics rather than a scripted table;
//   3. it reports the staging frontier, so the relayer can refuse input the
//      chain is not going to consume for another second. Without that a
//      player outrunning the engine builds a backlog whose latency grows
//      without bound, which is exactly the R8l5 failure.
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

export class PipelineSender {
  constructor(options = {}) {
    this.mode = "chain";
    // The advance pipeline and the frame loop run inside the child, on their
    // own schedule. The relayer's job is to hand over input, not to schedule.
    this.background = true;
    this.options = options;
    this.python = options.python ?? "python3";
    this.script = options.script ?? resolve(REPO_ROOT, "scripts", "doom_relayer_server.py");
    this.keysDir = options.keysDir ?? null;
    this.payerKey = options.payerKey ?? null;
    if (!this.keysDir || !this.payerKey) {
      throw new SenderError(
        "chain mode needs --keys-dir and --payer-key: the Doom keys stay on the host next to the relayer and are never sent anywhere",
        "no_keys",
      );
    }
    this.child = null;
    this.pending = new Map();
    this.nextId = 1;
    this.lines = [];
    this.logger = options.logger ?? null;
    this.hello = null;
    this.status = null;
    this.closed = false;
    this.onFrame = options.onFrame ?? null;
    this.onEvent = options.onEvent ?? null;
    this.onProgress = options.onProgress ?? null;
    this.osPaused = false;
    this.executionResumed = !options.startPaused;
    this.closing = false;
    this.startedAt = null;
    // Checked here rather than at the first input, so a relayer pointed at the
    // wrong tree says so while it is starting instead of after a player has
    // joined and is pressing keys.
    if (!existsSync(this.script)) {
      throw new SenderError(
        `the pipeline server is not at ${this.script}`,
        "no_pipeline_script",
        {script: this.script},
      );
    }
  }

  log(...parts) {
    this.logger?.(...parts);
  }

  // Everything the child is told, in one place, so a receipt can name the
  // configuration that produced it.
  argv() {
    const options = this.options;
    const argv = [
      this.script,
      "--run-dir", options.runDir ?? resolve(REPO_ROOT, "out", "doom-relayer-session"),
      "--keys-dir", this.keysDir,
      "--payer-key", this.payerKey,
    ];
    if (options.rpcA !== undefined && options.rpcA !== null) argv.push("--rpc-a", options.rpcA);
    // Empty RPC_B is an intentional single-node configuration. It must reach
    // argparse as an empty value or Python restores its public-node default.
    if (options.rpcB !== undefined && options.rpcB !== null) argv.push("--rpc-b", options.rpcB);
    // A third node, and the send rate, are the two levers on the frame's
    // own send cost: eight strip sends take 8 / (rate x endpoints) seconds.
    // Both are pushed only when set, so an unnamed session is the two-node
    // 30/s default the last measured run used.
    if (options.rpcC) argv.push("--rpc-c", options.rpcC);
    if (options.sendPacerRate !== undefined && options.sendPacerRate !== null) {
      argv.push("--send-pacer-rate", String(options.sendPacerRate));
    }
    if (options.imageSha256) argv.push("--image-sha256", options.imageSha256);
    if (options.imageBytes) argv.push("--image-bytes", String(options.imageBytes));
    if (options.seconds) argv.push("--seconds", String(options.seconds));
    if (options.startPaused) argv.push("--start-paused");
    if (options.publicMode) {
      argv.push("--public-mode");
      argv.push("--public-rollover-threshold", String(options.publicRolloverThreshold ?? 57_344));
      // The R-series pipeline's rollover needs these; the DCG pipeline (a ring
      // input stream, no rollover) is started without them.
      if (options.publicMaintenanceLockOwner) argv.push("--public-maintenance-lock-owner", options.publicMaintenanceLockOwner);
      if (options.stageCapScript) argv.push("--stage-cap-script", options.stageCapScript);
      if (options.r14KeysDir) argv.push("--r14-keys-dir", options.r14KeysDir);
    }
    if (options.estimateHorizonSeconds !== undefined && options.estimateHorizonSeconds !== null) {
      argv.push("--estimate-horizon-seconds", String(options.estimateHorizonSeconds));
    }
    if (options.payerBaselineLamports !== undefined && options.payerBaselineLamports !== null) {
      argv.push("--payer-baseline-lamports", String(options.payerBaselineLamports));
    }
    argv.push(options.render === false ? "--no-render" : "--render");
    argv.push("--render-buffers", String(options.renderBuffers ?? 1));
    argv.push("--strip-rate", String(options.stripRate ?? 16));
    argv.push("--advance-tics", String(options.advanceTics ?? 4));
    argv.push("--advance-pipeline-depth", String(options.advancePipelineDepth ?? 4));
    argv.push("--sweep-rate", String(options.sweepRate ?? 8));
    argv.push("--input-tics-per-second", String(options.inputTicsPerSecond ?? 35));
    if (options.inputStageLeadTics) argv.push("--input-stage-lead-tics", String(options.inputStageLeadTics));
    if (options.waitForAdvance === false) argv.push("--no-wait-for-advance");
    // These three were in start.sh and never reached the child: `argv()` is the
    // only place a chain-mode flag becomes a process argument, and a flag that
    // is not built here is a flag the pipeline has never seen.  The
    // reset-start-account one is the whole RESET/9 path, so its absence meant
    // a dead player's use press ended the session instead of restarting it.
    if (options.resetStartAccount) argv.push("--reset-start-account", options.resetStartAccount);
    if (options.atomicLiveInputs) argv.push("--atomic-live-inputs");
    if (options.frameDeadlineSeconds !== undefined && options.frameDeadlineSeconds !== null) {
      argv.push("--frame-deadline-seconds", String(options.frameDeadlineSeconds));
    }
    // The renderer's minimum cursor step. Same trap as the flags above: a flag
    // that is not built into `argv()` is a flag the pipeline has never seen, and
    // this one decides whether the session spends its block compute on pictures
    // of states the chain has already published.
    if (options.minCursorStepTics !== undefined && options.minCursorStepTics !== null) {
      argv.push("--min-cursor-step-tics", String(options.minCursorStepTics));
    }
    if (options.renderPoolWorkers !== undefined && options.renderPoolWorkers !== null) {
      argv.push("--render-pool-workers", String(options.renderPoolWorkers));
    }
    if (options.rpcLagDropSlots !== undefined && options.rpcLagDropSlots !== null) {
      argv.push("--rpc-lag-drop-slots", String(options.rpcLagDropSlots));
    }
    if (options.rpcLagCheckSeconds !== undefined && options.rpcLagCheckSeconds !== null) {
      argv.push("--rpc-lag-check-seconds", String(options.rpcLagCheckSeconds));
    }
    if (options.startCursor !== undefined && options.startCursor !== null) {
      argv.push("--start-cursor", String(options.startCursor));
    }
    // Same trap, twice more. `--status-batch` and `--status-min-interval` bound
    // the status poller, and `--strip-cu-limit` is the only lever on a context
    // whose renders cost more than 740,000 CU. All three were added to
    // start.sh, to the python parser, and to `main()` here -- and the first two
    // were *not* built here, so the pipeline ran on its defaults while the
    // runbook said otherwise. `test_relayer.mjs` now reads start.sh and asserts
    // every flag in it appears in `argv()`; that test is the reason this
    // comment can be short.
    if (options.statusBatch !== undefined && options.statusBatch !== null) {
      argv.push("--status-batch", String(options.statusBatch));
    }
    if (options.statusMinInterval !== undefined && options.statusMinInterval !== null) {
      argv.push("--status-min-interval", String(options.statusMinInterval));
    }
    if (options.stripCuLimit !== undefined && options.stripCuLimit !== null) {
      argv.push("--strip-cu-limit", String(options.stripCuLimit));
    }
    // The one-buffer read path. `--pre-read-snapshot` is what makes
    // `--overlap-readback` safe rather than merely fast: it takes the snapshot
    // account out of the readback's bank view, and the snapshot account is the
    // one account a snapshot transaction writes. Both are switches, so they are
    // pushed without a value and are absent unless asked for.
    if (options.preReadSnapshot) argv.push("--pre-read-snapshot");
    if (options.overlapReadback) argv.push("--overlap-readback");
    // Send a frame's strips behind its snapshot instead of after waiting for
    // it. A switch for the same reason as the two above: the pipeline is only
    // reachable through `argv()`, and a flag that is not built here is a
    // flag the pipeline has never seen.
    if (options.stripsBehindSnapshot) argv.push("--strips-behind-snapshot");
    if (options.batchRenderSends) argv.push("--batch-render-sends");
    argv.push("--spend-cap-fogo", String(options.spendCapFogo ?? 10));
    return argv;
  }

  async start() {
    if (this.child) return this.hello;
    const env = {...process.env};
    // The child imports the sweep and pipeline modules out of scripts/ and the
    // basanos package out of src/, exactly as the batch driver does.
    env.PYTHONPATH = [resolve(REPO_ROOT, "src"), dirname(this.script), env.PYTHONPATH]
      .filter(Boolean)
      .join(":");
    this.child = spawn(this.python, this.argv(), {env, stdio: ["pipe", "pipe", "pipe"]});
    this.startedAt = Date.now();
    const lines = createInterface({input: this.child.stdout});
    lines.on("line", (line) => this.#onLine(line));
    this.child.stderr.on("data", (chunk) => {
      const text = String(chunk).trimEnd();
      if (text) this.log(`[pipeline stderr] ${text}`);
    });
    this.child.on("exit", (code, signal) => {
      this.closed = true;
      const error = new SenderError(
        `the pipeline process exited with ${signal ?? `code ${code}`}`,
        "pipeline_exited",
        {code, signal, lines: this.lines.slice(-20)},
      );
      for (const [, waiter] of this.pending) waiter.reject(error);
      this.pending.clear();
      this.log(`pipeline process exited: ${signal ?? code}`);
    });
    // The child answers hello only after its own preflight, so a relayer that
    // joins against a program that moved, or a context that is full, finds out
    // here rather than mid-game.
    const started = this.#waitForEvent("started", 90_000);
    await started;
    this.hello = await this.request({op: "hello"});
    return this.hello;
  }

  async resume() {
    if (this.closed || !this.child) throw new SenderError("the pipeline process is not running", "pipeline_exited");
    if (this.osPaused) {
      this.child.kill("SIGCONT");
      this.osPaused = false;
      return {ok: true, resumed: true};
    }
    if (!this.executionResumed) {
      const reply = await this.request({op: "resume"});
      this.executionResumed = true;
      return reply;
    }
    return {ok: true, resumed: true};
  }

  async pause() {
    if (this.closed || !this.child || this.osPaused || !this.executionResumed) return {ok: true, paused: true};
    // The DCG pipeline pauses itself: it lets the frames it has sent land and
    // builds no more. A SIGSTOPped child would resume its in-flight lane
    // transactions with stale blockhashes.
    if (this.hello?.backend === "doom-dcg-lanes") {
      const reply = await this.request({op: "pause"});
      this.executionResumed = false;
      return reply;
    }
    this.child.kill("SIGSTOP");
    this.osPaused = true;
    return {ok: true, paused: true};
  }

  #onLine(line) {
    if (!line.trim()) return;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      this.log(`[pipeline] non-JSON line: ${line.slice(0, 200)}`);
      return;
    }
    this.lines.push(message);
    if (this.lines.length > 400) this.lines.shift();
    if (message.id !== undefined && this.pending.has(message.id)) {
      const waiter = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.ok === false) waiter.reject(new SenderError(message.error ?? "the pipeline refused the request", "pipeline_refused", message));
      else waiter.resolve(message);
      return;
    }
    if (message.event === "progress") {
      this.status = message;
      this.onProgress?.(message);
      return;
    }
    if (message.event === "frame") {
      this.onFrame?.(message);
      return;
    }
    if (message.event === "started" || message.event === "finished" || message.event) {
      // A public rollover clears the child's execution gate after RESET/9.
      // The next accepted input must send `resume` again, even though this
      // sender already resumed the original session.
      if (message.event === "rollover_complete") this.executionResumed = false;
      this.onEvent?.(message);
      this.log(`[pipeline] ${JSON.stringify(message)}`);
      for (const [id, waiter] of this.pending) {
        if (waiter.event === message.event) {
          this.pending.delete(id);
          waiter.resolve(message);
        }
      }
      return;
    }
    this.log(`[pipeline] ${line.slice(0, 200)}`);
  }

  #waitForEvent(event, timeoutMs) {
    return new Promise((resolvePromise, reject) => {
      const id = this.nextId++;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new SenderError(`the pipeline did not report ${event} within ${timeoutMs / 1000} s`, "pipeline_timeout", {
          lines: this.lines.slice(-20),
        }));
      }, timeoutMs);
      this.pending.set(id, {
        event,
        resolve: (value) => {
          clearTimeout(timer);
          resolvePromise(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      });
    });
  }

  request(payload, {timeoutMs = 30_000} = {}) {
    if (this.closed || !this.child) {
      return Promise.reject(new SenderError("the pipeline process is not running", "pipeline_exited"));
    }
    const id = this.nextId++;
    const line = `${JSON.stringify({...payload, id})}\n`;
    return new Promise((resolvePromise, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new SenderError(`the pipeline did not answer ${payload.op} within ${timeoutMs / 1000} s`, "pipeline_timeout"));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer);
          resolvePromise(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      });
      this.child.stdin.write(line, (error) => {
        if (error) {
          this.pending.delete(id);
          clearTimeout(timer);
          reject(new SenderError(`could not write to the pipeline: ${error.message}`, "pipeline_gone"));
        }
      });
    });
  }

  // -- the contract, against a background sender ----------------------------

  // The slot is the context the child is already driving. There is nothing to
  // provision: a context costs about 73 FOGO of rent, so the operator made it
  // before the relayer started rather than per player.
  async provisionSlot() {
    const hello = await this.start();
    return {
      slot: 0,
      label: `chain-${hello.context.slice(0, 8)}`,
      context: hello.context,
      meta: hello.meta,
      inputBuffer: hello.buffer,
      authority: hello.authority,
      state: hello.state,
      mirror: hello.mirror,
      snapshot: hello.snapshot_account,
      strips: hello.strip_accounts,
      inputCapacity: hello.capacity,
      cursor: hello.cursor,
      gametic: hello.gametic,
      // Above whatever a previous session staged but never advanced, so the
      // player's first batch does not try to rewrite tics the chain already
      // holds.
      nextSeq: hello.staged_frontier ?? hello.start_cursor,
      stagedFrontier: hello.staging_frontier,
      producer: hello.producer ?? null,
      frameCounters: hello.frame_counters ?? null,
      rpcUrls: hello.rpc_urls ?? null,
      // Which buffer holds the newest COMPLETE frame, and that buffer's
      // accounts. The child reads each frame back before counting it, so it
      // knows; the viewer, reading eight strip accounts that land in eight
      // transactions, cannot.
      latestFrame: hello.latest_frame ?? null,
    };
  }

  // The player's key state, staged as the next tics' commands. The child
  // deposits them at its own 10/s producer rate, exactly as the measured
  // session did.
  async deposit({slot, firstSeq, commands}) {
    if (this.closed) throw new SenderError("the pipeline process is not running", "pipeline_exited");
    const reply = await this.request({
      op: "commands",
      first_seq: firstSeq,
      commands: Buffer.from(commands).toString("base64"),
    });
    const offered = commands.length / 4;
    if (reply.accepted < 1) {
      throw new SenderError(
        `the pipeline accepted none of ${offered} tics at ${firstSeq}`,
        "short_deposit",
        reply,
      );
    }
    // A short accept is normal now, not a fault: the producer's own staging
    // limit is the last word on which tics are still the writer's, and it can
    // be tighter than the frontier the relayer trimmed against.  Throwing here
    // would turn "one tic less than offered" into a 500 the page cannot act on.
    if (reply.accepted !== offered) {
      this.log(
        `the pipeline accepted ${reply.accepted} of ${offered} tics at ${firstSeq} ` +
          `(first_seq ${reply.first_seq}, stage limit ${reply.producer_stage_limit})`,
      );
    }
    return {
      accepted: reply.accepted,
      firstSeq: reply.first_seq ?? firstSeq,
      signature: null,
      cursor: reply.cursor,
      producerGeneratedCursor: reply.producer_generated_cursor,
      producerNextCursor: reply.producer_next_cursor ?? null,
      producerStageLimit: reply.producer_stage_limit,
      // Whether the batch was written into a window the producer will reach at
      // all. A batch above the stage limit is queued, which is normal for a
      // page that leads by its round trip; a batch whose write landed after the
      // producer read those tics is the input drop, and the pipeline's own
      // numbers in `pipeline` are how a page tells the two apart.
      queuedAboveStageLimit: reply.queued_above_stage_limit ?? null,
      trimmedFromSeq: reply.trimmed_from_seq ?? null,
      pipeline: reply.pipeline ?? null,
    };
  }

  async keyState({slot, keyMask}) {
    if (this.closed) throw new SenderError("the pipeline process is not running", "pipeline_exited");
    // An idle public lobby SIGSTOPs the Python child so it cannot keep sending
    // transactions. Wake it before writing the next key state: a stopped child
    // cannot read the command, and waiting for its reply before calling resume
    // would deadlock the first input after an idle pause.
    if (this.osPaused) {
      this.child.kill("SIGCONT");
      this.osPaused = false;
    }
    const reply = await this.request({op: "key_state", key_mask: keyMask});
    this.status = {...this.status, ...reply};
    return {
      accepted: reply.accepted ?? 1,
      acceptedFrom: reply.accepted_from,
      stampGametic: reply.stamp_gametic ?? null,
      cursor: reply.cursor,
      gametic: reply.gametic,
      ticAssignedAtMs: reply.tic_assigned_at_ms ?? null,
      keyStateEvent: reply.key_state ?? null,
      producerNextCursor: reply.producer_next_cursor,
      pipeline: reply.pipeline ?? null,
    };
  }

  // The advance is not this method's job any more: the child runs the
  // cursor-bound pipeline continuously, so asking it to advance would be asking
  // for a second scheduler. This reports where the chain is.
  async advance() {
    const status = await this.readProgress();
    return {cursor: status.cursor, gametic: status.gametic, tics: 0, signature: null, background: true};
  }

  // Likewise the frame: the child's render loop is what paints one, and the
  // viewer reads the strip accounts off the chain itself. This reports the
  // newest frame the child knows landed.
  async renderFrame() {
    await this.readProgress();
    // readProgress returns a fresh object literal, so `last_frame` was read off
    // an object that never had it and this always threw `no_frame` -- which is
    // why the page's rendered-on-chain cell never left the snapshot account
    // link in chain mode. The raw answer is on `this.status`.
    const frame = this.status?.latest_frame ?? this.status?.last_frame ?? null;
    if (!frame) throw new SenderError("no frame has landed yet", "no_frame");
    // The child's own keys first, so this is the same object the status answer
    // carries and a page cannot tell the two apart; the camelCase aliases are
    // for the fields this class has always returned by that name.
    return {
      ...frame,
      buffer: frame.buffer ?? null,
      cursor: frame.cursor,
      gametic: frame.gametic,
      snapshotHash: frame.snapshot_hash,
      stateRoot: frame.state_root,
      signature: frame.signature,
      snapshotAccount: frame.snapshot_account ?? null,
      stripAccounts: frame.strip_accounts ?? null,
      strips: null,
      background: true,
    };
  }

  async readProgress() {
    const status = await this.request({op: "status"}, {timeoutMs: 10_000});
    this.status = status;
    return {
      cursor: status.cursor,
      gametic: status.gametic,
      capacity: status.capacity,
      halted: Boolean(status.halt),
      stagingFrontier: status.staging_frontier,
      producer: status.producer ?? null,
      // The chain's own fixed frontier over the input slots, and the guard the
      // input path refuses below. The producer's generated cursor is not this
      // number and cannot stand in for it: on a resumed context it walks over
      // slots an earlier session already deposited (doom-drive-2).
      writableFrontier: status.writable_frontier ?? null,
      writableFloor: status.writable_floor ?? null,
      frontierProbes: status.frontier_probes ?? null,
      // The producer's NEXT tic to generate, which is not the same number as
      // `generated_cursor` (the end of the batch it has already published) and
      // is what a page has to aim ahead of. See doom_relayer_server.py.
      producerNextCursor: status.producer_next_cursor ?? null,
      // The lead the producer resolved from its own measured round trip, and
      // the round trip it read. The child publishes both in every `status` and
      // in every input answer, and they were dropped here, so the page could
      // only re-derive a lead from its own POST latency -- which is a different
      // quantity, and 16-20 tics where the producer had measured 61. A viewer
      // that cannot see the producer's own number has to guess it.
      inputStageLeadTics: status.input_stage_lead_tics ?? null,
      inputRoundTripSeconds: status.input_round_trip_seconds ?? null,
      // Per-deposit fate for the player's commands, and whether the pipeline can
      // still deliver them at all. Both ride on `status`, which this method
      // already asked for, so neither costs a round trip.
      inputFate: status.input_fate ?? null,
      pipeline: status.pipeline ?? null,
      ticsPerSecondCap: status.input_tics_per_second_cap ?? null,
      frameCounters: status.frame_counters ?? null,
      rpcUrls: status.rpc_urls ?? null,
      rpcLag: status.rpc_lag ?? null,
      inputRefusals: status.input_refusals ?? null,
      ticsPerSecond: status.tics_per_second,
      framesCompleted: status.frames_completed,
      framesPerSecond: status.frames_per_second,
      latestFrame: status.latest_frame ?? null,
      spendFogo: status.spend_fogo,
    };
  }

  // How far past the committed cursor input may be staged. The relayer refuses
  // a batch beyond it rather than queueing input the chain will not run for
  // another second, which is what made R8l5's input latency drift upward
  // through a session.
  //
  // Asked of the child every time, never served from `this.status`. The bound
  // used to be the cursor plus a constant, so a cached copy was merely a little
  // stale and the page's own floor was stale by the same amount; it is now the
  // tic producer's own staging limit, an absolute sequence, while the page's
  // floor is a cursor read a moment later. Caching it then aims a batch at a
  // sequence the engine has already read -- which is refused, every push, and
  // is the accepted-and-dropped input path with a 409 on it. One pipe round
  // trip is a cheap price for the bound and the floor being from the same
  // instant.
  async stagingFrontier() {
    const status = await this.request({op: "status"}, {timeoutMs: 10_000});
    this.status = status;
    return status.staging_frontier;
  }


  // The viewer reads the chain in chain mode, so the relayer has no bytes to
  // hand it. Saying so beats returning eight empty accounts.
  async readStrips() {
    throw new SenderError(
      "chain mode has no strip bytes to hand out: the frame pipeline writes the strip accounts and " +
        "the viewer reads them from the network, which is the design in docs/design/doom-relayer-api-2026-09-25.md",
      "no_strip_bytes",
    );
  }

  async close() {
    if (!this.child || this.closed || this.closing) return;
    this.closing = true;
    if (this.osPaused) {
      this.child.kill("SIGCONT");
      this.osPaused = false;
    }
    try {
      // The documented stop: the child writes its receipt, then exits.
      await this.request({op: "stop"}, {timeoutMs: 5_000});
    } catch {
      // A child that is already gone needs no stop.
    }
    const exited = new Promise((resolvePromise) => {
      const timer = setTimeout(() => {
        this.child?.kill("SIGTERM");
        resolvePromise();
      }, 20_000);
      this.child.once("exit", () => {
        clearTimeout(timer);
        resolvePromise();
      });
    });
    await exited;
    this.closed = true;
    this.child?.stdin?.end();
  }
}

export function createPipelineSender(options = {}) {
  return new PipelineSender(options);
}

export function createSender({mode = "mock", ...options} = {}) {
  if (mode === "mock") return new MockSender(options);
  if (mode === "chain") return createPipelineSender(options);
  throw new SenderError(`unknown sender mode ${mode}`, "bad_mode", {mode});
}
