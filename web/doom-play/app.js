// Doom on Fogo: the viewer.
//
// This page never signs or sends a transaction. It reads the eight strip
// accounts of a frame, paints the 320x200 palette frame to a canvas, and
// signs input batches with a throwaway key so the relayer can attribute them
// to a player. Everything network-specific is in network.js.

import {DEFAULT_NETWORK, explorerAddress, explorerTransaction, requireNetwork} from "./network.js";
import {ChainReader, WadImageReader, readPlaypal, base58ToBytes, bytesToBase58} from "./rpc.js";
import {decodeFrame, frameToRgba, parseSnapshot, frameReadAddresses, toHex, StripError, FRAME_WIDTH, FRAME_HEIGHT} from "./strip.js";
import {loadOrCreateBurner, forgetSession, recallSession, rememberSession, ed25519Available} from "./burner.js";
import {encodeInput, heldToCommands} from "./input.js";
import {InputLog} from "./inputlog.js";

// The engine rate to assume before the relayer has reported one. Measured at
// 23.8-26.3 tics/s across two drive sessions at strip rate 64, so 25 is the
// middle of what has actually been seen rather than a design number.
const DEFAULT_TICS_PER_SECOND = 25;
// The relayer's own sequence window, and how far inside it the page aims.
const SEQ_WINDOW = 240;
// Batches in flight at once, so the cadence is the interval and not the round
// trip.
const MAX_PUSH_IN_FLIGHT = 2;
// How often the viewer reads a frame. A frame is complete only between one
// frame's last strip landing and the next frame's first, so a torn read is the
// expected case at strip rate 64 and is retried at once.
const POLL_MS = 250;

const THEME_KEY = "doom-play.theme";
const RELAYER_KEY = "doom-play.relayer";
const BUFFER_KEY = "doom-play.buffer";
const PALETTE_KEY = "doom-play.palette.v1";

const query = new URLSearchParams(location.search);
const networkName = query.get("net") ?? DEFAULT_NETWORK;
const net = requireNetwork(networkName, ["rpcHttp", "doom.program", "doom.context", "doom.strips", "explorer.address"]);

const el = (id) => document.getElementById(id);
const ui = {
  networkBadge: el("network-badge"),
  commitmentStamp: el("commitment-stamp"),
  themeToggle: el("theme-toggle"),
  sessionState: el("session-state"),
  sessionTitle: el("session-title"),
  burnerKey: el("burner-key"),
  slotCell: el("slot-cell"),
  inputsSent: el("inputs-sent"),
  inputLatency: el("input-latency"),
  relayerUrl: el("relayer-url"),
  bufferSelect: el("buffer-select"),
  join: el("join"),
  leave: el("leave"),
  stageState: el("stage-state"),
  stageBadge: el("stage-badge"),
  canvas: el("doom-canvas"),
  statTic: el("stat-tic"),
  statFps: el("stat-fps"),
  statPicture: el("stat-picture"),
  statStalls: el("stat-stalls"),
  statSlot: el("stat-slot"),
  statCommitment: el("stat-commitment"),
  statExplorer: el("stat-explorer"),
  footFrame: el("foot-frame"),
  railState: el("rail-state"),
  chainProgram: el("chain-program"),
  chainContext: el("chain-context"),
  chainSnapshot: el("chain-snapshot"),
  chainSnapshotHash: el("chain-snapshot-hash"),
  chainStateRoot: el("chain-state-root"),
  chainFrameSha: el("chain-frame-sha"),
  chainCapacity: el("chain-capacity"),
  stripGrid: el("strip-grid"),
  stripSummary: el("strip-summary"),
  inputlogList: el("inputlog-list"),
  inputlogSummary: el("inputlog-summary"),
  inputlogBanner: el("inputlog-banner"),
  inputlogLead: el("inputlog-lead"),
  inputlogTorn: el("inputlog-torn"),
  hintCapacity: el("hint-capacity"),
  spectatorHint: el("spectator-hint"),
  error: el("error"),
};

// ---------------------------------------------------------------- theme

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    /* storage disabled; the theme just will not persist */
  }
}

ui.themeToggle.addEventListener("click", () => {
  document.body.classList.add("theme-flip");
  setTheme(document.documentElement.dataset.theme === "light" ? "dark" : "light");
  setTimeout(() => document.body.classList.remove("theme-flip"), 260);
});

// ---------------------------------------------------------------- state

const state = {
  reader: null,
  wallet: null,
  burnerCreated: false,
  palette: null,
  buffer: Number(localStorage.getItem(BUFFER_KEY) ?? 0) || 0,
  session: null,
  relayerBase: null,
  nextSeq: 0,
  // The chain's committed cursor and the furthest sequence input may still be
  // deposited at. Input is placed against these two, not against a count of
  // what the page has sent: the engine consumes tics whether or not anyone
  // writes them, so a page that does not follow the cursor writes into slots
  // the game has already read.
  cursor: 0,
  stagingFrontier: null,
  inputWindow: null,
  // The tic producer's own two cursors, as the relayer reports them. This is
  // the other end of the input rendezvous: `producerStageLimit` is the
  // sequence it will deposit next and `producerGeneratedCursor` the last one it
  // has, so a batch aimed between them is read rather than dropped.
  producerStageLimit: null,
  producerGeneratedCursor: null,
  // The producer's NEXT tic to generate, which is not `producerGeneratedCursor`
  // (the end of the batch it has already published) and is the only cursor a
  // writer can honestly aim ahead of. See doom_relayer_server.py.
  producerNextCursor: null,
  // Whether keystrokes can still reach the engine, and the relayer's per-deposit
  // record of what happened to the ones already sent.
  pipeline: null,
  inputFate: null,
  // When the two producer numbers above were read, and the engine rate they are
  // read against. The page's aim is a prediction, and a prediction needs the
  // age of its input.
  producerReadAt: 0,
  ticsPerSecond: null,
  // The relayer's pointer to the render buffer holding the newest COMPLETE
  // frame, and that buffer's accounts. The page reads this buffer, not the one
  // being written.
  framePointer: null,
  frameCounters: null,
  // A key going down, and the first frame painted after it. That pair is the
  // input-to-picture latency a player feels, and it is the only one of the
  // page's numbers that says whether the game is responding.
  keysDownAt: null,
  pictureLatencies: [],
  clientSeq: 0,
  inputsSent: 0,
  batchesSigned: 0,
  refusals: 0,
  // The measured POST round trip in ms, and the lead it implies in tics. Both
  // are on the panel and both are in the receipt: the lead is the design's
  // price for a window narrower than the round trip, and its size is the
  // number that says whether the price is right.
  roundTripMs: null,
  leadTics: null,
  aimedAt: null,
  seqWindow: SEQ_WINDOW,
  latencies: [],
  lastFrame: null,
  lastFrameSlot: null,
  lastFrameBuffer: null,
  paintedFrames: 0,
  paintedAt: 0,
  // When each frame was painted, over a rolling window. The panel's rate is
  // the rate of THESE, and nothing else: the status tick used to push a
  // timestamp here every 500 ms and call the result a frame rate, so the
  // panel read "2.00" on a page that had painted one frame in sixty seconds
  // (measured, doom-drive-2). A paint is a paint.
  paintTimes: [],
  signature: null,
  keys: {},
  stopPolling: null,
  lastError: null,
  // A torn read is the expected case at strip rate 64, not a fault: the strips
  // of a frame land in eight transactions and a read catches them mid-write.
  // It was raised as an error and shown in the error bar, which is how a page
  // that was working fine came to look broken. Counted, not shown.
  tornReads: 0,
};

