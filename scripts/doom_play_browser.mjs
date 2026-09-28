#!/usr/bin/env node
// Open the pinned Doom page and drive only the same JOIN/key events a person
// would use. Keeping this outside web/doom-play leaves the Tokyo source pin
// untouched while the launcher can still guarantee the first moving frame.

import {spawn} from "node:child_process";
import {mkdir} from "node:fs/promises";
import {setTimeout as delay} from "node:timers/promises";

const [url, profileDir, debugPortText, chromePath, headlessText = "0"] = process.argv.slice(2);
const debugPort = Number(debugPortText);
if (!url || !profileDir || !Number.isInteger(debugPort) || !chromePath) {
  throw new Error("usage: doom_play_browser.mjs URL PROFILE DEBUG_PORT CHROME [HEADLESS]");
}

await mkdir(profileDir, {recursive: true});
const args = [
  "--no-first-run",
  "--no-default-browser-check",
  "--remote-debugging-address=127.0.0.1",
  `--remote-debugging-port=${debugPort}`,
  `--user-data-dir=${profileDir}`,
];
if (headlessText === "1") {
  args.push("--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage");
}
args.push(url);

const chrome = spawn(chromePath, args, {stdio: "ignore"});
let stopRequested = false;
let targetSocket = null;
let keyTimer = null;

function writeKeyUp(cdp) {
  if (!cdp) return;
  void cdp.send("Runtime.evaluate", {
    expression: "dispatchEvent(new KeyboardEvent('keyup',{code:'ArrowUp',key:'ArrowUp',bubbles:true,cancelable:true}))",
    returnByValue: true,
  }).catch(() => {});
}

async function stop(signal = "SIGTERM") {
  if (stopRequested) return;
  stopRequested = true;
  if (keyTimer) clearTimeout(keyTimer);
  if (targetSocket) writeKeyUp(targetSocket);
  if (chrome.exitCode === null && chrome.signalCode === null) chrome.kill(signal);
  await Promise.race([
    new Promise((resolve) => chrome.once("exit", resolve)),
    delay(3000),
  ]);
  if (chrome.exitCode === null && chrome.signalCode === null) chrome.kill("SIGKILL");
}

process.once("SIGINT", () => { void stop("SIGINT").finally(() => process.exit(130)); });
process.once("SIGTERM", () => { void stop("SIGTERM").finally(() => process.exit(143)); });
chrome.once("error", (error) => {
  console.error(`The Doom browser could not start: ${error.message}; install Google Chrome or set CHROME_BIN, then rerun scripts/doom_play.sh.`);
  process.exitCode = 1;
});
chrome.once("exit", (code, signal) => {
  if (!stopRequested) {
    console.error(`The Doom browser exited (${signal ?? code}); rerun scripts/doom_play.sh.`);
    process.exitCode = 1;
  }
});

class Cdp {
  constructor(socket) {
    this.ws = new WebSocket(socket);
    this.sequence = 1;
    this.pending = new Map();
    this.opened = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("Chrome DevTools connection timed out")), 5000);
      this.ws.addEventListener("open", () => { clearTimeout(timer); resolve(); }, {once: true});
      this.ws.addEventListener("error", () => { clearTimeout(timer); reject(new Error("Chrome DevTools connection failed")); }, {once: true});
    });
    this.ws.addEventListener("message", (event) => {
      let message;
      try { message = JSON.parse(event.data); } catch { return; }
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message ?? "Chrome DevTools command failed"));
      else pending.resolve(message.result ?? {});
    });
  }

  async send(method, params = {}) {
    await this.opened;
    const id = this.sequence++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Chrome DevTools ${method} timed out`));
      }, 5000);
      this.pending.set(id, {
        resolve: (result) => { clearTimeout(timer); resolve(result); },
        reject: (error) => { clearTimeout(timer); reject(error); },
      });
      this.ws.send(JSON.stringify({id, method, params}));
    });
  }

  async evaluate(expression) {
    const response = await this.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    });
    if (response.exceptionDetails) throw new Error(response.exceptionDetails.text ?? "page evaluation failed");
    return response.result?.value;
  }

  close() { try { this.ws.close(); } catch {} }
}

async function findTarget(deadline) {
  while (Date.now() < deadline && !stopRequested && chrome.exitCode === null) {
    try {
      const response = await fetch(`http://127.0.0.1:${debugPort}/json/list`, {signal: AbortSignal.timeout(1000)});
      const rows = await response.json();
      const page = rows.find((row) => row.type === "page" && row.url.includes("/web/doom-play/"));
      if (page?.webSocketDebuggerUrl) return page;
    } catch {
      // Chrome is opening the local page.
    }
    await delay(200);
  }
  return null;
}

