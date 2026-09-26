// A minimal Chrome DevTools Protocol client: a WebSocket this file implements
// itself, plus a launcher. Dependency-free on purpose -- the page and the
// relayer carry no package.json dependencies and neither does the proof, so a
// test that needs a browser cannot be the one thing that needs `npm install`.
//
// It is used by the offline page test (a fake relayer, a stubbed chain, no
// network) and by the two proof tools. What it adds over the copy inside
// `proof-frame-push.mjs` is `Fetch` request interception, so a page can be
// driven against a canned RPC without the page carrying a test-only hook.

import {spawn} from "node:child_process";
import {createHash, randomBytes} from "node:crypto";
import {createConnection} from "node:net";
import {existsSync, mkdtempSync} from "node:fs";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {setTimeout as sleep} from "node:timers/promises";

export const CHROME = process.env.PROOF_CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

export function chromeAvailable() {
  return existsSync(CHROME);
}

function wsConnect(url) {
  const parsed = new URL(url);
  const key = randomBytes(16).toString("base64");
  return new Promise((resolve, reject) => {
    const socket = createConnection({host: parsed.hostname, port: Number(parsed.port || 80)});
    let buffer = Buffer.alloc(0);
    let handshaked = false;
    const listeners = [];
    const send = (text) => {
      const payload = Buffer.from(text);
      const header = payload.length < 126
        ? Buffer.from([0x81, 0x80 | payload.length])
        : Buffer.concat([
          Buffer.from([0x81, 0x80 | 126]),
          (() => { const b = Buffer.alloc(2); b.writeUInt16BE(payload.length); return b; })(),
        ]);
      socket.write(Buffer.concat([header, Buffer.from([0, 0, 0, 0]), payload]));
    };
    socket.on("connect", () => {
      socket.write(
        `GET ${parsed.pathname}${parsed.search} HTTP/1.1\r\n` +
          `Host: ${parsed.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
          `Sec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`,
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

export class Browser {
  constructor({process: child, ws, port, profile}) {
    this.process = child;
    this.ws = ws;
    this.port = port;
    this.profile = profile;
    this.nextId = 0;
    this.waiting = new Map();
    this.handlers = new Map();
    this.closed = false;
    this.ws.onMessage((text) => this.#onMessage(text));
  }

  #onMessage(text) {
    const message = JSON.parse(text);
    if (message.id !== undefined && this.waiting.has(message.id)) {
      const {resolve, reject} = this.waiting.get(message.id);
      this.waiting.delete(message.id);
      if (message.error) reject(new Error(`${message.error.message} (${message.error.code})`));
      else resolve(message.result);
      return;
    }
    for (const fn of this.handlers.get(message.method) ?? []) fn(message.params, message.sessionId);
  }

  on(method, fn) {
    if (!this.handlers.has(method)) this.handlers.set(method, []);
    this.handlers.get(method).push(fn);
  }

  send(method, params = {}, sessionId = undefined) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, {resolve, reject});
      this.ws.send(JSON.stringify(sessionId ? {id, method, params, sessionId} : {id, method, params}));
    });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    try {
      this.ws.close();
    } catch {
      /* already gone */
    }
    this.process.kill();
  }
}

// Launch headless Chrome and attach to its first page target. Returns null when
// there is no Chrome on this machine, so a caller can skip rather than fail.
export async function launch({port = 9400 + Math.floor(Math.random() * 90), windowSize = "1600,1200"} = {}) {
  if (!chromeAvailable()) return null;
  const profile = mkdtempSync(join(tmpdir(), "doom-page-test-"));
  const child = spawn(CHROME, [
    "--headless=new",
    `--remote-debugging-port=${port}`,
    "--remote-allow-origins=*",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-gpu",
    `--window-size=${windowSize}`,
    `--user-data-dir=${profile}`,
    "about:blank",
  ], {stdio: ["ignore", "ignore", "ignore"]});
  let target = null;
  for (let attempt = 0; attempt < 80 && !target; attempt += 1) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      target = list.find((entry) => entry.type === "page") ?? null;
    } catch {
      /* not listening yet */
    }
    if (!target) await sleep(250);
  }
  if (!target) {
    child.kill();
    return null;
  }
  return new Browser({process: child, ws: await wsConnect(target.webSocketDebuggerUrl), port, profile});
}

// A page, with the two evaluate helpers every test here needs.
export class Page {
  constructor(browser) {
    this.browser = browser;
    this.consoleLines = [];
  }

  async open() {
    await this.browser.send("Page.enable");
    await this.browser.send("Runtime.enable");
  }

  // Record what the page threw, before any of the test's own code can see it.
  // The page's error bar is for the player; an exception the page swallowed on
  // its way to that bar is exactly what a test has to be able to read.
  async captureErrors() {
    await this.browser.send("Page.addScriptToEvaluateOnNewDocument", {
      source: `window.__errors = [];
        addEventListener("error", (e) => window.__errors.push(String((e.error && e.error.stack) || e.message)));
        addEventListener("unhandledrejection", (e) => window.__errors.push("unhandled " + String((e.reason && e.reason.stack) || e.reason)));`,
    });
  }

  async navigate(url) {
    await this.browser.send("Page.navigate", {url});
  }

  async evaluate(expression) {
    const answer = await this.browser.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (answer.exceptionDetails) {
      throw new Error(answer.exceptionDetails.exception?.description ?? "evaluate failed");
    }
    return answer.result?.value;
  }

  async key(type, code, keyName, windowsVirtualKeyCode) {
    await this.browser.send("Input.dispatchKeyEvent", {
      type, code, key: keyName,
      windowsVirtualKeyCode, nativeVirtualKeyCode: windowsVirtualKeyCode,
    });
  }

  async screenshot() {
    const answer = await this.browser.send("Page.captureScreenshot", {format: "png"});
    return Buffer.from(answer.data, "base64");
  }
}