// One row per key press, with the fate of every tic it aimed at.
const inputLog = new InputLog({
  link: (signature) => {
    const anchor = document.createElement("a");
    anchor.href = explorerTransaction(net, signature);
    anchor.textContent = "↗";
    anchor.target = "_blank";
    anchor.rel = "noopener";
    return anchor;
  },
});

function renderInputLog() {
  inputLog.setLead(state.leadTics, state.ticsPerSecond);
  inputLog.render(ui.inputlogList, ui.inputlogSummary, ui.inputlogLead, ui.inputlogTorn, state.tornReads);
}

const keys = () => state.keys;

const stripChips = [];
for (let i = 0; i < 8; i += 1) {
  const chip = document.createElement("div");
  chip.className = "strip-chip";
  chip.innerHTML = `<i></i><b>${i}</b>`;
  ui.stripGrid.append(chip);
  stripChips.push(chip);
}

// Two error channels. A read error is transient and clears itself on the next
// good frame. An action error, from joining or sending input, stays until the
// player does something about it, so a successful frame read does not silently
// wipe "no free slot" off the screen.
function showError(error, source = "read") {
  if (state.errorSource !== "action" || source === "action") state.errorSource = source;
  const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  if (text === state.lastError) return;
  state.lastError = text;
  ui.error.textContent = text;
  ui.error.hidden = false;
}

function clearError(source = "read") {
  // An action error outranks a read error and is not cleared by a good frame.
  if (state.errorSource === "action" && source !== "action") return;
  state.lastError = null;
  state.errorSource = null;
  ui.error.hidden = true;
}

const shorten = (text, head = 6, tail = 6) =>
  !text ? "—" : text.length <= head + tail + 1 ? text : `${text.slice(0, head)}…${text.slice(-tail)}`;

