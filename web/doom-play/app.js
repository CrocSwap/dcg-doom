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
import {FrameStream, decodePushedFrame, frameAccounts, readChainDigest, DEFAULT_CROSS_CHECK_EVERY} from "./push.js";

// The open keypress holds the page will carry at once. A press lives until the
// frame carrying it is painted, so the list is bounded and overflow is counted.
const MAX_OPEN_HOLDS = 24;
// How often the viewer reads a frame. A frame is complete only between one
// frame's last strip landing and the next frame's first, so a torn read is the
// expected case at strip rate 64 and is retried at once. This is the direct-read
// path; a page with a relayer to push from does not use it (see `push.js`).
const POLL_MS = 250;

const THEME_KEY = "doom-play.theme";
const RELAYER_KEY = "doom-play.relayer";
const BUFFER_KEY = "doom-play.buffer";
const PALETTE_KEY = "doom-play.palette.v1";

const query = new URLSearchParams(location.search);
const networkName = query.get("net") ?? DEFAULT_NETWORK;
const net = requireNetwork(networkName, ["rpcHttp", "doom.program", "doom.context", "doom.strips", "explorer.address"]);

// `?rpc=` points every chain read at another node. Two reasons it exists: a
// viewer on a network where the public node is slow or unreachable can be aimed
// at a private one without a rebuild, and the offline page tests need a canned
// node. It replaces the endpoint and drops the fallback list, because a fallback
// is a second node the page would silently read from instead -- which is exactly
// what a test asserting "the page read *this* node" must not allow. Mainnet is
// still refused: `ChainReader` asks `network.js` and never sees this.
const rpcOverride = query.get("rpc");
if (rpcOverride !== null) {
  let parsed = null;
  try {
    parsed = new URL(rpcOverride);
  } catch {
    parsed = null;
  }
  if (!parsed || !["http:", "https:"].includes(parsed.protocol)) {
    throw new Error(`?rpc=${rpcOverride} is not an http(s) URL`);
  }
  net.rpcHttp = parsed.origin + parsed.pathname.replace(/\/+$/, "");
  net.rpcHttpFallback = [];
}

// Which frame source a joined page uses. `push` is the relayer's own complete
// frames; `read` is this page polling the chain, which is the path the R12
// numbers (16 frames painted in 74 s, 151 torn reads) were measured on and
// which is kept so the two can be compared from the same page. `?frame-source=`
// overrides it and the choice is remembered.
const FRAME_SOURCE_KEY = "doom-play.frame-source";
const frameSourceQuery = query.get("frame-source");
const frameSource = frameSourceQuery === "read" || frameSourceQuery === "push"
  ? frameSourceQuery
  : (localStorage.getItem(FRAME_SOURCE_KEY) ?? "push");
