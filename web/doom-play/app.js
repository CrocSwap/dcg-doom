// Doom on Fogo: the viewer.
//
// This page never signs or sends a transaction. It reads the eight strip
// accounts of a frame, paints the 320x200 palette frame to a canvas, and
// signs input batches with a throwaway key so the relayer can attribute them
// to a player. Everything network-specific is in network.js.

import {DEFAULT_NETWORK, explorerAddress, explorerTransaction, requireNetwork} from "./network.js";
import {ChainReader, WadImageReader, readPlaypal, base58ToBytes, bytesToBase58} from "./rpc.js";
import {decodeFrame, frameToRgba, toHex, StripError, FRAME_WIDTH, FRAME_HEIGHT} from "./strip.js";
import {loadOrCreateBurner, forgetSession, recallSession, rememberSession, ed25519Available} from "./burner.js";
import {encodeInput, heldToCommands} from "./input.js";

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
  frameCounters: null,
  // A key going down, and the first frame painted after it. That pair is the
  // input-to-picture latency a player feels, and it is the only one of the
  // page's numbers that says whether the game is responding.
  keysDownAt: null,
  pictureLatencies: [],
  clientSeq: 0,
  inputsSent: 0,
  latencies: [],
  lastFrame: null,
  lastFrameSlot: null,
  paintedFrames: 0,
  paintedAt: 0,
  frameTimes: [],
  signature: null,
  keys: {},
  stopPolling: null,
  lastError: null,
};

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

function fps() {
  if (state.frameTimes.length < 2) return null;
  const span = state.frameTimes[state.frameTimes.length - 1] - state.frameTimes[0];
  if (span <= 0) return null;
  return ((state.frameTimes.length - 1) * 1000) / span;
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
    payloads: payload.strips.map((encoded) => {
      const binary = atob(encoded);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
      return bytes;
    }),
  };
}

async function readOnce() {
  const addresses = net.doom.strips[state.buffer];
  if (!addresses || addresses.length !== 8) {
    showError(new Error(`buffer ${state.buffer} has no strip accounts configured`));
    return;
  }
  let slot = null;
  let payloads = null;
  try {
    ({slot, payloads} = await state.reader.getAccounts(addresses));
  } catch (error) {
    const relayed = await readFromRelayer().catch(() => null);
    if (!relayed) throw error;
    ({slot, payloads} = relayed);
    setCell(ui.stageState, "MOCK MODE · RELAYER FRAME");
  }
  const present = payloads.filter(Boolean).length;
  ui.stripSummary.textContent = `${present} / 8`;
  for (let i = 0; i < 8; i += 1) {
    const ok = Boolean(payloads[i]);
    stripChips[i].classList.toggle("ok", ok);
    if (ok) stripChips[i].title = `strip ${i} present`;
  }
  if (present < 8) {
    // A frame is all eight strips or nothing. Keep the last good one.
    setCell(ui.railState, "PARTIAL");
    return;
  }
  const frame = await decodeFrame(payloads);
  frame.digest = await digestHex(frame.pixels);
  await paintFrame(frame);
  state.lastFrameSlot = slot;
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
    try {
      await readOnce();
    } catch (error) {
      // A torn frame is expected while a new frame is landing. Report the
      // first distinct reason and keep the last good frame on screen.
      if (error instanceof StripError) setCell(ui.railState, "TORN");
      showError(error);
    }
    if (stopped) return;
    // Frames land at roughly 1-2/s on the measured deployment. Poll a bit
    // faster than that so a new frame shows up promptly, and back off on
    // repeated failure rather than hammering a struggling node.
    const delay = state.lastError ? 3000 : 700;
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

function keyDown(code) {
  const name = KEY_MAP[code];
  if (!name) return false;
  state.keys[name] = true;
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
  for (const name of Object.keys(state.keys)) state.keys[name] = false;
  syncPad();
});

