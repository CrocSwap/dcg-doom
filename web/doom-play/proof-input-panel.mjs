// The proof driver: press each of the six keys through Chrome's own key path
// and read the input panel back.
//
// Dependency-free CDP over a WebSocket this file implements itself, because the
// proof must not depend on a package that may or may not be installed. It reads
// the page's own panel (and the same doomPlay.inputLog the panel draws from),
// so what it reports is what a player would see.
//
// Usage: node proof-input-panel.mjs [out-dir]

import {spawn} from "node:child_process";
import {createHash} from "node:crypto";
import {mkdirSync, writeFileSync} from "node:fs";
import {createConnection} from "node:net";
import {setTimeout as sleep} from "node:timers/promises";

const OUT = process.argv[2] ?? ".";
const PAGE = process.env.PROOF_PAGE ?? "http://127.0.0.1:8012/web/doom-play/?relayer=http://127.0.0.1:8787&net=testnet";
const CHROME = process.env.PROOF_CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const PORT = Number(process.env.PROOF_CDP_PORT ?? 9333);
const HOLD_MS = Number(process.env.PROOF_HOLD_MS ?? 1400);
const SETTLE_MS = Number(process.env.PROOF_SETTLE_MS ?? 2600);

mkdirSync(OUT, {recursive: true});

// -- a minimal websocket client, enough for CDP -----------------------------