function toBase64(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

// The relayer hands the slot binding over as hex, because JSON has no byte
// array. encodeInput wants the 32 bytes themselves.
function fromHex(text) {
  if (typeof text !== "string" || !/^[0-9a-fA-F]{64}$/.test(text)) return null;
  const bytes = new Uint8Array(32);
  for (let i = 0; i < 32; i += 1) bytes[i] = parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return bytes;
}

// A mock relayer's placeholder accounts are not base58, so linking them to an
// explorer would just be a broken link. Real addresses always are.
const BASE58 = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;

function explorerFor(address) {
  return BASE58.test(address ?? "") ? explorerAddress(net, address) : null;
}

function link(text, href) {
  if (!href) return document.createTextNode(text);
  const anchor = document.createElement("a");
  anchor.href = href;
  anchor.textContent = text;
  anchor.target = "_blank";
  anchor.rel = "noopener";
  return anchor;
}

function setCell(node, value, tone = "") {
  node.textContent = value;
  node.className = tone;
}

// ---------------------------------------------------------------- painting

let surface = null;
let surfaceContext = null;

function prepareSurface() {
  if (surface) return;
  surface = document.createElement("canvas");
  surface.width = FRAME_WIDTH;
  surface.height = FRAME_HEIGHT;
  surfaceContext = surface.getContext("2d", {alpha: false});
}

async function paintFrame(frame) {
  prepareSurface();
  const rgba = frameToRgba(frame.pixels, state.palette);
  const image = new ImageData(rgba, FRAME_WIDTH, FRAME_HEIGHT);
  surfaceContext.putImageData(image, 0, 0);

  const ratio = Math.min(globalThis.devicePixelRatio || 1, 2);
  const rect = ui.canvas.getBoundingClientRect();
  const width = Math.max(1, Math.floor(rect.width * ratio));
  const height = Math.max(1, Math.floor(rect.height * ratio));
  if (ui.canvas.width !== width || ui.canvas.height !== height) {
    ui.canvas.width = width;
    ui.canvas.height = height;
  }
  const context = ui.canvas.getContext("2d", {alpha: false});
  context.imageSmoothingEnabled = false;
  context.setTransform(1, 0, 0, 1, 0, 0);
  // Integer scaling keeps the pixels square; a 320x200 frame on a wide panel
  // is letterboxed rather than stretched.
  const scale = Math.min(width / FRAME_WIDTH, height / FRAME_HEIGHT);
  const drawWidth = Math.floor(FRAME_WIDTH * scale);
  const drawHeight = Math.floor(FRAME_HEIGHT * scale);
  context.fillStyle = "#000";
  context.fillRect(0, 0, width, height);
  context.drawImage(surface, Math.floor((width - drawWidth) / 2), Math.floor((height - drawHeight) / 2), drawWidth, drawHeight);

  state.lastFrame = frame;
  state.paintedFrames += 1;
  state.paintedAt = performance.now();
  state.paintTimes.push(state.paintedAt);
  while (state.paintTimes.length > 2 && state.paintedAt - state.paintTimes[0] > 4000) {
    state.paintTimes.shift();
  }
  // A frame past the tic the picture was on when the key went down is the
  // player's keypress showing up.  Recorded once per hold, and it is the
  // number that says the page is drivable rather than merely connected.
  if (state.keysDownAt !== null && frame.gametic > (state.gameticAtKeyDown ?? -1)) {
    state.pictureLatencies.push(performance.now() - state.keysDownAt);
    if (state.pictureLatencies.length > 40) state.pictureLatencies.shift();
    state.keysDownAt = null;
  }
  updateStatus();
}

function median(values) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

// The paint rate over the rolling window in `state.paintTimes`: intervals
// between consecutive paints, which is the only thing a viewer can see. Two
// paints are the minimum, because one paint has no rate.
function fps() {
  if (state.paintTimes.length < 2) return null;
  const span = state.paintTimes[state.paintTimes.length - 1] - state.paintTimes[0];
  if (span <= 0) return null;
  return ((state.paintTimes.length - 1) * 1000) / span;
}

function updateStatus() {
  const frame = state.lastFrame;
  if (!frame) return;
  setCell(ui.statTic, String(frame.gametic));
  const rate = fps();
  setCell(ui.statFps, rate === null ? "—" : rate.toFixed(2));
  const picture = median(state.pictureLatencies);
  setCell(ui.statPicture, picture === null ? "—" : `${picture.toFixed(0)} ms`);
  const counters = state.frameCounters;
  setCell(
    ui.statStalls,
    counters ? `${(counters.frame_stalls_per_minute ?? 0).toFixed(1)} / ${(counters.frame_skips ?? 0)}` : "—",
  );
  ui.statStalls.title = counters
    ? `frame deadline ${counters.frame_deadline_seconds ?? "?"} s · ${counters.frames_submitted ?? 0} submitted · ` +
      `${counters.frame_retries ?? 0} retried`
    : "the relayer has not reported frame counters yet";
  setCell(ui.statSlot, state.lastFrameSlot === null ? "—" : String(state.lastFrameSlot));
  setCell(ui.statCommitment, state.reader?.paintCommitment ?? net.commitment.paint, "good");

  const snapshot = net.doom.snapshots[state.buffer];
  const explorer = state.signature ? explorerTransaction(net, state.signature) : explorerFor(snapshot);
  if (explorer) {
    ui.statExplorer.textContent = state.signature ? shorten(state.signature, 10, 8) : "snapshot account ↗";
    ui.statExplorer.href = explorer;
    ui.statExplorer.title = explorer;
  } else {
    ui.statExplorer.textContent = state.signature ? shorten(state.signature, 10, 8) : "no explorer for this address";
    ui.statExplorer.removeAttribute("href");
    ui.statExplorer.removeAttribute("title");
  }

  ui.footFrame.textContent = `frame sha256 ${shorten(frame.digest ?? "…", 12, 12)} · ${state.paintedFrames} painted`;
  ui.chainSnapshotHash.textContent = frame.snapshotHash;
  ui.chainStateRoot.textContent = frame.stateRoot;
  ui.chainFrameSha.textContent = frame.digest ?? "—";
  for (let i = 0; i < 8; i += 1) {
    stripChips[i].classList.add("ok");
    stripChips[i].title = `strip ${i} · ${shorten(frame.stripHashes[i], 10, 8)}`;
  }
  ui.stripSummary.textContent = "8 / 8";
  ui.railState.textContent = "VERIFIED";
}

// ---------------------------------------------------------------- reading

async function loadPalette() {
  const cached = readCachedPalette();
  if (cached) {
    state.palette = cached;
    return "localStorage";
  }
  const doom = net.doom;
  requireNetwork(networkName, ["doom.wadAccounts", "doom.wadAccountBytes", "doom.wadHeaderBytes", "doom.wadImageBytes"]);
  const wad = new WadImageReader({
    readSlice: (address, offset, length) => state.reader.getAccount(address, {offset, length}),
    accounts: doom.wadAccounts,
    accountBytes: doom.wadAccountBytes,
    headerBytes: doom.wadHeaderBytes,
    imageBytes: doom.wadImageBytes,
  });
  const playpal = await readPlaypal(wad);
  state.palette = playpal;
  try {
    localStorage.setItem(PALETTE_KEY, JSON.stringify({wad: doom.wadSha256, palette: Array.from(playpal)}));
  } catch {
    /* a palette that will not cache just costs three slice reads next load */
  }
  return "chain";
}

function readCachedPalette() {
  try {
    const record = JSON.parse(localStorage.getItem(PALETTE_KEY) ?? "null");
    if (!record || record.wad !== net.doom.wadSha256 || !Array.isArray(record.palette)) return null;
    const bytes = Uint8Array.from(record.palette);
    return bytes.length === 768 ? bytes : null;
  } catch {
    return null;
  }
}

// A session's strips live on chain, and the viewer reads them from the network
// like any other reader. A mock relayer has no chain accounts at all, so when
// the network read fails and a session is open, the relayer's copy of the same
// eight strip accounts is used instead. Same decoder either way: the bytes are
// the bytes.
async function readFromRelayer() {
  if (!state.session || !state.relayerBase) return null;
  const response = await fetch(`${state.relayerBase}/v1/sessions/${encodeURIComponent(state.session.id)}/frame`);
  if (!response.ok) return null;
  const payload = await response.json();
  if (!Array.isArray(payload.strips) || payload.strips.length !== 8) return null;
  return {
    slot: null,
    // Mock mode has no snapshot account to read alongside the strips, so the
    // strips are the frame: decodeFrame falls back to the eight agreeing.
    hasSnapshot: false,
    payloads: [null, ...payload.strips.map((encoded) => {
      const binary = atob(encoded);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
      return bytes;
    })],
  };
}

// The buffer to read: the one the relayer says holds the newest complete
// frame, or the configured one before it has said anything. With two buffers
// the pipeline round-robins, so the configured buffer is the one being written
// about half the time.
function frameTarget() {
  const pointer = state.framePointer;
  const buffer = pointer && Array.isArray(pointer.strip_accounts) ? pointer.buffer : null;
  const strips = pointer && Array.isArray(pointer.strip_accounts) ? pointer.strip_accounts : net.doom.strips[state.buffer];
  const snapshot = pointer && pointer.snapshot_account ? pointer.snapshot_account : net.doom.snapshots[buffer ?? state.buffer];
  return {buffer: buffer ?? state.buffer, snapshot, strips};
}

async function readOnce() {
  const target = frameTarget();
  const strips = target.strips;
  if (!strips || strips.length !== 8) {
    showError(new Error(`buffer ${target.buffer} has no strip accounts configured`));
    return;
  }
  let slot = null;
  let payloads = null;
  let hasSnapshot = true;
  try {
    // One getMultipleAccounts for the snapshot and all eight strips of one
    // render buffer. They land in nine transactions, and at strip rate 64 a
    // frame is complete only briefly; two calls can straddle a write, and the
    // strips of one frame do not commit to each other -- only to the snapshot,
    // which is why the snapshot is in this read.
    ({slot, payloads} = await state.reader.getAccounts(frameReadAddresses(target)));
  } catch (error) {
    const relayed = await readFromRelayer().catch(() => null);
    if (!relayed) throw error;
    ({slot, payloads} = relayed);
    hasSnapshot = Boolean(relayed.hasSnapshot);
    setCell(ui.stageState, "MOCK MODE · RELAYER FRAME");
  }
  const present = payloads.slice(1).filter(Boolean).length;
  ui.stripSummary.textContent = `${present} / 8`;
  for (let i = 0; i < 8; i += 1) {
    const ok = Boolean(payloads[i + 1]);
    stripChips[i].classList.toggle("ok", ok);
    if (ok) stripChips[i].title = `strip ${i} present`;
  }
  if (present < 8 || (hasSnapshot && !payloads[0])) {
    // A frame is the snapshot and all eight strips or nothing. Keep the last
    // good one.
    setCell(ui.railState, "PARTIAL");
    return;
  }
  const snapshot = hasSnapshot ? await parseSnapshot(payloads[0]) : null;
  const frame = await decodeFrame(payloads.slice(1), {snapshot});
  frame.digest = await digestHex(frame.pixels);
  await paintFrame(frame);
  state.lastFrameSlot = slot;
  state.lastFrameBuffer = target.buffer;
  clearError();
}

async function digestHex(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return toHex(digest);
}

function startPolling() {
  state.stopPolling?.();
  let stopped = false;
  let timer = null;
  const loop = async () => {
    if (stopped) return;
    let torn = false;
    try {
      await readOnce();
    } catch (error) {
      // A torn frame is the expected case while a new frame is landing, and it
      // is not a fault the player can do anything about. It used to be raised
      // into the error bar, so a page that was working perfectly looked broken
      // and the error bar covered the panel; it is counted and shown as a
      // number instead, and the last good frame stays on screen.
      if (error instanceof StripError) {
        state.tornReads += 1;
        torn = true;
        setCell(ui.railState, "TORN");
        renderInputLog();
      } else {
        showError(error);
      }
    }
    if (stopped) return;
    // Frames land at 1-2/s at strip rate 16 and faster at 64, and a frame is
    // complete only for as long as the pipeline has not started the next one,
    // so a read that catches a frame mid-write is the expected case rather than
    // a fault: it is retried at the next poll with no backoff, because backing
    // off is what held the painted rate at one frame a minute. Other failures
    // are a node or a relayer having a bad time, and those do back off.
    const delay = torn || !state.lastError ? POLL_MS : 3000;
    timer = setTimeout(loop, delay);
  };
  loop();
  state.stopPolling = () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    state.stopPolling = null;
  };
}

