#!/usr/bin/env node
// Drive two isolated local Chrome profiles against a public-mode relayer.
// The pages and their burner keys stay local; only their signed inputs reach
// the supplied relayer URL.
import {spawn} from "node:child_process";
import {mkdirSync, mkdtempSync, rmSync} from "node:fs";
import {createServer as createNetServer} from "node:net";
import {dirname, join, resolve} from "node:path";
import {setTimeout as delay} from "node:timers/promises";
import {fileURLToPath} from "node:url";

const [pageUrl, chromePath, durationText = "30", portOneText = "19221", portTwoText = "19222", profileParent = "out/runs/doom-public-browser-probe"] = process.argv.slice(2);
const durationSeconds = Number(durationText);
const ports = [Number(portOneText), Number(portTwoText)];
if (!pageUrl || !chromePath || !Number.isInteger(durationSeconds) || durationSeconds < 5 || durationSeconds > 60 || ports.some((port) => !Number.isInteger(port) || port < 1024 || port > 65535)) {
  throw new Error("usage: doom_public_browser_probe.mjs PAGE_URL CHROME [ACTIVE_SECONDS 5..60] [DEBUG_PORT_1] [DEBUG_PORT_2] [PROFILE_PARENT]");
}
const relayBase = new URL(pageUrl).searchParams.get("relayer");
if (!relayBase) throw new Error("PAGE_URL needs ?relayer=http://127.0.0.1:PORT");
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profileRoot = resolve(repoRoot, profileParent);
mkdirSync(profileRoot, {recursive: true});
const tempProfiles = mkdtempSync(join(profileRoot, "chrome-profiles-"));
const browsers = [];
let browserStopPromise = null;

