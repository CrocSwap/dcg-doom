import {loadOrCreateBurner} from "../doom-play/burner.js";
import {encodeInput} from "../doom-play/input.js";
import {decodePushedFrame} from "../doom-play/push.js";
import {frameToRgba, FRAME_WIDTH, FRAME_HEIGHT} from "../doom-play/strip.js";

const query = new URLSearchParams(location.search);
const relayerBase = (query.get("relayer") ?? "http://127.0.0.1:8787").replace(/\/+$/, "");
const $ = (id) => document.getElementById(id);
const ui = {
  connection: $("connection"), playing: $("playing"), turn: $("turn"), idle: $("idle"), viewers: $("viewers"),
  frame: $("frame"), spend: $("spend"), place: $("place"), banner: $("banner"), error: $("error"),
  take: $("take-control"), join: $("join"), canvas: $("screen"),
};
const held = {forward:false, back:false, left:false, right:false, strafeLeft:false, strafeRight:false, attack:false, use:false, run:false};
const keyMap = {
  ArrowUp:"forward", KeyW:"forward", ArrowDown:"back", KeyS:"back", ArrowLeft:"left", KeyA:"left",
  ArrowRight:"right", KeyD:"right", KeyQ:"strafeLeft", KeyE:"strafeRight", Space:"attack", KeyF:"attack",
  KeyR:"use", ShiftLeft:"run", ShiftRight:"run",
};
let wallet = null;
let token = null;
let session = null;
let joining = false;
let publicState = null;
let palette = null;
let clientSeq = 0;
let eventSource = null;
let heartbeatTimer = null;
let inputTimer = null;
let inputBusy = false;
let inputPending = false;
let lastInputAt = 0;
let painted = 0;
const pointerKeys = new Map();
let inputInFlight = Promise.resolve();

function hexBytes(text) {
  const bytes = new Uint8Array(text.length / 2);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return bytes;
}

