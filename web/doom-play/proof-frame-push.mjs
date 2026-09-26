// The frame-push proof: measure the painted rate through the page, then press
// keys and measure how long a keypress takes to show up.
//
// Same shape as proof-input-panel.mjs and for the same reason -- dependency-free
// CDP over a WebSocket this file implements itself, so the proof does not depend
// on a package that may or may not be installed, and it reads the page's own
// state rather than anything this file keeps.
//
// What it is here to answer, in order:
//
//   1. painted frames per second over a window of at least PROOF_FPS_WINDOW
//      seconds, counted from the page's own paint timestamps;
//   2. torn reads on the push path, which should be none: a pushed frame is
//      eight whole strip accounts that agree, not a read that caught a write;
//   3. key-to-picture latency over at least PROOF_PRESSES presses;
//   4. the cross-check: how many pushed frames were read back from the chain and
//      compared, and how many disagreed;
//   5. the panel's own landed ratio, read from the page.
//
// Usage: node proof-frame-push.mjs [out-dir]

import {spawn} from "node:child_process";
import {createHash} from "node:crypto";
import {mkdirSync, rmSync, writeFileSync} from "node:fs";
import {createConnection} from "node:net";
import {setTimeout as sleep} from "node:timers/promises";

const OUT = process.argv[2] ?? ".";
const PAGE = process.env.PROOF_PAGE
  ?? "http://127.0.0.1:8012/web/doom-play/?relayer=http://127.0.0.1:8787&net=testnet&frame-source=push";
const CHROME = process.env.PROOF_CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const PORT = Number(process.env.PROOF_CDP_PORT ?? 9334);
const FPS_WINDOW = Number(process.env.PROOF_FPS_WINDOW ?? 60) * 1000;
const PRESSES = Number(process.env.PROOF_PRESSES ?? 12);
const HOLD_MS = Number(process.env.PROOF_HOLD_MS ?? 1200);
const SETTLE_MS = Number(process.env.PROOF_SETTLE_MS ?? 2200);

mkdirSync(OUT, {recursive: true});

// -- a minimal websocket client, enough for CDP -----------------------------