class Cdp {
  constructor(socketUrl) {
    this.socket = new WebSocket(socketUrl);
    this.sequence = 1;
    this.pending = new Map();
    this.opened = new Promise((resolveOpen, reject) => {
      const timer = setTimeout(() => reject(new Error("Chrome DevTools connection timed out")), 8_000);
      this.socket.addEventListener("open", () => { clearTimeout(timer); resolveOpen(); }, {once: true});
      this.socket.addEventListener("error", () => { clearTimeout(timer); reject(new Error("Chrome DevTools connection failed")); }, {once: true});
    });
    this.socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result ?? {});
    });
  }

  async send(method, params = {}) {
    await this.opened;
    const id = this.sequence++;
    return new Promise((resolveReply, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Chrome DevTools ${method} timed out`));
      }, 8_000);
      this.pending.set(id, {
        resolve: (value) => { clearTimeout(timer); resolveReply(value); },
        reject: (error) => { clearTimeout(timer); reject(error); },
      });
      this.socket.send(JSON.stringify({id, method, params}));
    });
  }

  async evaluate(expression) {
    const result = await this.send("Runtime.evaluate", {expression, awaitPromise: true, returnByValue: true, userGesture: true});
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.text ?? "page evaluation failed");
    return result.result?.value;
  }

  close() { this.socket.close(); }
}

async function launch(port, index) {
  const portProbe = createNetServer();
  await new Promise((resolvePort, rejectPort) => {
    portProbe.once("error", rejectPort);
    portProbe.listen(port, "127.0.0.1", () => portProbe.close(resolvePort));
  }).catch(() => { throw new Error(`Chrome debug port ${port} is already in use; refusing to attach to an unrelated browser`); });
  const profile = join(tempProfiles, `browser-${index}`);
  const child = spawn(chromePath, [
    "--no-first-run", "--no-default-browser-check", "--headless=new", "--no-sandbox",
    "--disable-gpu", "--disable-dev-shm-usage", "--remote-debugging-address=127.0.0.1",
    `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "about:blank",
  ], {stdio: "ignore"});
  browsers.push({child, cdp: null});
  const deadline = Date.now() + 20_000;
  let target = null;
  while (Date.now() < deadline && child.exitCode === null) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`, {signal: AbortSignal.timeout(750)})).json();
      target = targets.find((row) => row.type === "page");
      if (target?.webSocketDebuggerUrl) break;
    } catch { /* Chrome is still opening. */ }
    await delay(150);
  }
  if (!target?.webSocketDebuggerUrl) throw new Error(`Chrome ${index} did not expose a page target`);
  const cdp = new Cdp(target.webSocketDebuggerUrl);
  browsers.at(-1).cdp = cdp;
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", {source: `(() => {
    const OriginalEventSource = window.EventSource;
    window.__doomPublicFrames = [];
    window.EventSource = class extends OriginalEventSource {
      addEventListener(type, listener, options) {
        if (type !== "frame") return super.addEventListener(type, listener, options);
        return super.addEventListener(type, (event) => {
          try {
            const record = JSON.parse(event.data);
            window.__doomPublicFrames.push({frame_index: record.frame_index, at_ms: Date.now()});
          } catch {}
          listener.call(this, event);
        }, options);
      }
    };
  })();`});
  await cdp.send("Page.navigate", {url: pageUrl});
  return cdp;
}

async function pageState(cdp) {
  return cdp.evaluate(`JSON.stringify({
    connection: document.querySelector("#connection")?.textContent ?? "",
    turn: document.querySelector("#turn")?.textContent ?? "",
    nickname: document.querySelector("#playing")?.textContent ?? "",
    error: document.querySelector("#error")?.textContent ?? "",
    token: sessionStorage.getItem("doom-public.token"),
    frames: window.__doomPublicFrames ?? []
  })`).then(JSON.parse);
}

async function waitFor(cdp, predicate, label, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const state = await pageState(cdp);
    if (predicate(state)) return state;
    await delay(200);
  }
  throw new Error(`timed out waiting for ${label}: ${JSON.stringify(await pageState(cdp))}`);
}

async function publicState(token) {
  const url = new URL("/v1/public/state", relayBase);
  url.searchParams.set("token", token);
  const response = await fetch(url, {signal: AbortSignal.timeout(3_000)});
  if (!response.ok) throw new Error(`public state returned ${response.status}`);
  return response.json();
}

function percentile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(fraction * sorted.length) - 1)];
}

async function stopBrowsers() {
  if (browserStopPromise) return browserStopPromise;
  browserStopPromise = (async () => {
  for (const {child, cdp} of browsers) {
    cdp?.close();
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  }
  await Promise.all(browsers.map(({child}) => new Promise((resolveExit) => {
    if (child.exitCode !== null || child.signalCode !== null) return resolveExit();
    const timer = setTimeout(() => { child.kill("SIGKILL"); resolveExit(); }, 3_000);
    child.once("exit", () => { clearTimeout(timer); resolveExit(); });
  })));
  rmSync(tempProfiles, {recursive: true, force: true});
  })();
  return browserStopPromise;
}

process.once("SIGINT", () => { void stopBrowsers().finally(() => process.exit(130)); });
process.once("SIGTERM", () => { void stopBrowsers().finally(() => process.exit(143)); });

try {
  const playerCdp = await launch(ports[0], 1);
  const player = await waitFor(playerCdp, (state) => state.turn === "YOU" && state.token, "first browser to become player");
  const spectatorCdp = await launch(ports[1], 2);
  const spectator = await waitFor(spectatorCdp, (state) => state.turn === "SPECTATING" && state.token, "second browser to become spectator");
  await playerCdp.evaluate(`dispatchEvent(new KeyboardEvent("keydown", {code:"ArrowUp", key:"ArrowUp", bubbles:true, cancelable:true}))`);
  await playerCdp.evaluate(`dispatchEvent(new KeyboardEvent("keydown", {code:"ShiftLeft", key:"Shift", bubbles:true, cancelable:true}))`);
  const inputDeadline = Date.now() + 10_000;
  let playingState = null;
  while (Date.now() < inputDeadline) {
    playingState = await publicState(player.token);
    if (!playingState.paused) break;
    await delay(100);
  }
  if (!playingState || playingState.paused) throw new Error("the player input did not resume the shared sender");
  const activeStartMs = Date.now();
  await delay(durationSeconds * 1000);
  const activeEndMs = Date.now();
  await playerCdp.evaluate(`dispatchEvent(new KeyboardEvent("keyup", {code:"ArrowUp", key:"ArrowUp", bubbles:true, cancelable:true})); dispatchEvent(new KeyboardEvent("keyup", {code:"ShiftLeft", key:"Shift", bubbles:true, cancelable:true}))`);

  const pauseDeadline = Date.now() + 75_000;
  let beforePause = null;
  while (Date.now() < pauseDeadline) {
    const state = await publicState(player.token);
    if (state.paused_reason === "idle") { beforePause = state; break; }
    await delay(1_000);
  }
  if (!beforePause) throw new Error("the active session did not report an idle pause within 75 seconds");
  await delay(10_000);
  const afterPause = await publicState(player.token);

  const playerAfter = await pageState(playerCdp);
  const spectatorAfter = await pageState(spectatorCdp);
  const playerFrames = playerAfter.frames.filter((row) => row.at_ms >= activeStartMs && row.at_ms <= activeEndMs);
  const spectatorFrames = spectatorAfter.frames.filter((row) => row.at_ms >= activeStartMs && row.at_ms <= activeEndMs);
  const uniquePlayerFrames = new Map(playerFrames.map((row) => [row.frame_index, row]));
  const uniqueSpectatorFrames = new Map(spectatorFrames.map((row) => [row.frame_index, row]));
  const common = [...uniquePlayerFrames.keys()].filter((index) => uniqueSpectatorFrames.has(index));
  const delays = common.map((index) => Math.abs(uniquePlayerFrames.get(index).at_ms - uniqueSpectatorFrames.get(index).at_ms));
  const elapsedSeconds = (activeEndMs - activeStartMs) / 1000;
  const result = {
    browsers: 2,
    player_nickname: player.nickname,
    spectator_nickname: spectator.nickname,
    active_seconds: elapsedSeconds,
    player_unique_pictures: uniquePlayerFrames.size,
    player_pictures_per_second: uniquePlayerFrames.size / elapsedSeconds,
    comparison_baseline_pictures_per_second: 3.35,
    spectator_unique_pictures: uniqueSpectatorFrames.size,
    matched_frame_count: common.length,
    spectator_fanout_delay_ms: {
      median: percentile(delays, 0.5),
      p95: percentile(delays, 0.95),
      max: delays.length ? Math.max(...delays) : null,
    },
    idle_pause: {
      phase: afterPause.phase,
      paused_reason: afterPause.paused_reason,
      spend_before_fogo: beforePause.spend_fogo,
      spend_after_fogo: afterPause.spend_fogo,
      delta_fogo: Number((afterPause.spend_fogo - beforePause.spend_fogo).toFixed(9)),
    },
    final_public_state: afterPause,
  };
  console.log(JSON.stringify(result, null, 2));
} finally {
  await stopBrowsers();
}