localStorage.setItem(FRAME_SOURCE_KEY, frameSource);
// How many pushed frames between chain cross-checks. Zero turns the check off,
// which is a measurement and not a feature: the check is the only thing that
// would catch a relayer that stopped passing the chain's bytes.
const crossCheckEvery = Number(query.get("cross-check-every") ?? DEFAULT_CROSS_CHECK_EVERY) || 0;

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
  statNextPicture: el("stat-next-picture"),
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
  framesSource: el("frames-source"),
  framesPushed: el("frames-pushed"),
  framesTorn: el("frames-torn"),
  framesCrosschecks: el("frames-crosschecks"),
  framesMismatches: el("frames-mismatches"),
  framesHint: el("frames-hint"),
  inputlogList: el("inputlog-list"),
  inputlogSummary: el("inputlog-summary"),
  inputlogBanner: el("inputlog-banner"),
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
  // The chain cursor is shown beside frames; the relayer owns input assignment.
  cursor: 0,
  // Whether keystrokes can still reach the engine, and the relayer's per-deposit
  // record of what happened to the ones already sent.
  pipeline: null,
  inputFate: null,
  keyStateEvents: [],
  keyTimingBreakdown: [],
  ticsPerSecond: null,
  // The relayer's pointer to the render buffer holding the newest COMPLETE
  // frame, and that buffer's accounts. The page reads this buffer, not the one
  // being written.
  framePointer: null,
  frameCounters: null,
  // A key going down, and the two things that can be measured about how long it
  // takes to show. They are different quantities, and conflating them is how a
  // The latency meter must wait for the picture containing the stamped tic.
  //
  //   stampLatencies       keydown -> the first painted frame whose cursor is
  //                        past the tic this press's input was stamped for.
  //                        Cursor and input stamps share a sequence even when
  //                        RESET restores a game state with a nonzero gametic.
  //                        This is the latency a player feels.
  //   nextPictureLatencies keydown -> the first painted frame past the cursor
  //                        that was on screen when the key went down. "Time to
  //                        the next picture": it says the page is drivable, and
  //                        it says nothing about whether the key is in the
  //                        picture.
  //
  // Both are kept, both are on the panel, and the second is never reported under
  // the first's name.
  //
  // One open hold per *press*, not per hold of the keyboard: a player taps a key
  // for 300 ms and the picture carrying it arrives four seconds later, so the
  // clock cannot stop at keyup. Keying the hold to the keyboard hold instead is
  // what made the first version of this meter report zero samples on a live
  // session -- every press was closed before its frame arrived.
  //
  // `stampSamples` is the per-press log: which key, how long, and the cursor
  // pair that makes the sample checkable (the stamped tic and the cursor of the
  // frame that ended the wait). The frame's gametic is retained as context.
  holds: [],
  holdsStarted: 0,
  holdsUnstamped: 0,
  holdsDropped: 0,
  stampLatencies: [],
  nextPictureLatencies: [],
  stampSamples: [],
  // Recent paints by cursor, so a stamp that arrives after its frame was
  // painted is timed against that paint rather than the answer. `at` also
  // excludes frames painted before keydown.
  paintTics: [],
  clientSeq: 0,
  inputsSent: 0,
  batchesSigned: 0,
  refusals: 0,
  // Browser-to-relayer POST round trip, measured separately from key latency.
  roundTripMs: null,
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
  // The push stream, and what it has done. `stream` is null until a session is
  // joined and the page is on the push path; `pushSummary` is what the panel
  // shows and what the receipt reads.
  stream: null,
  frameSource,
  crossCheckEvery,
  pushSummary: null,
  lastCrossCheck: null,
  lastError: null,
  // A torn read is the expected case at strip rate 64, not a fault: the strips
  // of a frame land in eight transactions and a read catches them mid-write.
  // It was raised as an error and shown in the error bar, which is how a page
  // that was working fine came to look broken. Counted, not shown.
  tornReads: 0,
};

// One row per key press, with the fate of its relayer-assigned tic.
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
  inputLog.render(
    ui.inputlogList, ui.inputlogSummary, null, ui.inputlogTorn, state.tornReads,
    ui.inputlogBanner,
  );
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
  // The (paint time, cursor, gametic) ring, so a stamp that arrives after its
  // frame was painted can still be timed against the paint rather than against
  // the answer. Cursor shares the input sequence; gametic can start elsewhere
  // after RESET.
  state.paintTics.push({at: state.paintedAt, cursor: frame.cursor, gametic: frame.gametic});
  if (state.paintTics.length > 4096) state.paintTics.shift();
  measureKeypress(frame.cursor, frame.gametic, state.paintedAt);
  updateStatus();
}