// ---------------------------------------------------------------- input

const KEY_MAP = {
  ArrowUp: "forward",
  ArrowDown: "back",
  ArrowLeft: "left",
  ArrowRight: "right",
  KeyW: "forward",
  KeyS: "back",
  KeyA: "left",
  KeyD: "right",
  KeyQ: "strafeLeft",
  KeyE: "strafeRight",
  Space: "attack",
  KeyF: "attack",
  KeyR: "use",
  ShiftLeft: "run",
  ShiftRight: "run",
};

// What the panel calls a key, so a row reads "FORWARD" rather than "KeyW".
const KEY_LABEL = {
  ArrowUp: "FORWARD", KeyW: "FORWARD",
  ArrowDown: "BACK", KeyS: "BACK",
  ArrowLeft: "LEFT", KeyA: "LEFT",
  ArrowRight: "RIGHT", KeyD: "RIGHT",
  KeyQ: "STRAFE LEFT", KeyE: "STRAFE RIGHT",
  Space: "FIRE", KeyF: "FIRE", KeyR: "USE", ShiftLeft: "RUN", ShiftRight: "RUN",
};

const pressRows = new Map();

function keyDown(code) {
  const name = KEY_MAP[code];
  if (!name) return false;
  const first = !state.keys[name];
  state.keys[name] = true;
  if (first) pressRows.set(name, inputLog.press(name, KEY_LABEL[code] ?? name.toUpperCase()));
  // The first key of a hold starts the clock for input-to-picture: the tic it
  // is aimed at is the relayer's frontier, and the latency is the time until a
  // frame painted that is past the tic the picture was on when the key went
  // down.  A held key does not restart it, or a long hold would report only
  // the last frame's latency.
  if (state.keysDownAt === null) {
    state.keysDownAt = performance.now();
    state.gameticAtKeyDown = state.lastFrame ? state.lastFrame.gametic : -1;
  }
  return true;
}

