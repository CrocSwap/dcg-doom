// Doom on Fogo: the viewer.
//
// This page never signs or sends a transaction. It reads the eight strip
// accounts of a frame, paints the 320x200 palette frame to a canvas, and
// signs input batches with a throwaway key so the relayer can attribute them
// to a player. Everything network-specific is in network.js.

import {DEFAULT_NETWORK, NETWORKS, explorerAddress, explorerTransaction, requireNetwork} from "./network.js";
import {ChainReader, WadImageReader, readPlaypal, base58ToBytes, bytesToBase58} from "./rpc.js";
import {decodeFrame, frameToRgba, toHex, StripError, FRAME_WIDTH, FRAME_HEIGHT} from "./strip.js";
import {loadOrCreateBurner, forgetSession, recallSession, rememberSession, ed25519Available} from "./burner.js";
import {encodeInput, heldToCommands, slotBinding, MAX_COMMANDS_PER_BATCH} from "./input.js";

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

function showError(error) {
  const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  if (text === state.lastError) return;
  state.lastError = text;
  ui.error.textContent = text;
  ui.error.hidden = false;
}

function clearError() {
  state.lastError = null;
  ui.error.hidden = true;
}

const shorten = (text, head = 6, tail = 6) =>
  !text ? "—" : text.length <= head + tail + 1 ? text : `${text.slice(0, head)}…${text.slice(-tail)}`;

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
  updateStatus();
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
  setCell(ui.statSlot, state.lastFrameSlot === null ? "—" : String(state.lastFrameSlot));
  setCell(ui.statCommitment, state.reader?.paintCommitment ?? net.commitment.paint, "good");

  const explorer = state.signature
    ? explorerTransaction(net, state.signature)
    : explorerAddress(net, net.doom.snapshots[state.buffer]);
  ui.statExplorer.textContent = state.signature ? shorten(state.signature, 10, 8) : "snapshot account ↗";
  if (explorer) {
    ui.statExplorer.href = explorer;
    ui.statExplorer.title = explorer;
  } else {
    ui.statExplorer.removeAttribute("href");
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

async function readOnce() {
  const addresses = net.doom.strips[state.buffer];
  if (!addresses || addresses.length !== 8) {
    showError(new Error(`buffer ${state.buffer} has no strip accounts configured`));
    return;
  }
  const {slot, payloads} = await state.reader.getAccounts(addresses);
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

// One batch per animation frame while keys are held. Vanilla Doom runs at 35
// tics a second; the relayer decides how many of those tics it can actually
// land, and drops from the front if the slot falls behind.
let pushTimer = null;
function scheduleInputPush() {
  if (pushTimer !== null) return;
  pushTimer = requestAnimationFrame(() => {
    pushTimer = null;
    void pushInput();
  });
}

async function pushInput() {
  if (!state.session || !state.wallet) return;
  const held = Object.values(state.keys).some(Boolean);
  if (!held && state.nextSeq === 0) return;
  const count = held ? 8 : 1;
  const commands = heldToCommands(state.keys, state.nextSeq, count);
  state.clientSeq += 1;
  const message = encodeInput({
    slot: state.session.slot,
    firstSeq: state.nextSeq,
    commands,
    clientSeq: state.clientSeq,
    issuedAt: Math.floor(Date.now() / 1000),
    binding: state.session.binding,
  });
  const signature = await state.wallet.sign(message);
  const base64 = (bytes) => btoa(String.fromCharCode(...bytes));
  const sentAt = performance.now();
  try {
    const response = await fetch(`${state.relayerBase}/v1/sessions/${encodeURIComponent(state.session.id)}/input`, {
      method: "POST",
      headers: {"content-type": "application/json"},
      body: JSON.stringify({message: base64(message), signature: base64(signature)}),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.error ?? `relayer returned ${response.status}`);
    state.nextSeq = payload.next_seq ?? state.nextSeq + count;
    state.inputsSent += count;
    state.latencies.push(performance.now() - sentAt);
    if (state.latencies.length > 40) state.latencies.shift();
    const median = [...state.latencies].sort((a, b) => a - b)[Math.floor(state.latencies.length / 2)];
    setCell(ui.inputLatency, `${median.toFixed(0)} ms`);
    setCell(ui.inputsSent, String(state.inputsSent));
    if (payload.frame?.signature) state.signature = payload.frame.signature;
    clearError();
  } catch (error) {
    showError(error);
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
  state.session = session;
  state.nextSeq = session.next_seq ?? 0;
  state.signature = session.frame?.signature ?? null;
  if (session.strip_accounts?.length) net.doom.strips[state.buffer] = session.strip_accounts;
  if (session.snapshot_account) net.doom.snapshots[state.buffer] = session.snapshot_account;
  rememberSession(localStorage, {id: session.id, slot: session.slot, base, buffer: state.buffer, publicKey: state.wallet.publicKeyHex});
  setCell(ui.sessionState, `PLAYING · ${session.id.slice(0, 8)}`, "good");
  setCell(ui.slotCell, String(session.slot));
  ui.join.disabled = true;
  ui.leave.disabled = false;
  ui.spectatorHint.hidden = true;
  await pollSessionStatus();
  scheduleInputPush();
}

async function pollSessionStatus() {
  if (!state.session) return;
  try {
    const status = await relayer(`/v1/sessions/${encodeURIComponent(state.session.id)}`);
    state.nextSeq = status.next_seq ?? state.nextSeq;
    setCell(ui.slotCell, String(status.slot));
    setCell(ui.inputLatency, status.tics_per_second ? `${status.tics_per_second.toFixed(1)} tic/s` : "—");
    if (status.frame?.signature) state.signature = status.frame.signature;
    if (status.strip_accounts?.length === 8) net.doom.strips[state.buffer] = status.strip_accounts;
    if (status.state && status.state !== "playing") {
      setCell(ui.sessionState, status.state.toUpperCase(), "warn");
    }
    clearError();
  } catch (error) {
    showError(error);
  }
}

async function leaveSession() {
  if (!state.session) return;
  try {
    await relayer(`/v1/sessions/${encodeURIComponent(state.session.id)}`, {method: "DELETE"});
  } catch (error) {
    showError(error);
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
    showError(error);
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

function describeChain() {
  const doom = net.doom;
  ui.chainProgram.replaceChildren(link(shorten(doom.program, 8, 8), explorerAddress(net, doom.program)));
  ui.chainContext.replaceChildren(link(shorten(doom.context, 8, 8), explorerAddress(net, doom.context)));
  ui.chainSnapshot.replaceChildren(
    link(shorten(doom.snapshots[state.buffer] ?? "—", 8, 8), explorerAddress(net, doom.snapshots[state.buffer] ?? "")),
  );
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