// The two keypress meters, and the one place either of them is recorded.
//
// `nextPicture`: the first frame past the cursor that was on screen when the
// key went down. Recorded once per press, whatever else happens.
//
// `stamp`: the first frame past the cursor tic the press's input was stamped for.
// `stampTic` is null until the relayer answers, so this does nothing until the
// page knows the tic, and then it fires on the very next qualifying paint -- or,
// if that paint has already happened, on the paint ring's earliest entry at or
// past the stamp. Either way the sample is the time from keydown to a frame that
// contains the keypress.
//
// Every open press is offered the frame, not just the newest one: two presses a
// few hundred milliseconds apart have two stamped tics, and each closes on its
// own frame.
function measureKeypress(cursor, gametic, at) {
  if (!state.holds.length || typeof cursor !== "number") return;
  const done = [];
  for (const hold of state.holds) {
    if (at < hold.downAt) continue;
    if (hold.nextPictureMs === null && cursor > hold.cursorAtKeyDown) {
      hold.nextPictureMs = at - hold.downAt;
      state.nextPictureLatencies.push(hold.nextPictureMs);
      if (state.nextPictureLatencies.length > 40) state.nextPictureLatencies.shift();
    }
    // A frame at exactly the input sequence predates that tic's execution.
    if (hold.stampTic === null || cursor <= hold.stampTic) continue;
    hold.stampMs = at - hold.downAt;
    // The stamped tic and the cursor of the frame that ended the wait, together.
    // They are only comparable as a pair: a held key keeps offering batches and
    // each one is stamped at a later tic, so the page's *latest* stamped tic
    // belongs to a later press than the sample this one produced.
    const sample = {
      key: hold.key,
      tic: hold.stampTic,
      cursor,
      gametic,
      ms: hold.stampMs,
      next_picture_ms: hold.nextPictureMs,
    };
    state.stampSamples.push(sample);
    if (state.stampSamples.length > 60) state.stampSamples.shift();
    refreshKeyTimingBreakdown();
    state.lastStampSample = sample;
    state.stampLatencies.push(hold.stampMs);
    if (state.stampLatencies.length > 40) state.stampLatencies.shift();
    done.push(hold);
  }
  state.holds = state.holds.filter((hold) => !done.includes(hold));
}

function refreshKeyTimingBreakdown() {
  const rows = [];
  for (const sample of state.stampSamples) {
    const event = state.keyStateEvents.find((row) => row.seq === sample.tic);
    const post = state.keyPostSamples?.find((row) => row.tic === sample.tic);
    if (!event || !post) continue;
    const hasClockBridge = Number.isFinite(post.post_received_at_ms)
      && Number.isFinite(post.sent_wall_ms)
      && Number.isFinite(post.post_rtt_ms)
      && Number.isFinite(post.page_down_wall_ms);
    // The browser and Tokyo do not share a wall clock. Estimate their offset
    // from this request's midpoint (symmetric network delay), then use the
    // relayer's receipt time for keydown -> POST. The remaining server-side
    // intervals below use the relayer's clock on both ends.
    const clockOffsetMs = hasClockBridge
      ? Number(post.post_received_at_ms) - Number(post.sent_wall_ms) - Number(post.post_rtt_ms) / 2
      : null;
    const pageToPost = hasClockBridge
      ? Number(post.post_received_at_ms) - Number(post.page_down_wall_ms) - clockOffsetMs
      : null;
    const waitToDeposit = Number.isFinite(event.deposit_submitted_at_ms) && Number.isFinite(post.post_received_at_ms)
      ? Number(event.deposit_submitted_at_ms) - Number(post.post_received_at_ms)
      : null;
    const cursorToTic = Number.isFinite(event.cursor_reached_at_ms) && Number.isFinite(event.deposit_submitted_at_ms)
      ? Number(event.cursor_reached_at_ms) - Number(event.deposit_submitted_at_ms)
      : null;
    const known = [pageToPost, waitToDeposit, cursorToTic]
      .filter((value) => Number.isFinite(value));
    rows.push({
      tic: sample.tic,
      key: sample.key,
      keydown_to_picture_ms: sample.ms,
      page_to_post_ms_estimated: pageToPost,
      browser_to_relayer_clock_offset_ms_estimated: clockOffsetMs,
      wait_to_deposit_ms_measured: waitToDeposit,
      cursor_to_tic_ms_measured: cursorToTic,
      render_and_paint_ms_residual: known.length === 3
        ? Math.max(0, sample.ms - known.reduce((sum, value) => sum + value, 0))
        : null,
      deposit_signature: event.deposit_signature ?? null,
    });
  }
  state.keyTimingBreakdown = rows;
}