function keyUp(code) {
  const name = KEY_MAP[code];
  if (!name) return false;
  state.keys[name] = false;
  inputLog.release(pressRows.get(name));
  pressRows.delete(name);
  return true;
}

addEventListener("keydown", (event) => {
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
  if (keyDown(event.code)) {
    event.preventDefault();
    scheduleInputPush();
  }
});

addEventListener("keyup", (event) => {
  if (keyUp(event.code)) {
    event.preventDefault();
    scheduleInputPush();
  }
});

addEventListener("blur", () => {
  for (const name of Object.keys(state.keys)) {
    state.keys[name] = false;
    inputLog.release(pressRows.get(name));
    pressRows.delete(name);
  }
  syncPad();
});

for (const button of document.querySelectorAll(".pad button")) {
  const name = button.dataset.key;
  const press = (event) => {
    event.preventDefault();
    if (!state.keys[name]) pressRows.set(name, inputLog.press(name, name.toUpperCase()));
    state.keys[name] = true;
    syncPad();
    scheduleInputPush();
  };
  const release = (event) => {
    event.preventDefault();
    state.keys[name] = false;
    inputLog.release(pressRows.get(name));
    pressRows.delete(name);
    syncPad();
    scheduleInputPush();
  };
  button.addEventListener("pointerdown", press);
  button.addEventListener("pointerup", release);
  button.addEventListener("pointercancel", release);
  button.addEventListener("pointerleave", (event) => {
    if (state.keys[name]) release(event);
  });
  button.addEventListener("contextmenu", (event) => event.preventDefault());
}

function syncPad() {
  for (const button of document.querySelectorAll(".pad button")) {
    button.dataset.held = state.keys[button.dataset.key] ? "true" : "false";
  }
}

// Aiming ahead of the producer, because a batch cannot be aimed at it.
//
// The engine reads an input slot when its cursor reaches it, and the producer
// has already fixed every tic below its generated cursor, so a batch aimed at
// the live window is a batch aimed below the producer by the time it lands.
// Measured on the first drive: the round trip from this page to the producer is
// 0.3-0.4 s, which is 7-11 tics at the measured 23.8-26.3 tics/s, and the
// window is four tics wide. 19 POSTs, 0 accepted, all 409 `input_stale`, 0
// command tics deposited against 6,116-12,416 tics consumed. Aiming at a
// window narrower than the trip cannot win, however exactly it is aimed.
//
// So the page leads the window by its own round trip, and repeats the held key
// state across an overlapping batch rather than counting the tics it has sent.
// The lead is the price: a keystroke is read by the engine one lead after it is
// signed, which is the input-to-picture latency this design implies.
const PUSH_TICS = 16;
const PUSH_INTERVAL_MS = 100;
// The lead, in tics, above the producer's NEXT tic to generate.
//
// It has to cover three distances, and the first version of this page covered
// none of them properly:
//   * the push interval, because the producer reads a reading at most one
//     cadence old;
//   * the POST round trip, because the producer keeps generating while the
//     request is in the air;
//   * the producer's own batch, `round(35 * 0.1)` = 4 tics, because it
//     snapshots a whole batch at once and a write that arrives inside that
//     window is a coin flip.
// Measured on the 2026-09-26 play session: the aim had a median of 12 tics over
// `producer_generated_cursor`, which is itself four tics ahead of the producer's
// real position, and 93.7% of the tics the chain deposited carried no key.
const PRODUCER_BATCH_TICS = 4;
const MIN_LEAD_TICS = 16;
const MAX_LEAD_TICS = 64;
// Stay this far inside the relayer's own 240-tic sequence window, so a
// mispredicted aim is a refusal the page can read a resume point from rather
// than a batch_refused for being too far ahead.
const SEQ_WINDOW_MARGIN = 40;
let pushTimer = null;
let pushesInFlight = 0;
let lastPushAt = 0;
function scheduleInputPush() {
  if (pushTimer !== null) return;
  pushTimer = requestAnimationFrame(() => {
    pushTimer = null;
    // Two batches may be in flight so the cadence is the interval rather than
    // the round trip. They cannot collide: each is aimed from its own reading
    // of the producer's position and carries its own client_seq, and both write
    // the same key state over the same tics.
    if (pushesInFlight >= MAX_PUSH_IN_FLIGHT) {
      scheduleInputPush();
      return;
    }
    const wait = lastPushAt + PUSH_INTERVAL_MS - performance.now();
    if (wait > 0) {
      pushTimer = setTimeout(() => {
        pushTimer = null;
        void pushInput();
      }, wait);
      return;
    }
    void pushInput();
  });
}

// Where the producer's next tic to generate will be when our POST reaches it.
// The number the relayer last reported is as old as that answer, and the
// producer does not stop, so the page carries it forward by its own age.
function predictedNext() {
  const observed = state.producerNextCursor ?? state.producerGeneratedCursor;
  if (observed === null || observed === undefined) return null;
  const rate = state.ticsPerSecond ?? DEFAULT_TICS_PER_SECOND;
  const ageSeconds = Math.max(0, performance.now() - state.producerReadAt) / 1000;
  // `producerGeneratedCursor` is the END of the batch the producer has already
  // published, so when that is all there is, the next tic is a batch lower.
  const base = state.producerNextCursor === null || state.producerNextCursor === undefined
    ? observed - PRODUCER_BATCH_TICS
    : observed;
  return base + Math.floor(ageSeconds * rate);
}

// The lead, in tics: the push cadence, the measured round trip and the
// producer's own batch, and never less than MIN_LEAD_TICS.
function aimLead() {
  const rate = state.ticsPerSecond ?? DEFAULT_TICS_PER_SECOND;
  const tripTics = Math.ceil(((state.roundTripMs ?? 0) / 1000) * rate);
  const cadenceTics = Math.ceil((PUSH_INTERVAL_MS / 1000) * rate);
  return Math.max(MIN_LEAD_TICS, Math.min(MAX_LEAD_TICS, tripTics + cadenceTics + PRODUCER_BATCH_TICS));
}