function wsConnect(url) {
  const parsed = new URL(url);
  const key = createHash("sha1").update(createHash("md5").update(String(Math.random())).digest()).digest("base64");
  return new Promise((resolve, reject) => {
    const socket = createConnection({host: parsed.hostname, port: Number(parsed.port || 80)});
    let buffer = Buffer.alloc(0);
    let handshaked = false;
    const listeners = [];
    const send = (text) => {
      const payload = Buffer.from(text);
      const header = payload.length < 126
        ? Buffer.from([0x81, 0x80 | payload.length])
        : Buffer.concat([Buffer.from([0x81, 0x80 | 126]), (() => { const b = Buffer.alloc(2); b.writeUInt16BE(payload.length); return b; })()]);
      socket.write(Buffer.concat([header, Buffer.from([0, 0, 0, 0]), payload]));
    };
    socket.on("connect", () => {
      socket.write(
        `GET ${parsed.pathname}${parsed.search} HTTP/1.1\r\n` +
          `Host: ${parsed.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
          `Sec-WebSocket-Key: ${Buffer.from(key).toString("base64").slice(0, 24)}\r\n` +
          "Sec-WebSocket-Version: 13\r\n\r\n",
      );
    });
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (!handshaked) {
        const at = buffer.indexOf("\r\n\r\n");
        if (at < 0) return;
        handshaked = true;
        buffer = buffer.subarray(at + 4);
        resolve({send, onMessage: (fn) => listeners.push(fn), close: () => socket.end()});
      }
      for (;;) {
        if (buffer.length < 2) return;
        const opcode = buffer[0] & 0x0f;
        let length = buffer[1] & 0x7f;
        let offset = 2;
        if (length === 126) {
          if (buffer.length < 4) return;
          length = buffer.readUInt16BE(2);
          offset = 4;
        } else if (length === 127) {
          if (buffer.length < 10) return;
          length = Number(buffer.readBigUInt64BE(2));
          offset = 10;
        }
        if (buffer.length < offset + length) return;
        const payload = buffer.subarray(offset, offset + length).toString("utf8");
        buffer = buffer.subarray(offset + length);
        if (opcode === 1) for (const fn of listeners) fn(payload);
        if (opcode === 8) socket.end();
      }
    });
    socket.on("error", reject);
  });
}

// -- the browser ------------------------------------------------------------

// The profile goes with the browser. On macOS it carries the `code_sign_clone`
// directory Chrome writes into it, which is megabytes per run, and a proof that
// leaves one behind on every session is a proof that fills the disk. `PROOF_CLEAN`
// =0 keeps it, for a run that wants to look at the profile.
const PROFILE = process.env.PROOF_PROFILE ?? "/tmp/doom-push-proof-profile";
const chrome = spawn(CHROME, [
  "--headless=new",
  `--remote-debugging-port=${PORT}`,
  "--remote-allow-origins=*",
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-gpu",
  "--window-size=1600,1200",
  `--user-data-dir=${PROFILE}`,
  "about:blank",
], {stdio: ["ignore", "ignore", "pipe"]});
chrome.stderr.on("data", () => {});

// One exit path for every way this file ends, including the join failure below.
let closed = false;
function closeChrome() {
  if (closed) return;
  closed = true;
  try {
    chrome.kill();
  } catch {
    /* already gone */
  }
  if (process.env.PROOF_CLEAN === "0") return;
  const sweep = () => {
    try {
      rmSync(PROFILE, {recursive: true, force: true});
    } catch {
      /* the profile is already gone */
    }
  };
  chrome.once("exit", sweep);
  const timer = setTimeout(() => {
    try {
      chrome.kill("SIGKILL");
    } catch {
      /* already gone */
    }
    sweep();
  }, 2_000);
  timer.unref?.();
}
process.on("exit", closeChrome);
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    closeChrome();
    process.exit(1);
  });
}

async function target() {
  for (let attempt = 0; attempt < 80; attempt += 1) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      const page = list.find((t) => t.type === "page");
      if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl;
    } catch { /* not up yet */ }
    await sleep(250);
  }
  throw new Error("chrome did not offer a debuggable page");
}

const ws = await wsConnect(await target());
let nextId = 0;
const waiting = new Map();
ws.onMessage((text) => {
  const message = JSON.parse(text);
  const resolve = waiting.get(message.id);
  if (resolve) {
    waiting.delete(message.id);
    resolve(message);
  }
});
function call(method, params = {}) {
  const id = ++nextId;
  return new Promise((resolve) => {
    waiting.set(id, resolve);
    ws.send(JSON.stringify({id, method, params}));
  });
}
async function evaluate(expression) {
  const answer = await call("Runtime.evaluate", {expression, awaitPromise: true, returnByValue: true});
  if (answer.result?.exceptionDetails) {
    throw new Error(answer.result.exceptionDetails.exception?.description ?? "evaluate failed");
  }
  return answer.result?.result?.value;
}
async function key(type, code, keyName, windowsVirtualKeyCode) {
  await call("Input.dispatchKeyEvent", {
    type, code, key: keyName,
    windowsVirtualKeyCode, nativeVirtualKeyCode: windowsVirtualKeyCode,
  });
}
async function shot(name) {
  const answer = await call("Page.captureScreenshot", {format: "png"});
  writeFileSync(`${OUT}/${name}.png`, Buffer.from(answer.result.data, "base64"));
}

// Read the page's own numbers. One expression, so a rate and the counts that
// explain it come from the same instant.
const SNAPSHOT = `(() => {
  const s = window.doomPlay?.state;
  if (!s) return null;
  const stream = s.stream;
  return {
    painted: s.paintedFrames,
    torn: s.tornReads,
    paints: s.paintTimes.slice(),
    paintTics: (s.paintTics ?? []).slice(),
    ticsPerSecond: s.ticsPerSecond ?? null,
    // The two keypress meters, kept apart. stampLatencies is keydown to the
    // first painted frame at or past the tic the input was stamped for;
    // nextPictureLatencies is keydown to the next painted frame of any kind,
    // which is a much smaller number and not a keypress latency.
    stampLatencies: s.stampLatencies.slice(),
    stampSamples: (s.stampSamples ?? []).slice(),
    keyTimingBreakdown: (s.keyTimingBreakdown ?? []).slice(),
    nextPictureLatencies: s.nextPictureLatencies.slice(),
    holdsStarted: s.holdsStarted,
    holdsUnstamped: s.holdsUnstamped,
    holdsDropped: s.holdsDropped ?? 0,
    holdsOpen: (s.holds ?? []).length,
    lastStampedTic: s.lastStampedTic ?? null,
    frameSource: s.frameSource,
    stream: stream ? stream.summary() : null,
    lastCrossCheck: s.lastCrossCheck,
    session: s.session?.id ?? null,
    slot: s.lastFrameSlot,
    buffer: s.lastFrameBuffer,
    lastGametic: s.lastFrame?.gametic ?? null,
    lastCursor: s.lastFrame?.cursor ?? null,
    lastSha: s.lastFrame?.sha256 ?? s.lastFrame?.digest ?? null,
    panel: window.doomPlay.inputLog.summary(),
    banner: document.getElementById("inputlog-banner")?.hidden
      ? null
      : document.getElementById("inputlog-banner")?.textContent?.replace(/\\s+/g, " ").trim() ?? null,
    error: document.getElementById("error")?.hidden
      ? null
      : document.getElementById("error")?.textContent?.replace(/\\s+/g, " ").trim() ?? null,
    framesReadout: {
      source: document.getElementById("frames-source")?.textContent ?? null,
      pushed: document.getElementById("frames-pushed")?.textContent ?? null,
      torn: document.getElementById("frames-torn")?.textContent ?? null,
      crossChecks: document.getElementById("frames-crosschecks")?.textContent ?? null,
      mismatches: document.getElementById("frames-mismatches")?.textContent ?? null,
    },
    fps: document.getElementById("stat-fps")?.textContent ?? null,
    picture: document.getElementById("stat-picture")?.textContent ?? null,
  };
})()`;

await call("Page.enable");
await call("Runtime.enable");
await call("Page.navigate", {url: PAGE});
await sleep(4000);

// JOIN, through the page's own button, so the session is one the relayer made.
const joined = await evaluate(`(async () => {
  const button = document.getElementById("join");
  if (!button) return "no join button";
  button.click();
  for (let i = 0; i < 80; i += 1) {
    await new Promise((r) => setTimeout(r, 500));
    const state = window.doomPlay?.state?.session;
    if (state) return state.id;
  }
  return "no session after 40 s";
})()`);
console.log("session:", joined);
if (typeof joined !== "string" || !/^[0-9a-f]{8,}$/.test(joined)) {
  writeFileSync(`${OUT}/proof.json`, JSON.stringify({page: PAGE, joined, ok: false}, null, 2));
  await shot("proof-join-failed");
  console.log("JOIN FAILED; the page says:", await evaluate(`document.getElementById("error")?.textContent ?? ""`));
  closeChrome();
  process.exit(1);
}

// Wait for the first pushed frame, so the window below measures a running rate
// rather than the time to the first frame.
const first = await evaluate(`(async () => {
  for (let i = 0; i < 80; i += 1) {
    if ((window.doomPlay?.state?.paintedFrames ?? 0) > 0) break;
    await new Promise((r) => setTimeout(r, 500));
  }
  return ${SNAPSHOT};
})()`);
console.log("first frame:", JSON.stringify({
  painted: first.painted, stream: first.stream, source: first.framesReadout, error: first.error,
}));

// 1. painted rate over the window. Counted from the page's own paint
//    timestamps, not from a stopwatch around this file.
const before = await evaluate(SNAPSHOT);
const windowStart = Date.now();
await sleep(FPS_WINDOW);
const after = await evaluate(SNAPSHOT);
const windowSeconds = (Date.now() - windowStart) / 1000;
// The paints inside the window, from the page's rolling list. The list is capped
// at four seconds, so the count comes from the difference of the totals and the
// span from the page's own first and last paint inside it.
const paintedInWindow = after.painted - before.painted;
const tornInWindow = after.torn - before.torn;
const pushedInWindow = (after.stream?.pushed ?? 0) - (before.stream?.pushed ?? 0);
const oldGametics = new Set(before.paintTics.map((row) => row.gametic).filter(Number.isFinite));
const newGametics = new Set(after.paintTics.map((row) => row.gametic).filter(Number.isFinite));
const newPicturesInWindow = [...newGametics].filter((gametic) => !oldGametics.has(gametic)).length;
console.log(`window ${windowSeconds.toFixed(1)} s: painted ${paintedInWindow} ` +
  `(${(paintedInWindow / windowSeconds).toFixed(2)} fps), new ${newPicturesInWindow} ` +
  `(${(newPicturesInWindow / windowSeconds).toFixed(2)} pictures/s), pushed ${pushedInWindow}, torn ${tornInWindow}`);
await shot("proof-fps");

// 2. keys. The presses cycle movement and fire states without triggering the
//    dead-player use/reset path. Two numbers come out of each press and they are different
//    quantities:
//
//      keypress  keydown to the first painted frame whose gametic is at or past
//                the tic the relayer stamped this hold's input for. The page's
//                `stampLatencies` entry for the hold, and the number the
//                playability bar is written against.
//      next pic  keydown to the next painted frame of any kind. The page's
//                `nextPictureLatencies` entry, and what the old proof reported
//                under the name "latency".
//
//    A press that produced no stamped answer contributes to neither, and the
//    counts say so rather than reading as a zero.
const KEYS = [
  {key: "forward", code: "ArrowUp", keyName: "ArrowUp", vk: 38},
  {key: "back", code: "ArrowDown", keyName: "ArrowDown", vk: 40},
  {key: "left", code: "ArrowLeft", keyName: "ArrowLeft", vk: 37},
  {key: "right", code: "ArrowRight", keyName: "ArrowRight", vk: 39},
  {key: "fire", code: "Space", keyName: " ", vk: 32},
];
const presses = [];
for (let index = 0; index < PRESSES; index += 1) {
  const spec = KEYS[index % KEYS.length];
  const pictureBefore = await evaluate(`window.doomPlay.state.lastFrame?.gametic ?? null`);
  // The page's per-press log, by count. Attribution is the page's, because the
  // page knows which key each clock was opened for and when; slicing the two
  // latency rings after a fixed settle would attribute a slow press's sample to
  // whichever press happened to read the ring next.
  const mark = await evaluate(`window.doomPlay.state.stampSamples.length`);
  const at = Date.now();
  await key("keyDown", spec.code, spec.keyName, spec.vk);
  await sleep(HOLD_MS);
  await key("keyUp", spec.code, spec.keyName, spec.vk);
  await sleep(SETTLE_MS);
  const row = await evaluate(SNAPSHOT);
  const samples = row.stampSamples.slice(mark).filter((sample) => sample.key === spec.key);
  const sample = samples.length ? samples[samples.length - 1] : null;
  presses.push({
    key: spec.key,
    index,
    pictureBefore,
    pictureAfter: row.lastGametic,
    pictureMoved: pictureBefore !== row.lastGametic,
    // Keydown to a frame at or past the tic this press's input was stamped for,
    // with the pair that makes it checkable.
    keypressMs: sample ? sample.ms : null,
    stampedTic: sample ? sample.tic : null,
    paintedGametic: sample ? sample.gametic : null,
    // Keydown to the next frame of any kind. Not a keypress latency.
    nextPictureMs: sample ? sample.next_picture_ms : null,
    wallMs: Date.now() - at,
    painted: row.painted,
    torn: row.torn,
    panel: row.panel,
  });
  console.log(
    spec.key.padEnd(8),
    "picture", pictureBefore === row.lastGametic ? "same" : `moved ${pictureBefore} -> ${row.lastGametic}`,
    "keypress", presses.at(-1).keypressMs === null ? "-" : `${presses.at(-1).keypressMs.toFixed(0)} ms`,
    "next pic", presses.at(-1).nextPictureMs === null ? "-" : `${presses.at(-1).nextPictureMs.toFixed(0)} ms`,
    "painted", row.painted,
    "open", row.holdsOpen,
  );
}
await shot("proof-keys");

// The presses are done but their pictures are not: a press is 1.2 s of key and
// the frame carrying it arrives however long the lead is. Wait for the page's
// open holds to close, so the report counts every press rather than the ones
// that happened to land inside the last settle window.
const DRAIN_MS = Number(process.env.PROOF_DRAIN_MS ?? 30_000);
const drainStart = Date.now();
let drained = await evaluate(SNAPSHOT);
const completeTiming = (press, snapshot) => {
  if (!Number.isFinite(press.stampedTic)) return true;
  const row = snapshot.keyTimingBreakdown.find((item) => item.tic === press.stampedTic);
  return row && [
    row.page_to_post_ms_estimated,
    row.wait_to_deposit_ms_measured,
    row.cursor_to_tic_ms_measured,
    row.render_and_paint_ms_residual,
  ].every(Number.isFinite);
};
const pendingTiming = (snapshot) => presses.filter((press) => !completeTiming(press, snapshot)).length;
while ((drained.holdsOpen > 0 || pendingTiming(drained) > 0) && Date.now() - drainStart < DRAIN_MS) {
  await sleep(1000);
  drained = await evaluate(SNAPSHOT);
}
console.log(`drain: ${drained.holdsOpen} hold(s), ${pendingTiming(drained)} timing row(s) pending after ` +
  `${((Date.now() - drainStart) / 1000).toFixed(1)} s`);
// Every press's sample, from the page's own log rather than from the per-press
// rows, so a sample that landed during the drain is counted.
const allSamples = drained.stampSamples.filter((sample) => sample.key !== null);
for (const press of presses) {
  if (press.keypressMs !== null) continue;
  const late = allSamples.filter((sample) => sample.key === press.key);
  if (late.length) {
    const sample = late[late.length - 1];
    press.keypressMs = sample.ms;
    press.stampedTic = sample.tic;
    press.paintedGametic = sample.gametic;
    press.nextPictureMs = sample.next_picture_ms;
    press.late = true;
  }
}

const final = await evaluate(SNAPSHOT);
// The relayer's own view of the same session, so the page's numbers and the
// pipeline's numbers are side by side rather than one of them being believed.
let relayerHealth = null;
try {
  relayerHealth = await (await fetch(`http://127.0.0.1:8787/healthz`)).json();
} catch (error) {
  relayerHealth = {error: String(error)};
}
// The keypress meter and the next-picture meter, summarised separately and
// under their own names. `keypress_latency` is the number the bar is written
// against; `next_picture` is the old proof's "latency", kept because it is what
// the page's drivability looks like and it is much the smaller number.
const percentile = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.ceil((p / 100) * sorted.length) - 1;
  return sorted[Math.min(sorted.length - 1, Math.max(0, rank))];
};
const summarize = (values) => ({
  samples: values.length,
  min: values.length ? Math.min(...values) : null,
  median: percentile(values, 50),
  p90: percentile(values, 90),
  max: values.length ? Math.max(...values) : null,
});
const keypressValues = presses.map((press) => press.keypressMs).filter((value) => value !== null);
const nextPictureValues = presses.map((press) => press.nextPictureMs).filter((value) => value !== null);
const result = {
  page: PAGE,
  ok: tornInWindow === 0 && paintedInWindow > 0,
  joined,
  window: {
    seconds: windowSeconds,
    painted: paintedInWindow,
    painted_fps: paintedInWindow / windowSeconds,
    new_pictures: newPicturesInWindow,
    new_pictures_per_second: newPicturesInWindow / windowSeconds,
    pushed: pushedInWindow,
    torn: tornInWindow,
  },
  first,
  after_window: after,
  presses,
  // key press -> first painted frame at or past the stamped tic.
  keypress_latency: summarize(keypressValues),
  // key press -> the next painted frame, whatever it shows. Not a keypress
  // latency; kept under its own name so the two are never averaged.
  next_picture: summarize(nextPictureValues),
  // How much of the pressing was measurable at all. A press whose POST never
  // answered has no stamped tic and therefore no keypress sample.
  coverage: {
    presses: presses.length,
    holds_started: final.holdsStarted,
    holds_unstamped: final.holdsUnstamped,
    holds_open: final.holdsOpen,
    holds_dropped: final.holdsDropped,
    samples: final.stampSamples.length,
  },
  // Every sample, with the pair that makes each one checkable: the tic the
  // input was stamped for and the gametic of the frame that ended the wait.
  samples: final.stampSamples,
  timing_breakdown: final.keyTimingBreakdown,
  timing_breakdown_coverage: {
    required: presses.filter((press) => Number.isFinite(press.stampedTic)).length,
    complete: presses.filter((press) => completeTiming(press, final)).length,
    pending: pendingTiming(final),
  },
  timing_breakdown_summary: Object.fromEntries([
    "page_to_post_ms_estimated",
    "wait_to_deposit_ms_measured",
    "cursor_to_tic_ms_measured",
    "render_and_paint_ms_residual",
  ].map((name) => {
    const values = final.keyTimingBreakdown
      .map((row) => row[name])
      .filter((value) => Number.isFinite(value));
    return [name, summarize(values)];
  })),
  engine_tics_per_second: final.ticsPerSecond,
  final,
  relayer_health: relayerHealth,
};
writeFileSync(`${OUT}/proof.json`, JSON.stringify(result, null, 2));
console.log("painted fps:", result.window.painted_fps.toFixed(2), "torn:", tornInWindow);
console.log("keypress latency ms:", JSON.stringify(result.keypress_latency));
console.log("next picture ms:", JSON.stringify(result.next_picture));
console.log("coverage:", JSON.stringify(result.coverage));
console.log("push:", JSON.stringify(final.stream));
console.log("cross-check:", JSON.stringify(final.lastCrossCheck));
console.log("panel:", JSON.stringify(final.panel));
console.log("relayer frame_push:", JSON.stringify(relayerHealth?.frame_push));
console.log("error:", final.error, "banner:", final.banner);
ws.close();
closeChrome();