function wsConnect(url) {
  const parsed = new URL(url);
  const key = createHash("sha1").update(createHash("md5").update(String(Math.random())).digest()).digest("base64");
  return new Promise((resolve, reject) => {
    const socket = createConnection({host: parsed.hostname, port: Number(parsed.port || 80)});
    const pending = [];
    let buffer = Buffer.alloc(0);
    let handshaked = false;
    const listeners = [];
    const send = (text) => {
      const payload = Buffer.from(text);
      const header = payload.length < 126
        ? Buffer.from([0x81, 0x80 | payload.length])
        : Buffer.concat([Buffer.from([0x81, 0x80 | 126]), (() => { const b = Buffer.alloc(2); b.writeUInt16BE(payload.length); return b; })()]);
      const mask = Buffer.from([0, 0, 0, 0]);
      socket.write(Buffer.concat([header, mask, payload]));
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

const chrome = spawn(CHROME, [
  "--headless=new",
  `--remote-debugging-port=${PORT}`,
  "--remote-allow-origins=*",
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-gpu",
  "--window-size=1600,1200",
  "--user-data-dir=/tmp/doom-proof-profile",
  "about:blank",
], {stdio: ["ignore", "ignore", "pipe"]});
chrome.stderr.on("data", () => {});

async function target() {
  for (let attempt = 0; attempt < 60; attempt += 1) {
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
    type,
    code,
    key: keyName,
    windowsVirtualKeyCode,
    nativeVirtualKeyCode: windowsVirtualKeyCode,
  });
}
async function shot(name) {
  const answer = await call("Page.captureScreenshot", {format: "png"});
  writeFileSync(`${OUT}/${name}.png`, Buffer.from(answer.result.data, "base64"));
}

await call("Page.enable");
await call("Runtime.enable");
await call("Page.navigate", {url: PAGE});
await sleep(3500);

// JOIN, through the page's own button.
const joined = await evaluate(`(async () => {
  const button = document.getElementById("join");
  if (!button) return "no join button";
  button.click();
  for (let i = 0; i < 60; i += 1) {
    await new Promise((r) => setTimeout(r, 500));
    const state = window.doomPlay?.state?.session;
    if (state) return state.id;
  }
  return "no session after 30 s";
})()`);
console.log("session:", joined);
if (typeof joined !== "string" || !/^[0-9a-f]{8,}$/.test(joined)) {
  console.log("JOIN FAILED; the page says:", await evaluate(`document.getElementById("error")?.textContent ?? ""`));
  writeFileSync(`${OUT}/proof.json`, JSON.stringify({joined, ok: false}, null, 2));
  await shot("proof-join-failed");
  chrome.kill();
  process.exit(1);
}

// Wait for the first frame so a landing is a change in the picture and not just
// a number in a table.
await sleep(6000);
const before = await evaluate(`(async () => {
  for (let i = 0; i < 40; i += 1) {
    if (window.doomPlay?.state?.lastFrame) break;
    await new Promise((r) => setTimeout(r, 500));
  }
  return {frame: window.doomPlay?.state?.lastFrame?.digest ?? null, painted: window.doomPlay?.state?.paintedFrames ?? 0};
})()`);
console.log("first frame:", before);

// `key` is what a player presses; `action` is the id the page files it under.
// They differ for fire: the page's KEY_MAP calls Space and F `attack` (the
// engine's button name) and labels the row FIRE, so asking the panel for a
// press named "fire" finds nothing and reports zero offered tics. Measured on
// the 2026-09-26 proof run against the live R12 context: `fire offered 0`
// while the receipt said 226 command tics landed.
const KEYS = [
  {key: "forward", action: "forward", code: "ArrowUp", keyName: "ArrowUp", vk: 38},
  {key: "back", action: "back", code: "ArrowDown", keyName: "ArrowDown", vk: 40},
  {key: "left", action: "left", code: "ArrowLeft", keyName: "ArrowLeft", vk: 37},
  {key: "right", action: "right", code: "ArrowRight", keyName: "ArrowRight", vk: 39},
  {key: "fire", action: "attack", code: "Space", keyName: " ", vk: 32},
  {key: "use", action: "use", code: "KeyR", keyName: "r", vk: 82},
];

const rows = [];
for (const spec of KEYS) {
  const pictureBefore = await evaluate(`window.doomPlay.state.lastFrame?.digest ?? null`);
  await key("keyDown", spec.code, spec.keyName, spec.vk);
  await sleep(HOLD_MS);
  await key("keyUp", spec.code, spec.keyName, spec.vk);
  await sleep(SETTLE_MS);
  const row = await evaluate(`(() => {
    const log = window.doomPlay.inputLog;
    const summary = log.summary();
    const press = [...log.presses].reverse().find((p) => p.action === ${JSON.stringify(spec.action)});
    const tics = press ? [...press.tics.entries()].map(([seq, t]) => ({seq, fate: t.fate, signature: t.signature ?? null})) : [];
    const byFate = {};
    for (const t of tics) byFate[t.fate] = (byFate[t.fate] ?? 0) + 1;
    const links = [...document.querySelectorAll("#inputlog-list .fate.landed a")].map((a) => a.getAttribute("href"));
    // What the CHAIN was given for this press's landed tics, not what the page
    // offered: the producer's ledger is the only copy of the bytes that reached
    // a DEPOSIT, and the sign of angleturn's high byte is what says whether left
    // turned left. Measured wrong once already: commit 49c282ebc swapped them
    // and only the player's own eyes caught it.
    const landed = tics.filter((t) => t.fate === "landed").map((t) => t.seq);
    const chain = [];
    for (const [runSeq, run] of log.runs) {
      for (const seq of landed) {
        if (seq < runSeq || seq >= runSeq + run.count) continue;
        const at = (seq - runSeq) * 4;
        const b = run.bytes.subarray(at, at + 4);
        if (b.length === 4) chain.push({seq, hex: [...b].map((x) => x.toString(16).padStart(2, "0")).join("")});
      }
    }
    const turns = [...new Set(chain.map((c) => c.hex.slice(4, 6)))].sort();
    const buttons = [...new Set(chain.map((c) => c.hex.slice(6, 8)))].sort();
    return {
      action: ${JSON.stringify(spec.action)},
      label: press?.label ?? null,
      offered: tics.length,
      byFate,
      signatures: [...new Set(tics.filter((t) => t.fate === "landed" && t.signature).map((t) => t.signature))],
      links,
      rowLinks: [...document.querySelectorAll("#inputlog-list .inputlog-row")].find((n) => n.dataset.action === ${JSON.stringify(spec.action)})?.querySelectorAll(".fate.landed a").length ?? 0,
      chainLanded: chain.length,
      chainTurnBytes: turns,
      chainButtonBytes: buttons,
      summary,
      panelRowText: [...document.querySelectorAll("#inputlog-list .inputlog-row")].find((n) => n.dataset.action === ${JSON.stringify(spec.action)})?.textContent?.replace(/\\s+/g, " ").trim() ?? null,
      banner: document.getElementById("inputlog-banner")?.hidden ? null : document.getElementById("inputlog-banner")?.textContent?.replace(/\\s+/g, " ").trim() ?? null,
      lead: document.getElementById("inputlog-lead")?.textContent ?? null,
      torn: document.getElementById("inputlog-torn")?.textContent ?? null,
      error: document.getElementById("error")?.hidden ? null : document.getElementById("error")?.textContent ?? null,
    };
  })()`);
  const pictureAfter = await evaluate(`window.doomPlay.state.lastFrame?.digest ?? null`);
  rows.push({...spec, pictureBefore, pictureAfter, pictureChanged: pictureBefore !== pictureAfter, ...row});
  console.log(
    spec.key.padEnd(8),
    "offered", String(row.offered).padStart(3),
    JSON.stringify(row.byFate),
    "links", String(row.rowLinks).padStart(2),
    "turn", (row.chainTurnBytes ?? []).join(",") || "-",
    "btn", (row.chainButtonBytes ?? []).join(",") || "-",
    "picture", pictureBefore === pictureAfter ? "same" : "CHANGED",
    row.banner ? `BANNER: ${row.banner.slice(0, 60)}` : "",
  );
  await shot(`proof-${spec.action}`);
}

const after = await evaluate(`({
  summary: window.doomPlay.inputLog.summary(),
  banner: document.getElementById("inputlog-banner")?.hidden ? null : document.getElementById("inputlog-banner")?.textContent?.replace(/\\s+/g, " ").trim() ?? null,
  painted: window.doomPlay.state.paintedFrames,
  torn: window.doomPlay.state.tornReads,
  lead: document.getElementById("inputlog-lead")?.textContent,
  panelText: document.getElementById("inputlog-list")?.textContent?.replace(/\\s+/g, " ").trim(),
  status: {
    cursor: window.doomPlay.state.cursor,
    leadTics: window.doomPlay.state.leadTics,
    roundTripMs: window.doomPlay.state.roundTripMs,
    ticsPerSecond: window.doomPlay.state.ticsPerSecond,
    latencies: window.doomPlay.state.pictureLatencies,
    producerNextCursor: window.doomPlay.state.producerNextCursor,
    producerGeneratedCursor: window.doomPlay.state.producerGeneratedCursor,
  },
})`);
await shot("proof-final");
writeFileSync(`${OUT}/proof.json`, JSON.stringify({page: PAGE, joined, before, rows, after}, null, 2));
console.log("summary:", JSON.stringify(after.summary));
console.log("status:", JSON.stringify(after.status));
console.log("banner:", after.banner);
console.log("torn reads:", after.torn, "painted:", after.painted);
ws.close();
chrome.kill();