async function pushInput() {
  if (!state.session || !state.wallet) return;
  const held = Object.values(state.keys).some(Boolean);
  if (held && state.producerReadAt === 0) {
    // The first batch of a hold has no producer position of its own, and the
    // one the session was opened with is as old as the join.
    await readWindow();
  }
  const next = predictedNext();
  // The floor is the producer's next tic to generate: a write at or below it
  // names tics the producer is about to snapshot, so its command is either
  // ignored or a coin flip. The chain cursor is a second floor, because a tic
  // the engine has read is gone whatever the producer says. The page's own last
  // batch is deliberately not a floor: the batches overlap and repeat the same
  // keys, so a floor there would walk the aim up faster than the engine runs
  // and build the backlog the relayer's lead bound exists to prevent.
  const floor = Math.max(
    state.cursor,
    next === null ? 0 : next,
  );
  if (!held || floor <= 0) {
    // No keys, or nothing has been read yet: the next frame offers again.
    // Nothing is lost by waiting, because a tic nobody writes is a tic with no
    // keys held.
    if (held) scheduleInputPush();
    return;
  }
  const lead = aimLead();
  const ceiling = Math.max(floor, state.cursor + state.seqWindow - SEQ_WINDOW_MARGIN);
  const firstSeq = Math.min(floor + lead, ceiling);
  const count = PUSH_TICS;
  state.leadTics = firstSeq - floor;
  state.aimedAt = firstSeq;
  const commands = heldToCommands(state.keys, firstSeq, count);
  state.clientSeq += 1;
  const message = encodeInput({
    slot: state.session.slot,
    firstSeq,
    commands,
    clientSeq: state.clientSeq,
    issuedAt: Math.floor(Date.now() / 1000),
    binding: state.session.binding,
  });
  const signature = await state.wallet.sign(message);
  const sentAt = performance.now();
  lastPushAt = sentAt;
  pushesInFlight += 1;
  state.batchesSigned += 1;
  try {
    const response = await fetch(`${state.relayerBase}/v1/sessions/${encodeURIComponent(state.session.id)}/input`, {
      method: "POST",
      headers: {"content-type": "application/json"},
      body: JSON.stringify({message: toBase64(message), signature: toBase64(signature)}),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      // A refusal that names a resume point is not a failure to show, it is the
      // relayer saying where the chain is. Take its word for the sequence and
      // offer again. The panel still records it: a keystroke the relayer would
      // not take is a fate the player is entitled to see.
      state.refusals += 1;
      inputLog.noteBatch({
        firstSeq,
        count,
        commands,
        refused: true,
        reason: payload.error ?? payload.detail?.reason ?? `relayer returned ${response.status}`,
        at: sentAt,
      });
      renderInputLog();
      if (payload.detail?.next_seq !== undefined) {
        if (payload.detail.cursor !== undefined) state.cursor = payload.detail.cursor;
        if (payload.detail.staging_frontier !== undefined) state.stagingFrontier = payload.detail.staging_frontier;
        if (payload.detail.input_window !== undefined) state.inputWindow = payload.detail.input_window;
        if (payload.detail.producer_generated_cursor !== undefined) {
          state.producerGeneratedCursor = payload.detail.producer_generated_cursor;
          // Only stamp the reading as fresh when the answer also carried the
          // next cursor the aim is built from. Stamping it without that left the
          // prediction frozen at an old cursor while claiming to be current, so
          // the aim walked backwards into tics the producer had already read.
          if (payload.detail.producer_next_cursor !== undefined && payload.detail.producer_next_cursor !== null) {
            state.producerNextCursor = payload.detail.producer_next_cursor;
            state.producerReadAt = performance.now();
          }
        }
        if (payload.detail.producer_stage_limit !== undefined) {
          state.producerStageLimit = payload.detail.producer_stage_limit;
        }
        // A batch the producer has already passed says exactly how far short
        // the aim was, and the producer's own position is the number to aim
        // from next time. The lead grows by the shortfall; it is the measured
        // round trip plus whatever the round trip under-measured.
        if (payload.detail.producer_generated_cursor !== undefined) {
          const short = payload.detail.producer_generated_cursor - firstSeq + 1;
          if (short > 0) state.leadTics = Math.min(MAX_LEAD_TICS, Math.max(state.leadTics ?? 0, short + MIN_LEAD_TICS));
        }
        // Take the relayer's word for the sequence to write next. It is the
        // producer's own position, which is the only side that knows it; the
        // chain cursor is below it and a floor there is a refusal.
        state.nextSeq = Math.max(state.nextSeq, payload.detail.next_seq);
        if (held) scheduleInputPush();
        return;
      }
      throw new Error(payload.error ?? `relayer returned ${response.status}`);
    }
    adoptAccounts(payload);
    inputLog.noteBatch({
      firstSeq,
      count,
      commands,
      accepted: payload.accepted ?? count,
      acceptedFrom: payload.accepted_from ?? firstSeq,
      trimmedFromSeq: payload.trimmed_from_seq ?? null,
      queuedAboveStageLimit: payload.queued_above_stage_limit ?? null,
      at: sentAt,
    });
    renderInputLog();
    // The batch's own end, so the next one overlaps it rather than leaving a
    // hole. It is not a floor: the aim is the producer's position plus the
    // lead, and this is only here to say how much of the window is written.
    state.nextSeq = Math.max(state.nextSeq, firstSeq + count);
    state.inputsSent += count;
    const elapsed = performance.now() - sentAt;
    state.latencies.push(elapsed);
    if (state.latencies.length > 40) state.latencies.shift();
    // The round trip the lead is built from is the worst of the recent ones:
    // the aim has to survive the slow request, not the median one.
    state.roundTripMs = Math.max(...state.latencies.slice(-8));
    const median = [...state.latencies].sort((a, b) => a - b)[Math.floor(state.latencies.length / 2)];
    setCell(ui.inputLatency, `${median.toFixed(0)} ms · lead ${state.leadTics}`);
    setCell(ui.inputsSent, String(state.inputsSent));
    if (payload.frame?.signature) state.signature = payload.frame.signature;
    clearError("action");
  } catch (error) {
    showError(error, "action");
  } finally {
    pushesInFlight -= 1;
    // Held keys keep offering, on the interval, for as long as they are down.
    // This is the loop the R8l6 session was missing: one batch per keydown is
    // a nudge, not a walk.
    if (Object.values(state.keys).some(Boolean)) scheduleInputPush();
  }
}

// ---------------------------------------------------------------- relayer

async function relayer(path, options = {}) {
  const response = await fetch(`${state.relayerBase}${path}`, options);
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error ?? `relayer returned ${response.status}`);
  return payload;
}