// The relayer has said which tic it stamped a batch for. The batch was signed
// while some press was open, and the press it belongs to is the newest one that
// does not have a tic yet: presses are seconds apart in anything a person plays,
// and a held key offers a batch every 100 ms, so the first answer after a
// keydown is that keydown's.
function noteStampedTic(tic, targetHold = null) {
  if (typeof tic !== "number" || !Number.isFinite(tic)) return;
  pruneHolds();
  const hold = targetHold ?? [...state.holds].reverse().find((row) => row.stampTic === null);
  if (hold === undefined) return;
  hold.stampTic = tic;
  // If the frame carrying that tic is already on screen the paint ring knows
  // when it was painted; otherwise the next qualifying paint records it. A
  // frame painted before keydown cannot count as the press's picture.
  const first = state.paintTics.find((row) => row.at >= hold.downAt && row.cursor > tic);
  if (first) measureKeypress(first.cursor, first.gametic, first.at);
}

// A press that will never get a stamp: every key of it is up, nothing has
// answered for it, and it has been open longer than any picture could take. The
// sample is left out rather than guessed, and counted, so a session whose POSTs
// all failed reads "0 of 8 presses stamped" rather than "0 ms".
const HOLD_STALE_MS = 30_000;
function pruneHolds() {
  if (!state.holds.length) return;
  const now = performance.now();
  const kept = [];
  for (const hold of state.holds) {
    const keysUp = !hold.keys.some((name) => state.keys[name]);
    if (keysUp && hold.stampTic === null && now - hold.downAt > HOLD_STALE_MS) {
      state.holdsUnstamped += 1;
      continue;
    }
    // The ring is a bound on the page's own memory, not on the measurement: a
    // press older than the ring is one the session will not wait for.
    if (state.holds.length - kept.length >= MAX_OPEN_HOLDS) {
      state.holdsDropped += 1;
      state.holdsUnstamped += 1;
      continue;
    }
    kept.push(hold);
  }
  state.holds = kept;
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

// The frame-path readout. Which path this page is on, and what it has done:
// pushed frames, how many failed to decode, how many have been compared against
// the chain, and how many of those comparisons disagreed. The last of those is
// the number a reader should watch, because it is the only one that can turn the
// picture into a lie.
function updateFrameReadout() {
  const summary = state.stream ? state.stream.summary() : state.pushSummary;
  const onPush = state.frameSource === "push" && Boolean(state.stream);
  setCell(ui.framesSource, onPush ? "PUSHED" : state.frameSource === "read" ? "READ FROM CHAIN" : "—");
  setCell(ui.framesPushed, summary ? String(summary.pushed) : "0");
  setCell(ui.framesTorn, summary ? String(summary.torn) : "0");
  const checks = summary ? summary.cross_checks : 0;
  setCell(ui.framesCrosschecks, String(checks));
  const mismatches = summary ? summary.cross_check_mismatches : 0;
  setCell(ui.framesMismatches, String(mismatches), mismatches > 0 ? "bad" : "good");
  const check = state.lastCrossCheck;
  if (ui.framesHint) {
    if (!onPush) {
      ui.framesHint.textContent =
        "This page reads the chain itself. That is the path the R12 numbers were measured on: " +
        "16 frames painted in 74 s with 151 torn reads, because a read can catch a frame mid-write.";
    } else if (!check) {
      ui.framesHint.textContent =
        `A pushed frame is the relayer's own read of the chain, painted without this page asking. ` +
        `One in ${state.crossCheckEvery || DEFAULT_CROSS_CHECK_EVERY} is read again from the chain and compared.`;
    } else {
      const stale = summary ? (summary.painted_stale ?? 0) : 0;
      const skipped = summary ? (summary.skipped ?? 0) : 0;
      const tail = stale || skipped
        ? ` ${stale} painted a frame older than the one on screen, ${skipped} were frames this page had already painted.`
        : "";
      ui.framesHint.textContent = `${check.comparable && !check.match ? "MISMATCH" : "chain check"} ` +
        `${check.checked}: ${check.reason}.${tail}`;
    }
  }
}

function updateStatus() {
  const frame = state.lastFrame;
  if (!frame) {
    updateFrameReadout();
    return;
  }
  setCell(ui.statTic, String(frame.gametic));
  const rate = fps();
  setCell(ui.statFps, rate === null ? "—" : rate.toFixed(2));
  // Two cells, two quantities, and neither is ever the other's number.
  //
  //   KEY->STAMPED TIC  keydown to the first painted frame at or past the tic
  //                     this press's input was stamped for. The latency a player
  //                     feels.
  //   NEXT PIC          keydown to the next painted frame of any kind. The
  //                     page's drivability, and much the smaller number: on R15
  //                     it read 408 ms while the key's actual stamped tic landed
  //                     to wait out was 2,155 ms.
  //
  // The stamp cell counts its own coverage, because a session where no POST
  // answered would otherwise read "0 ms" instead of "nothing was measured".
  const stamp = median(state.stampLatencies);
  const stamped = `${state.stampLatencies.length}/${state.holdsStarted}`;
  setCell(
    ui.statPicture,
    stamp === null ? "—" : `${stamp.toFixed(0)} ms · ${stamped}`,
    state.holdsUnstamped > 0 ? "bad" : "",
  );
  if (ui.statPicture) {
    ui.statPicture.title =
      `key press to the first painted frame at or past the stamped tic, median of ${state.stampLatencies.length}; ` +
      `${state.holdsStarted} press(es) started, ${state.holdsUnstamped} never got a stamped answer, ` +
      `${state.holds.length} still open, ${state.holdsDropped} dropped at the bound` +
      (state.lastStampSample
        ? `; last: ${state.lastStampSample.key} stamped at tic ${state.lastStampSample.tic}, ` +
          `painted at cursor ${state.lastStampSample.cursor}`
        : "");
  }
  const next = median(state.nextPictureLatencies);
  setCell(ui.statNextPicture, next === null ? "—" : `${next.toFixed(0)} ms`);
  if (ui.statNextPicture) {
    ui.statNextPicture.title = "key press to the next painted frame, whatever it shows. Not a keypress latency.";
  }
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
  updateFrameReadout();

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

// ---------------------------------------------------------------- the push
//
// A joined page takes its frames from the relayer when `frame-source` is `push`
// (the default) and reads the chain itself when it is `read`. Both paint through
// the same `paintFrame`, so the two paths are comparable on one page: the
// difference in painted rate and torn reads is the difference between the paths
// and not between two implementations of the painter.

// The cross-check's chain read: the eight strip headers and the snapshot header,
// 1,728 bytes, and the digests out of them. The pixel-for-pixel version of this
// read 1,072,384 B and lost the race to every frame -- measured, ten checks and
// ten "the chain moved on" on the R13 context at 2.02 fps. The digests are the
// stronger comparison anyway: a strip's digest covers its pixels, and the pushed
// frame's eight were verified before it was painted.
const readPushedFromChain = (record) => readChainDigest(state.reader, record);

function startPush() {
  stopFrames();
  if (!state.session || !state.relayerBase) return;
  const stream = new FrameStream({
    base: state.relayerBase,
    sessionId: state.session.id,
    crossCheckEvery: state.crossCheckEvery,
    readChain: readPushedFromChain,
    onFrame: async (frame, record) => {
      // The relayer names the buffer it read, so the strip chips and the status
      // line describe the frame on screen rather than the configured buffer.
      if (record.buffer !== null && record.buffer !== undefined) {
        state.lastFrameBuffer = record.buffer;
        const accounts = frameAccounts(record);
        if (accounts) {
          net.doom.strips[record.buffer] = accounts.strips;
          if (accounts.snapshot) net.doom.snapshots[record.buffer] = accounts.snapshot;
        }
      }
      ui.stripSummary.textContent = "8 / 8";
      for (const chip of stripChips) chip.classList.add("ok");
      frame.digest = frame.sha256 ?? null;
      await paintFrame(frame);
      state.pushSummary = stream.summary();
      updateStatus();
    },
    onCrossCheck: ({comparison, checked}) => {
      state.lastCrossCheck = {...comparison, checked};
      // A mismatch is raised: it is the one condition on this path that means
      // the picture on screen cannot be trusted, and a page whose job includes
      // saying whether a frame is honest cannot swallow it into a count.
      if (comparison.comparable && !comparison.match) {
        showError(new Error(`frame push disagrees with the chain: ${comparison.reason}`), "push");
      } else {
        clearError("push");
      }
      updateStatus();
    },
  });
  try {
    stream.start();
  } catch (error) {
    showError(error, "push");
    // A browser with no EventSource, or a relayer that has no stream route, is
    // not a reason to show nothing: the direct read is right there and it is
    // what this page did before.
  // With no session there is nobody to push from, so a spectator page reads the
  // chain itself. A joined page switches to the push path in `startFrames`.
  startPolling();

    return;
  }
  state.stream = stream;
  setCell(ui.stageState, "PUSHED FRAMES");
}

// Stop whichever frame source is running. Both are stopped on every session
// change, because a stream for a session that has ended is a page painting
// somebody else's game.
function stopFrames() {
  state.stream?.stop();
  state.stream = null;
  state.stopPolling?.();
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
  return pressKey(name, KEY_LABEL[code] ?? name.toUpperCase()) ?? true;
}

function keyUp(code) {
  const name = KEY_MAP[code];
  if (!name) return false;
  return releaseKey(name);
}

// Shared by the keyboard and the on-screen pad, because a press measured from
// the pad and a press measured from the keyboard are the same event and the old
// split -- the pad set `state.keys` itself and never opened a hold -- is how a
// page driven entirely by mouse could report no keypress latency at all.
function pressKey(name, label) {
  const first = !state.keys[name];
  state.keys[name] = true;
  if (first) pressRows.set(name, inputLog.press(name, label));
  // One open hold per *press*. A key repeat (event.repeat, or any second keydown
  // for a key already down) is not a new press and must not restart the clock; a
  // key that comes up and goes down again is. The clock does not stop at keyup:
  // a tap is 300 ms and the picture carrying it arrives seconds later, so the
  // hold lives until its frame does.
  if (!first) return null;
  pruneHolds();
  const hold = {
    key: name,
    downAt: performance.now(),
    downWallMs: Date.now(),
    cursorAtKeyDown: state.lastFrame ? state.lastFrame.cursor : -1,
    stampTic: null,
    nextPictureMs: null,
    stampMs: null,
    keys: [name],
    inputRecord: pressRows.get(name),
  };
  state.holds.push(hold);
  state.holdsStarted += 1;
  return hold;
}

function releaseKey(name) {
  if (!state.keys[name]) return false;
  state.keys[name] = false;
  inputLog.release(pressRows.get(name));
  pressRows.delete(name);
  // Deliberately does not close the hold. The frame carrying this press has not
  // been painted yet, and closing here is what made a live session report zero
  // keypress samples: every press was closed before its picture arrived.
  pruneHolds();
  return true;
}

addEventListener("keydown", (event) => {
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
  if (event.repeat) return;
  const changed = keyDown(event.code);
  if (changed) {
    event.preventDefault();
    scheduleInputPush({holds: changed === true ? [] : [changed]});
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
    if (state.keys[name]) releaseKey(name);
  }
  syncPad();
});

for (const button of document.querySelectorAll(".pad button")) {
  const name = button.dataset.key;
  const press = (event) => {
    event.preventDefault();
    const hold = pressKey(name, name.toUpperCase());
    syncPad();
    if (hold) scheduleInputPush({holds: [hold]});
  };
  const release = (event) => {
    event.preventDefault();
    const changed = releaseKey(name);
    syncPad();
    if (changed) scheduleInputPush();
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

// The page reports the current key state. It does not calculate a tic, producer
// lead, or delivery window; the relayer assigns the first still-depositable tic.
// V2 sends key-state transitions only. The sender reserves their sequence under
// the same lock it uses to build a DEPOSIT+ADVANCE transaction, so the page
// neither predicts a cursor nor pays a second delivery lead. Transitions are
// kept in order: a quick down/up pair occupies two successive tics instead of
// being coalesced into a key-up that loses the tap.
const liveInputQueue = [];
let liveInputBusy = false;
let liveInputFrame = null;
let liveInputRetryAt = 0;
let liveInputRetryTimer = null;
let lastQueuedKeyState = null;

function sameKeyState(left, right) {
  return Object.keys(right).every((key) => Boolean(left?.[key]) === Boolean(right[key]));
}

function scheduleInputPush({holds = []} = {}) {
  if (!state.session || !state.wallet) return;
  const keys = {...state.keys};
  const previous = liveInputQueue.at(-1)?.keys ?? lastQueuedKeyState;
  if (previous && sameKeyState(previous, keys) && holds.length === 0) return;
  liveInputQueue.push({keys, holds: [...holds], queuedAt: performance.now(), attempts: 0});
  lastQueuedKeyState = keys;
  if (liveInputFrame !== null) return;
  liveInputFrame = requestAnimationFrame(() => {
    liveInputFrame = null;
    void pushQueuedInput();
  });
}

async function pushQueuedInput() {
  if (liveInputBusy || !state.session || !state.wallet || !liveInputQueue.length) return;
  if (performance.now() < liveInputRetryAt) {
    if (liveInputRetryTimer === null) {
      liveInputRetryTimer = setTimeout(() => {
        liveInputRetryTimer = null;
        void pushQueuedInput();
      }, liveInputRetryAt - performance.now());
    }
    return;
  }
  liveInputBusy = true;
  const flight = liveInputQueue.shift();
  try {
    state.clientSeq += 1;
    const message = encodeInput({
      slot: state.session.slot,
      keys: flight.keys,
      clientSeq: state.clientSeq,
      issuedAt: Math.floor(Date.now() / 1000),
      binding: state.session.binding,
    });
    const signature = await state.wallet.sign(message);
    const sentAt = performance.now();
    const sentWallMs = Date.now();
    state.batchesSigned += 1;
    const response = await fetch(`${state.relayerBase}/v1/sessions/${encodeURIComponent(state.session.id)}/input`, {
      method: "POST",
      headers: {"content-type": "application/json"},
      body: JSON.stringify({message: toBase64(message), signature: toBase64(signature)}),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      state.refusals += 1;
      throw new Error(payload.error ?? `relayer returned ${response.status}`);
    }
    if (payload.key_state && Number.isFinite(payload.key_state.seq)) {
      const existing = state.keyStateEvents.findIndex((row) => row.seq === payload.key_state.seq);
      if (existing < 0) state.keyStateEvents.push(payload.key_state);
      else state.keyStateEvents[existing] = {...state.keyStateEvents[existing], ...payload.key_state};
    }
    const responseAt = performance.now();
    const postRttMs = responseAt - sentAt;
    state.roundTripMs = postRttMs;
    state.latencies.push(postRttMs);
    if (state.latencies.length > 40) state.latencies.shift();
    state.inputsSent += 1;
    const stampTic = payload.stamp_tic ?? payload.accepted_from;
    if (typeof stampTic === "number") {
      state.lastStampedTic = stampTic;
      const oneTic = heldToCommands(flight.keys, stampTic, 1);
      for (const hold of flight.holds) {
        inputLog.noteStampedState(hold.inputRecord, stampTic, oneTic);
        noteStampedTic(stampTic, hold);
      }
    }
    state.keyPostSamples ??= [];
    state.keyPostSamples.push({
      tic: stampTic ?? null,
      page_to_response_ms: responseAt - flight.queuedAt,
      post_rtt_ms: postRttMs,
      send_queue_ms: sentAt - flight.queuedAt,
      post_received_at_ms: payload.post_received_at_ms ?? null,
      tic_assigned_at_ms: payload.tic_assigned_at_ms ?? null,
      sent_wall_ms: sentWallMs,
      page_down_wall_ms: flight.holds[0]?.downWallMs ?? sentWallMs,
    });
    if (state.keyPostSamples.length > 80) state.keyPostSamples.shift();
    inputLog.observe({cursor: payload.cursor, input_fate: payload.input_fate});
    refreshKeyTimingBreakdown();
    renderInputLog();
    if (typeof payload.cursor === "number") state.cursor = payload.cursor;
    if (payload.pipeline) state.pipeline = payload.pipeline;
    const median = [...state.latencies].sort((a, b) => a - b)[Math.floor(state.latencies.length / 2)];
    setCell(ui.inputLatency, `${median.toFixed(0)} ms POST round trip · tic assigned by relayer`);
    setCell(ui.inputsSent, String(state.inputsSent));
    clearError("action");
  } catch (error) {
    flight.attempts += 1;
    liveInputQueue.unshift(flight);
    showError(error, "action");
    const delay = Math.min(2000, 100 * (2 ** Math.min(flight.attempts - 1, 4)));
    liveInputRetryAt = performance.now() + delay;
  } finally {
    liveInputBusy = false;
    if (liveInputQueue.length && liveInputFrame === null && performance.now() >= liveInputRetryAt) {
      liveInputFrame = requestAnimationFrame(() => {
        liveInputFrame = null;
        void pushQueuedInput();
      });
    }
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
    // The 32 bytes `fromHex` above already decoded, not `session.binding` again.
    // The relayer sends the binding as 64 characters of hex, and `toHex` takes
    // bytes, so handing it the string threw a RangeError on the way past -- which
    // is the line between "the page has a session" and "the page has a session
    // it polled, that starts its input loop and can find again after a reload".
    // Measured here on 2026-09-26: a JOIN that painted frames and staged nothing.
    binding: toHex(binding),
  });
  await pollSessionStatus();
  liveInputQueue.length = 0;
  lastQueuedKeyState = null;
  if (Object.values(state.keys).some(Boolean)) {
    scheduleInputPush({holds: state.holds.filter((hold) => hold.stampTic === null)});
  }
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
  // The session is what the push stream is scoped to, so the frame source is
  // chosen here rather than at load: a spectator page has no session and no
  // stream, and a joined page has both.
  startFrames();
}

// One place that decides where frames come from, so a session change cannot
// leave both sources running.
function startFrames() {
  stopFrames();
  if (state.frameSource === "push" && state.session) {
    startPush();
    return;
  }
  startPolling();
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
  if (payload.pipeline !== undefined && payload.pipeline !== null) state.pipeline = payload.pipeline;
  if (payload.input_fate !== undefined && payload.input_fate !== null) {
    state.inputFate = payload.input_fate;
    if (Array.isArray(payload.input_fate.key_states)) state.keyStateEvents = payload.input_fate.key_states;
    refreshKeyTimingBreakdown();
  }
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

async function pollSessionStatus() {
  if (!state.session) return;
  try {
    const status = await relayer(`/v1/sessions/${encodeURIComponent(state.session.id)}`);
    adoptAccounts(status);
    setCell(ui.slotCell, String(status.slot));
    // Before the first input, show the engine rate. Input POST and key-to-picture
    // latency are separate measurements once the first transition is assigned.
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
    refreshKeyTimingBreakdown();
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
  // The stream is scoped to the session that just ended. Leaving it open would
  // paint a game this page no longer has a stake in.
  stopFrames();
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