for (const button of document.querySelectorAll(".pad button")) {
  const name = button.dataset.key;
  const press = (event) => {
    event.preventDefault();
    state.keys[name] = true;
    syncPad();
    scheduleInputPush();
  };
  const release = (event) => {
    event.preventDefault();
    state.keys[name] = false;
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

// One batch per animation frame while keys are held, placed at the front of
// the relayer's staging window. Vanilla Doom runs at 35 tics/s; the engine reads
// an input slot when its cursor reaches it and the producer has already
// deposited every tic below its own staging limit, so the only tics a keypress
// can still reach are the handful just behind the relayer's frontier. A page
// that instead counted the tics it had sent drifted behind the engine at
// 35 tics/s, and after ten seconds of no keys every batch it offered named a
// tic the chain had already consumed: inputs accepted, player still.
//
// Only one push is ever in flight. Two overlapping pushes would both read the
// same nextSeq and send the same firstSeq, and the relayer would rightly
// refuse the second, so the keys a player is holding would silently stop
// registering.
const MAX_PUSH_TICS = 8;
let pushTimer = null;
let pushInFlight = false;
function scheduleInputPush() {
  if (pushTimer !== null) return;
  pushTimer = requestAnimationFrame(() => {
    pushTimer = null;
    if (pushInFlight) {
      // Try again on the next frame rather than dropping the keystroke.
      scheduleInputPush();
      return;
    }
    void pushInput();
  });
}

async function pushInput() {
  if (!state.session || !state.wallet) return;
  const held = Object.values(state.keys).some(Boolean);
  if (held) {
    // A fresh window before every signed batch. The frontier moves with the
    // cursor, so a window read a status poll ago is already tens of tics out
    // of date and the batch would be signed for slots the engine has read.
    await readWindow();
  }
  // The floor is the relayer's own next sequence, floored at the chain's
  // cursor: never rewind, and never offer a tic the chain has consumed.
  //
  // And floored one past the producer's generated cursor, which is the bound
  // that actually decides the batch's fate: a write at or below it names tics
  // DEPOSIT has already fixed, so it is refused as `input_stale` with the
  // sequence to resume at. The producer generates at 35 tics/s against an engine
  // at 26, so it runs ahead of the chain cursor by 12-16 tics; a floor at the
  // cursor therefore aimed every batch at a sequence the engine had already
  // read, and the page held a key for a minute with 21 refusals and 0 tics
  // deposited. The writable window is (generated_cursor, stage_limit].
  const generated = state.producerGeneratedCursor;
  const floor = Math.max(state.nextSeq, state.cursor, generated === null || generated === undefined ? -1 : generated + 1);
  state.nextSeq = floor;
  const frontier = state.stagingFrontier ?? floor;
  // Aim at the live window, which is the last few tics before the frontier. The
  // producer has already deposited everything below that, so a batch anywhere
  // else in the staging window is accepted, counted and never read by the game.
  // The relayer measures the window; the page only has to point at it.
  const width = Math.max(1, Math.min(state.inputWindow ?? MAX_PUSH_TICS, frontier - floor));
  if (!held || frontier <= floor) {
    // No keys, or the frontier has not moved: the next frame offers again.
    // Nothing is lost by waiting, because a tic nobody writes is a tic with no
    // keys held.
    if (held) scheduleInputPush();
    return;
  }
  const firstSeq = Math.max(floor, frontier - width);
  const count = width;
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
  pushInFlight = true;
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
      // offer again.
      if (payload.detail?.next_seq !== undefined) {
        if (payload.detail.cursor !== undefined) state.cursor = payload.detail.cursor;
        if (payload.detail.staging_frontier !== undefined) state.stagingFrontier = payload.detail.staging_frontier;
        if (payload.detail.input_window !== undefined) state.inputWindow = payload.detail.input_window;
        if (payload.detail.producer_generated_cursor !== undefined) {
          state.producerGeneratedCursor = payload.detail.producer_generated_cursor;
        }
        if (payload.detail.producer_stage_limit !== undefined) {
          state.producerStageLimit = payload.detail.producer_stage_limit;
        }
        // Take the relayer's word for the sequence to write next. It is the
        // producer's own position, which is the only side that knows it; the
        // chain cursor is below it and a floor there is a refusal.
        state.nextSeq = payload.detail.next_seq;
        if (held) scheduleInputPush();
        return;
      }
      throw new Error(payload.error ?? `relayer returned ${response.status}`);
    }
    adoptAccounts(payload);
    // The next tic to write is the one after the batch just deposited. The
    // relayer's own next_seq is not read back here: it counts accepted batches
    // and would drag the aim above the live window, where a write is accepted
    // and never read.
    state.nextSeq = firstSeq + count;
    state.inputsSent += count;
    state.latencies.push(performance.now() - sentAt);
    if (state.latencies.length > 40) state.latencies.shift();
    const median = [...state.latencies].sort((a, b) => a - b)[Math.floor(state.latencies.length / 2)];
    setCell(ui.inputLatency, `${median.toFixed(0)} ms`);
    setCell(ui.inputsSent, String(state.inputsSent));
    if (payload.frame?.signature) state.signature = payload.frame.signature;
    clearError("action");
  } catch (error) {
    showError(error, "action");
  } finally {
    pushInFlight = false;
    // Held keys keep offering, one batch per frame, for as long as they are
    // down. This is the loop the R8l6 session was missing: one batch per
    // keydown is eight tics, which is a nudge, not a walk.
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
  if (payload.producer_stage_limit !== undefined) {
    state.producerStageLimit = payload.producer_stage_limit;
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
    if (status.tics_per_second) setCell(ui.inputLatency, `${status.tics_per_second.toFixed(1)} tic/s`);
    if (status.frame?.signature) state.signature = status.frame.signature;
    if (status.state && status.state !== "playing") {
      setCell(ui.sessionState, status.state.toUpperCase(), "warn");
    }
    clearError();
  } catch (error) {
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
  setInterval(() => {
    const now = performance.now();
    state.frameTimes.push(now);
    while (state.frameTimes.length && now - state.frameTimes[0] > 4000) state.frameTimes.shift();
    if (state.lastFrame) updateStatus();
  }, 500);
  addEventListener("resize", () => {
    if (state.lastFrame) void paintFrame(state.lastFrame);
  });
}

// Exposed for the offline tests and for poking at a live page from a console.
globalThis.doomPlay = {state, net, readOnce, paintFrame, base58ToBytes, bytesToBase58};

void boot().catch((error) => showError(error));