async function joinSession() {
  if (!state.wallet) throw new Error("the burner key is not ready yet");
  const base = ui.relayerUrl.value.trim().replace(/\/+$/, "");
  if (!base) throw new Error("set RELAYER to the relayer URL first");
  state.relayerBase = base;
  try {
    localStorage.setItem(RELAYER_KEY, base);
  } catch {
    /* not persisting the relayer URL is fine */
  }
  const session = await relayer("/v1/sessions", {
    method: "POST",
    headers: {"content-type": "application/json"},
    body: JSON.stringify({
      publicKey: state.wallet.publicKeyHex,
      network: networkName,
      buffer: state.buffer,
    }),
  });
  const binding = fromHex(session.binding);
  if (!binding) {
    throw new Error(`the relayer returned a slot binding that is not 32 bytes of hex: ${session.binding}`);
  }
  state.session = {...session, binding};
  state.signature = session.frame?.signature ?? null;
  adoptSession(session);
  rememberSession(localStorage, {
    id: session.id,
    slot: session.slot,
    base,
    buffer: state.buffer,
    publicKey: state.wallet.publicKeyHex,
    binding: toHex(session.binding),
  });
  await pollSessionStatus();
  scheduleInputPush();
}

// Puts the page into playing shape for a session, whether it was just created
// or picked back up after a reload.
function adoptSession(session) {
  adoptAccounts(session);
  setCell(ui.sessionState, `PLAYING · ${session.id.slice(0, 8)}`, "good");
  setCell(ui.slotCell, String(session.slot));
  ui.join.disabled = true;
  ui.leave.disabled = false;
  ui.spectatorHint.hidden = true;
}

// The session's chain accounts, not network.js's. The relayer drives whichever
// context the operator provisioned, and a page that showed the default one
// beside the session's own strips and snapshot was describing a deployment
// nobody was watching.
function adoptAccounts(payload) {
  if (!payload) return;
  if (payload.strip_accounts?.length === 8) net.doom.strips[state.buffer] = payload.strip_accounts;
  if (payload.snapshot_account) net.doom.snapshots[state.buffer] = payload.snapshot_account;
  if (payload.context && payload.context !== net.doom.context) {
    net.doom.context = payload.context;
    describeChain();
  }
  if (payload.cursor !== undefined) state.cursor = payload.cursor;
  if (payload.staging_frontier !== undefined) state.stagingFrontier = payload.staging_frontier;
  if (payload.input_window !== undefined && payload.input_window !== null) state.inputWindow = payload.input_window;
  if (payload.producer_generated_cursor !== undefined) {
    state.producerGeneratedCursor = payload.producer_generated_cursor;
  }
  if (payload.producer_next_cursor !== undefined && payload.producer_next_cursor !== null) {
    state.producerNextCursor = payload.producer_next_cursor;
  }
  if (payload.producer_stage_limit !== undefined) {
    state.producerStageLimit = payload.producer_stage_limit;
  }
  if (payload.pipeline !== undefined && payload.pipeline !== null) state.pipeline = payload.pipeline;
  if (payload.input_fate !== undefined && payload.input_fate !== null) state.inputFate = payload.input_fate;
  // Every answer that carries the producer's position is stamped with when it
  // arrived, because the aim carries it forward by its age.
  if (payload.producer_generated_cursor !== undefined) state.producerReadAt = performance.now();
  if (payload.tics_per_second) state.ticsPerSecond = payload.tics_per_second;
  // The relayer's pointer to the buffer holding the newest complete frame,
  // with that buffer's accounts: the page reads this buffer rather than the one
  // the pipeline is writing.
  if (payload.latest_frame?.buffer !== undefined && payload.latest_frame?.buffer !== null) {
    state.framePointer = payload.latest_frame;
    if (Array.isArray(payload.latest_frame.strip_accounts)) {
      net.doom.strips[payload.latest_frame.buffer] = payload.latest_frame.strip_accounts;
    }
    if (payload.latest_frame.snapshot_account) {
      net.doom.snapshots[payload.latest_frame.buffer] = payload.latest_frame.snapshot_account;
    }
  }
  if (payload.frame_counters) state.frameCounters = payload.frame_counters;
}

// A fresh cursor and staging frontier. The relayer reads the chain for this, so
// it is the only honest way for the page to learn where input may be written.
async function readWindow() {
  if (!state.session) return;
  const status = await relayer(`/v1/sessions/${encodeURIComponent(state.session.id)}`);
  adoptAccounts(status);
  state.nextSeq = Math.max(state.nextSeq, status.cursor ?? 0);
}


async function pollSessionStatus() {
  if (!state.session) return;
  try {
    const status = await relayer(`/v1/sessions/${encodeURIComponent(state.session.id)}`);
    adoptAccounts(status);
    state.nextSeq = Math.max(state.nextSeq, status.cursor ?? 0);
    setCell(ui.slotCell, String(status.slot));
    // Only replace the latency readout when the relayer actually knows a rate.
    // Before the first batch of a hold the panel shows the engine rate; after
    // it, the panel shows the round trip and the lead, which is the pair a
    // player reads to know whether their keystrokes are landing.
    if (status.tics_per_second && !state.latencies.length) {
      setCell(ui.inputLatency, `${status.tics_per_second.toFixed(1)} tic/s`);
    }
    if (status.frame?.signature) state.signature = status.frame.signature;
    if (status.state && status.state !== "playing") {
      setCell(ui.sessionState, status.state.toUpperCase(), "warn");
      inputLog.close(`the session is ${status.state}`);
    }
    // Everything the panel needs, on a poll that was already happening.
    inputLog.observe(status);
    renderInputLog();
    clearError();
  } catch (error) {
    // The relayer is not answering. That is the loudest thing that can happen to
    // a page whose job is to show whether keys are landing, so the panel says so
    // rather than leaving a frozen picture and a silent keyboard.
    inputLog.close(error instanceof Error ? `${error.name}: ${error.message}` : String(error));
    renderInputLog();
    showError(error, "action");
  }
}