function bytesToBase64(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

async function api(path, {method = "GET", body = null} = {}) {
  const response = await fetch(`${relayerBase}${path}`, {
    method,
    headers: body ? {"content-type":"application/json"} : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error ?? `relayer returned ${response.status}`);
  return payload;
}

function showError(message) {
  ui.error.textContent = message;
  ui.error.hidden = false;
}

function clearError() {
  ui.error.hidden = true;
  ui.error.textContent = "";
}

function drawBanner(message, kind = "") {
  ui.banner.textContent = message ?? "";
  ui.banner.className = `banner ${kind}`.trim();
  ui.banner.hidden = !message;
}

function renderState(value) {
  publicState = value;
  ui.connection.textContent = "LIVE · SHARED VIEW";
  ui.connection.parentElement.classList.add("online");
  ui.playing.textContent = value.active_player ?? "No player";
  ui.turn.textContent = value.is_player ? "YOU" : value.offer?.mine ? "OFFER" : "SPECTATING";
  ui.idle.textContent = value.idle_seconds == null ? "—" : `${value.idle_seconds}s`;
  ui.viewers.textContent = `${value.viewers} / ${value.max_viewers}`;
  ui.frame.textContent = value.frame_index == null ? "Waiting" : `#${value.frame_index} · tic ${value.cursor ?? "—"}`;
  ui.spend.textContent = `${Number(value.spend_fogo ?? 0).toFixed(4)} / ${value.spend_cap_fogo} FOGO`;
  if (value.is_player) ui.place.textContent = "You are playing · control sends only when you press a mapped key";
  else if (value.offer?.mine) ui.place.textContent = `Your offer · ${value.offer.seconds} seconds to take control`;
  else if (Number.isFinite(value.spectator_place)) ui.place.textContent = `Spectator place ${value.spectator_place}`;
  else ui.place.textContent = "Waiting for a player";
  ui.take.hidden = !value.offer?.mine;
  ui.take.textContent = `TAKE CONTROL · ${value.offer?.seconds ?? 5}s`;
  ui.join.hidden = false;
  ui.join.disabled = Boolean(session);
  if (value.death_message) drawBanner(value.death_message, "death");
  else if (value.pause_message) drawBanner(value.pause_message);
  else if (value.offer?.mine) drawBanner(`Your turn is ready. Take control within ${value.offer.seconds} seconds.`);
  else if (!painted) drawBanner("Waiting for the first pushed picture…");
  else drawBanner(null);
}

function paint(frame) {
  if (!palette) return;
  const context = ui.canvas.getContext("2d", {alpha:false});
  const image = new ImageData(frameToRgba(frame.pixels, Uint8Array.from(palette)), FRAME_WIDTH, FRAME_HEIGHT);
  const surface = document.createElement("canvas");
  surface.width = FRAME_WIDTH;
  surface.height = FRAME_HEIGHT;
  surface.getContext("2d").putImageData(image, 0, 0);
  const ratio = Math.min(globalThis.devicePixelRatio || 1, 2);
  const bounds = ui.canvas.getBoundingClientRect();
  const width = Math.max(1, Math.floor(bounds.width * ratio));
  const height = Math.max(1, Math.floor(bounds.height * ratio));
  if (ui.canvas.width !== width || ui.canvas.height !== height) {
    ui.canvas.width = width;
    ui.canvas.height = height;
  }
  context.imageSmoothingEnabled = false;
  context.fillStyle = "#000";
  context.fillRect(0, 0, width, height);
  const scale = Math.min(width / FRAME_WIDTH, height / FRAME_HEIGHT);
  const drawWidth = Math.floor(FRAME_WIDTH * scale);
  const drawHeight = Math.floor(FRAME_HEIGHT * scale);
  context.drawImage(surface, Math.floor((width-drawWidth)/2), Math.floor((height-drawHeight)/2), drawWidth, drawHeight);
  painted += 1;
  drawBanner(publicState?.death_message ?? publicState?.pause_message ?? null, publicState?.death_message ? "death" : "");
}

async function handleFrame(record) {
  try {
    const frame = await decodePushedFrame(record);
    if (!frame) throw new Error("the relayer pushed an incomplete frame");
    paint(frame);
    if (publicState && !publicState.pause_message && !publicState.death_message) drawBanner(null);
  } catch (error) {
    showError(`Frame rejected: ${error.message}`);
  }
}

function connectStream() {
  eventSource?.close();
  const last = Number(sessionStorage.getItem("doom-public.frame") ?? "");
  const since = Number.isFinite(last) ? `&since=${last}` : "";
  eventSource = new EventSource(`${relayerBase}/v1/public/stream?token=${encodeURIComponent(token)}${since}`);
  eventSource.addEventListener("state", (event) => {
    const value = JSON.parse(event.data);
    renderState(value);
  });
  eventSource.addEventListener("frame", (event) => {
    const record = JSON.parse(event.data);
    if (record.frame_index != null) sessionStorage.setItem("doom-public.frame", String(record.frame_index));
    void handleFrame(record);
  });
  eventSource.onerror = () => {
    ui.connection.textContent = "RECONNECTING";
    ui.connection.parentElement.classList.remove("online");
  };
}

async function join() {
  if (joining || session) return;
  joining = true;
  ui.join.disabled = true;
  clearError();
  try {
    if (!wallet) wallet = (await loadOrCreateBurner()).wallet;
    const joined = await api("/v1/public/join", {method:"POST", body:{publicKey:wallet.publicKeyHex}});
    token = joined.token;
    session = joined.session;
    if (!session?.binding || !Number.isInteger(session.slot)) throw new Error("relayer returned no shared game binding");
    session.bindingBytes = hexBytes(session.binding);
    sessionStorage.setItem("doom-public.token", token);
    palette = (await api("/v1/public/palette")).palette;
    connectStream();
    renderState(joined.state);
    clearError();
    if (heartbeatTimer) clearInterval(heartbeatTimer);
    heartbeatTimer = setInterval(() => {
      if (token) void api("/v1/public/heartbeat", {method:"POST", body:{token}}).catch((error) => showError(error.message));
    }, 10_000);
  } catch (error) {
    showError(error.message);
    ui.connection.textContent = "OFFLINE";
    ui.connection.parentElement.classList.remove("online");
  } finally {
    joining = false;
    ui.join.disabled = Boolean(session);
  }
}

async function sendInput() {
  if (!session || !token || !wallet || !publicState?.is_player || inputBusy) return;
  inputBusy = true;
  inputPending = false;
  clientSeq += 1;
  const message = encodeInput({
    slot: session.slot,
    keys: {...held},
    clientSeq,
    issuedAt: Math.floor(Date.now() / 1000),
    binding: session.bindingBytes,
  });
  try {
    const signature = await wallet.sign(message);
    await api("/v1/public/input", {method:"POST", body:{
      token,
      publicKey:wallet.publicKeyHex,
      message:bytesToBase64(message),
      signature:bytesToBase64(signature),
    }});
    lastInputAt = performance.now();
    clearError();
  } catch (error) {
    if (error.message.includes("rate limit")) inputPending = true;
    showError(error.message);
  } finally {
    inputBusy = false;
    if (inputPending || Object.values(held).some(Boolean)) scheduleInput();
  }
}

function scheduleInput() {
  if (!publicState?.is_player) return;
  inputPending = true;
  if (inputTimer !== null) return;
  const delay = Math.max(0, 100 - (performance.now() - lastInputAt));
  inputTimer = setTimeout(() => {
    inputTimer = null;
    inputInFlight = inputInFlight.then(sendInput, sendInput);
  }, delay);
}

function setKey(name, value) {
  if (!Object.hasOwn(held, name) || held[name] === value) return;
  held[name] = value;
  scheduleInput();
}

window.addEventListener("keydown", (event) => {
  const name = keyMap[event.code];
  if (!name) return;
  event.preventDefault();
  if (!event.repeat) setKey(name, true);
});
window.addEventListener("keyup", (event) => {
  const name = keyMap[event.code];
  if (name) {
    event.preventDefault();
    setKey(name, false);
  }
});
window.addEventListener("blur", () => {
  for (const key of Object.keys(held)) setKey(key, false);
});

for (const button of document.querySelectorAll(".touch-controls button[data-key]")) {
  const name = button.dataset.key;
  button.addEventListener("pointerdown", (event) => {
    event.preventDefault();
    button.setPointerCapture(event.pointerId);
    pointerKeys.set(event.pointerId, name);
    setKey(name, true);
  });
  const release = (event) => {
    if (pointerKeys.get(event.pointerId) !== name) return;
    pointerKeys.delete(event.pointerId);
    setKey(name, [...pointerKeys.values()].includes(name));
  };
  button.addEventListener("pointerup", release);
  button.addEventListener("pointercancel", release);
  button.addEventListener("lostpointercapture", release);
}

ui.join.addEventListener("click", () => void join());
ui.take.addEventListener("click", async () => {
  try {
    await api("/v1/public/offer/accept", {method:"POST", body:{token}});
    clearError();
  } catch (error) { showError(error.message); }
});

window.addEventListener("pagehide", () => {
  eventSource?.close();
  if (heartbeatTimer) clearInterval(heartbeatTimer);
  if (token) void fetch(`${relayerBase}/v1/public/leave?token=${encodeURIComponent(token)}`, {method:"DELETE", keepalive:true});
});

void join();