try {
  const target = await findTarget(Date.now() + 20_000);
  if (!target) throw new Error("Google Chrome did not open the Doom page within 20 seconds; check the local page port and rerun scripts/doom_play.sh.");
  targetSocket = new Cdp(target.webSocketDebuggerUrl);
  await targetSocket.send("Runtime.enable");
  let joinClicked = false;
  let inputStarted = false;
  let inputStartedAt = 0;
  let watchStartCursor = 0;
  let watchdogReported = false;
  const pageDeadline = Date.now() + 20_000;
  console.log("Doom is open in Chrome; joining the testnet session.");

  while (!stopRequested && chrome.exitCode === null) {
    const state = await targetSocket.evaluate(`(() => {
      const s = globalThis.doomPlay?.state;
      const button = document.querySelector('#join');
      return {
        ready: Boolean(s), wallet: Boolean(s?.wallet), session: Boolean(s?.session),
        joinDisabled: Boolean(button?.disabled), hasJoin: Boolean(button),
        cursor: Number(s?.cursor ?? -1), frameCursor: Number(s?.lastFrame?.cursor ?? -1),
        error: document.querySelector('#error')?.textContent ?? '',
      };
    })()`);

    if (!joinClicked && state.ready && state.wallet && state.hasJoin && !state.joinDisabled && !state.session) {
      joinClicked = true;
      await targetSocket.evaluate("document.querySelector('#join').click()");
    }
    if (!inputStarted && state.session) {
      inputStarted = true;
      inputStartedAt = Date.now();
      watchStartCursor = Math.max(state.cursor, state.frameCursor);
      await targetSocket.evaluate("dispatchEvent(new KeyboardEvent('keydown',{code:'ArrowUp',key:'ArrowUp',bubbles:true,cancelable:true}))");
      console.log("Doom joined; sending the opening move and watching for cursor progress.");
      keyTimer = setTimeout(() => {
        writeKeyUp(targetSocket);
      }, 9000);
    }
    if (inputStarted && !watchdogReported && Date.now() - inputStartedAt >= 15_000) {
      if (Math.max(state.cursor, state.frameCursor) > watchStartCursor) {
        watchdogReported = true;
        console.log(`Doom is moving at cursor ${Math.max(state.cursor, state.frameCursor)}.`);
      } else {
        watchdogReported = true;
        const sentence = "The game cursor did not move within 15 seconds of your first input. Press Ctrl-C and run scripts/doom_play.sh --repair to reinitialize R15.";
        await targetSocket.evaluate(`(() => {
          const stage = document.querySelector('#stage-state');
          const error = document.querySelector('#error');
          if (stage) { stage.textContent = 'PIPELINE STOPPED'; stage.classList.add('bad'); }
          if (error) { error.textContent = ${JSON.stringify(sentence)}; error.hidden = false; }
        })()`);
        console.error(sentence);
      }
    }
    if (!inputStarted && Date.now() >= pageDeadline) {
      const sentence = state.error
        ? `The Doom page could not join: ${state.error}. Press Ctrl-C and rerun scripts/doom_play.sh.`
        : "The Doom page did not join a testnet session within 20 seconds; press Ctrl-C and rerun scripts/doom_play.sh.";
      console.error(sentence);
      throw new Error(sentence);
    }
    await delay(250);
  }
  if (targetSocket) targetSocket.close();
  if (!stopRequested && chrome.exitCode !== null) process.exitCode = 1;
} catch (error) {
  if (!stopRequested) {
    console.error(error.message);
    process.exitCode = 1;
  }
} finally {
  targetSocket?.close();
  await stop("SIGTERM");
}