async function leaveSession() {
  if (!state.session) return;
  try {
    await relayer(`/v1/sessions/${encodeURIComponent(state.session.id)}`, {method: "DELETE"});
  } catch (error) {
    showError(error, "action");
  }
  state.session = null;
  inputLog.reset();
  renderInputLog();
  forgetSession(localStorage);
  setCell(ui.sessionState, "SPECTATING", "warn");
  setCell(ui.slotCell, "—");
  setCell(ui.inputLatency, "—");
  ui.join.disabled = false;
  ui.leave.disabled = true;
  ui.spectatorHint.hidden = false;
}

ui.join.addEventListener("click", () => {
  ui.join.disabled = true;
  joinSession().catch((error) => {
    showError(error, "action");
    ui.join.disabled = false;
  });
});

ui.leave.addEventListener("click", () => {
  void leaveSession();
});

ui.bufferSelect.addEventListener("change", () => {
  state.buffer = Number(ui.bufferSelect.value) || 0;
  try {
    localStorage.setItem(BUFFER_KEY, String(state.buffer));
  } catch {
    /* not persisting the buffer choice is fine */
  }
  setCell(ui.railState, "READING");
  void readOnce();
});

// ---------------------------------------------------------------- boot

// Boot-time notes go to the console; the page has a status bar for the rest.
const pageLog = (...parts) => console.info("[doom-play]", ...parts);

function describeChain() {
  const doom = net.doom;
  ui.chainProgram.replaceChildren(link(shorten(doom.program, 8, 8), explorerFor(doom.program)));
  ui.chainContext.replaceChildren(link(shorten(doom.context, 8, 8), explorerFor(doom.context)));
  ui.chainSnapshot.replaceChildren(link(shorten(doom.snapshots[state.buffer] ?? "—", 8, 8), explorerFor(doom.snapshots[state.buffer])));
  ui.chainCapacity.textContent = doom.inputCapacity ? `${doom.inputCapacity.toLocaleString()} slots` : "—";
  ui.hintCapacity.textContent = doom.inputCapacity ? doom.inputCapacity.toLocaleString() : "all";
  ui.networkBadge.textContent = net.label;
  ui.commitmentStamp.textContent = `PAINT AT ${net.commitment.paint.toUpperCase()} · RECONCILE AT ${net.commitment.reconcile.toUpperCase()}`;
  ui.statCommitment.textContent = net.commitment.paint;
}

async function boot() {
  ui.bufferSelect.value = String(state.buffer);
  try {
    ui.relayerUrl.value = localStorage.getItem(RELAYER_KEY) ?? query.get("relayer") ?? "";
  } catch {
    ui.relayerUrl.value = query.get("relayer") ?? "";
  }
  describeChain();

  state.reader = new ChainReader({network: networkName});
  setCell(ui.railState, "READING");

  if (!ed25519Available()) {
    setCell(ui.burnerKey, "UNSUPPORTED BROWSER", "bad");
    ui.join.disabled = true;
    showError(new Error("this browser has no WebCrypto Ed25519, so inputs cannot be signed here"));
  } else {
    const {wallet, created} = await loadOrCreateBurner();
    state.wallet = wallet;
    state.burnerCreated = created;
    setCell(ui.burnerKey, wallet.shortFingerprint, created ? "warn" : "");
    ui.burnerKey.title = created ? "made in this tab just now" : "loaded from localStorage";
  }

  try {
    const source = await loadPalette();
    setCell(ui.stageState, `PALETTE FROM ${source.toUpperCase()}`);
  } catch (error) {
    setCell(ui.stageState, "NO PALETTE", "bad");
    showError(error);
    return;
  }

  startPolling();

  // A reload presents the same burner key, so try to pick the session back up
  // before making the player press JOIN again.
  const remembered = recallSession(localStorage);
  if (remembered?.id && remembered.publicKey === state.wallet.publicKeyHex) {
    state.relayerBase = remembered.base;
    ui.relayerUrl.value = remembered.base;
    if (remembered.buffer !== undefined) {
      state.buffer = remembered.buffer;
      ui.bufferSelect.value = String(state.buffer);
    }
    try {
      const status = await relayer(`/v1/sessions/${encodeURIComponent(remembered.id)}`);
      if (status.state === "playing") {
        state.session = {...status, binding: fromHex(remembered.binding ?? "") ?? null};
        if (!state.session.binding) {
          const fresh = await relayer("/v1/sessions", {
            method: "POST",
            headers: {"content-type": "application/json"},
            body: JSON.stringify({publicKey: state.wallet.publicKeyHex, network: networkName, buffer: state.buffer}),
          });
          state.session = {...fresh, binding: fromHex(fresh.binding)};
        }
        adoptSession(state.session);
        pageLog(`re-attached to session ${status.id.slice(0, 8)} on slot ${status.slot}`);
      }
    } catch {
      // The relayer is gone or the session ended. Start clean.
      forgetSession(localStorage);
    }
  }

  setInterval(() => void pollSessionStatus(), 2000);
  // The status tick refreshes the panel and nothing else: the rate on the
  // panel comes from the paints themselves (see `fps`).
  setInterval(() => {
    if (state.lastFrame) updateStatus();
  }, 500);
  addEventListener("resize", () => {
    if (state.lastFrame) void paintFrame(state.lastFrame);
  });
}

// Exposed for the offline tests and for poking at a live page from a console.
globalThis.doomPlay = {state, net, readOnce, paintFrame, base58ToBytes, bytesToBase58, inputLog, renderInputLog};

void boot().catch((error) => showError(error));
