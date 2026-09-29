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

const [pageUrl, chromePath, durationText = "30", portOneText = "19221", portTwoText = "19222", profileParent = "out/runs/doom-public-browser-probe", stageName = "complete"] = process.argv.slice(2);
const durationSeconds = Number(durationText);
const ports = [Number(portOneText), Number(portTwoText)];
const supportedStages = new Set(["complete", "active", "continue", "idle", "cleanup"]);
const maxDuration = stageName === "complete" ? 60 : 23;
if (!pageUrl || !chromePath || !Number.isInteger(durationSeconds) || durationSeconds < 5 || durationSeconds > maxDuration || ports.some((port) => !Number.isInteger(port) || port < 1024 || port > 65535) || !supportedStages.has(stageName)) {
  throw new Error("usage: doom_public_browser_probe.mjs PAGE_URL CHROME [ACTIVE_SECONDS 5..60] [DEBUG_PORT_1] [DEBUG_PORT_2] [PROFILE_PARENT] [complete|active|continue|idle|cleanup]");
}
const relayBase = new URL(pageUrl).searchParams.get("relayer");
if (!relayBase) throw new Error("PAGE_URL needs ?relayer=http://127.0.0.1:PORT");
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profileRoot = resolve(repoRoot, profileParent);
mkdirSync(profileRoot, {recursive: true});
const persistentProfiles = join(profileRoot, "persistent-chrome-profiles");
const temporaryProfiles = stageName === "complete" ? mkdtempSync(join(profileRoot, "chrome-profiles-")) : persistentProfiles;
if (stageName === "active") {
  for (let index = 1; index <= 2; index += 1) {
    const pidPath = join(persistentProfiles, `browser-${index}.pid`);
    if (await import("node:fs/promises").then(({access}) => access(pidPath).then(() => true).catch(() => false))) {
      const {readFileSync} = await import("node:fs");
      const pid = Number(readFileSync(pidPath, "utf8").trim());
      try { process.kill(pid, 0); throw new Error(`persistent Chrome PID ${pid} is still running; use the continue/cleanup stages`); } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    }
  }
  rmSync(persistentProfiles, {recursive: true, force: true});
}
if (stageName !== "complete") mkdirSync(persistentProfiles, {recursive: true});
const browsers = [];
let browserStopPromise = null;
let preserveBrowsers = false;

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

async function connect(port, index) {
  const deadline = Date.now() + 8_000;
  let target = null;
  while (Date.now() < deadline) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`, {signal: AbortSignal.timeout(750)})).json();
      target = targets.find((row) => row.type === "page");
      if (target?.webSocketDebuggerUrl) break;
    } catch { /* Chrome is opening. */ }
    await delay(100);
  }
  if (!target?.webSocketDebuggerUrl) throw new Error(`Chrome ${index} did not expose a page target on port ${port}`);
  const cdp = new Cdp(target.webSocketDebuggerUrl);
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  return cdp;
}

async function launch(port, index) {
  const portProbe = createNetServer();
  await new Promise((resolvePort, rejectPort) => {
    portProbe.once("error", rejectPort);
    portProbe.listen(port, "127.0.0.1", () => portProbe.close(resolvePort));
  }).catch(() => { throw new Error(`Chrome debug port ${port} is already in use; refusing to attach to an unrelated browser`); });
  const profile = join(temporaryProfiles, `browser-${index}`);
  const child = spawn(chromePath, [
    "--no-first-run", "--no-default-browser-check", "--headless=new", "--no-sandbox",
    "--disable-gpu", "--disable-dev-shm-usage", "--remote-debugging-address=127.0.0.1",
    `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "about:blank",
  ], {stdio: "ignore"});
  browsers.push({child, cdp: null});
  if (stageName !== "complete") {
    mkdirSync(persistentProfiles, {recursive: true});
    const {writeFileSync} = await import("node:fs");
    writeFileSync(join(persistentProfiles, `browser-${index}.pid`), `${child.pid}\n`, {mode: 0o600});
  }
  const deadline = Date.now() + 12_000;
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

async function waitFor(cdp, predicate, label, timeoutMs = 15_000) {
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
  if (stageName === "complete") {
    rmSync(temporaryProfiles, {recursive: true, force: true});
  } else {
    const {readFileSync, existsSync} = await import("node:fs");
    const {spawnSync} = await import("node:child_process");
    const tracked = [];
    for (let index = 1; index <= 2; index += 1) {
      const pidPath = join(persistentProfiles, `browser-${index}.pid`);
      if (!existsSync(pidPath)) continue;
      const pid = Number(readFileSync(pidPath, "utf8").trim());
      try {
        const command = spawnSync("ps", ["-p", String(pid), "-o", "command="], {encoding: "utf8"}).stdout.trim();
        const profile = join(persistentProfiles, `browser-${index}`);
        if (command.includes(`--user-data-dir=${profile}`)) {
          process.kill(pid, "SIGTERM");
          tracked.push({pid, profile});
        }
      } catch { /* an already exited browser needs no signal */ }
    }
    const stillRunning = ({pid, profile}) => {
      const command = spawnSync("ps", ["-p", String(pid), "-o", "command="], {encoding: "utf8"}).stdout.trim();
      return command.includes(`--user-data-dir=${profile}`);
    };
    const exitDeadline = Date.now() + 3_000;
    while (tracked.some(stillRunning) && Date.now() < exitDeadline) await delay(100);
    for (const browser of tracked) {
      if (stillRunning(browser)) {
        try { process.kill(browser.pid, "SIGKILL"); } catch { /* it exited between the check and signal */ }
      }
    }
    if (stageName === "cleanup") rmSync(persistentProfiles, {recursive: true, force: true});
  }
  })();
  return browserStopPromise;
}

process.once("SIGINT", () => { void stopBrowsers().finally(() => process.exit(130)); });
process.once("SIGTERM", () => { void stopBrowsers().finally(() => process.exit(143)); });

try {
  const attachExisting = stageName === "continue" || stageName === "idle" || stageName === "cleanup";
  const playerCdp = attachExisting ? await connect(ports[0], 1) : await launch(ports[0], 1);
  if (attachExisting) browsers.push({child: {exitCode: 0, signalCode: null}, cdp: playerCdp});
  else browsers.at(-1).cdp = playerCdp;
  const preserveTurnCheck = stageName === "active" || stageName === "continue" || stageName === "complete";
  const player = await waitFor(playerCdp, (state) => Boolean(state.token) && (!preserveTurnCheck || state.turn === "YOU"), "player browser session");
  const spectatorCdp = attachExisting ? await connect(ports[1], 2) : await launch(ports[1], 2);
  if (attachExisting) browsers.push({child: {exitCode: 0, signalCode: null}, cdp: spectatorCdp});
  else browsers.at(-1).cdp = spectatorCdp;
  const spectator = await waitFor(spectatorCdp, (state) => Boolean(state.token) && (!preserveTurnCheck || state.turn === "SPECTATING"), "spectator browser session");
  if (stageName === "cleanup") {
    console.log(JSON.stringify({stage: stageName, player: player.nickname, spectator: spectator.nickname}));
    preserveBrowsers = false;
  } else if (stageName === "idle") {
    const deadline = Date.now() + 62_000;
    let paused = null;
    while (Date.now() < deadline) {
      paused = await publicState(player.token);
      if (paused.paused_reason === "idle") break;
      await delay(500);
    }
    if (!paused || paused.paused_reason !== "idle") throw new Error("the active session did not report its idle pause within 62 seconds");
    console.log(JSON.stringify({stage: stageName, player: player.nickname, spectator: spectator.nickname, public_state: paused}, null, 2));
    preserveBrowsers = true;
  } else {
  await playerCdp.evaluate(`dispatchEvent(new KeyboardEvent("keydown", {code:"ArrowUp", key:"ArrowUp", bubbles:true, cancelable:true}))`);
  await playerCdp.evaluate(`dispatchEvent(new KeyboardEvent("keydown", {code:"ShiftLeft", key:"Shift", bubbles:true, cancelable:true}))`);
  const inputDeadline = Date.now() + 8_000;
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

  let beforePause = null;
  let afterPause = null;
  if (stageName === "complete") {
    const pauseDeadline = Date.now() + 75_000;
    while (Date.now() < pauseDeadline) {
      const state = await publicState(player.token);
      if (state.paused_reason === "idle") { beforePause = state; break; }
      await delay(1_000);
    }
    if (!beforePause) throw new Error("the active session did not report an idle pause within 75 seconds");
    await delay(10_000);
    afterPause = await publicState(player.token);
  }

  const playerAfter = await pageState(playerCdp);
  const spectatorAfter = await pageState(spectatorCdp);
  const playerFrames = playerAfter.frames.filter((row) => row.at_ms >= activeStartMs && row.at_ms <= activeEndMs);
  const spectatorFrames = spectatorAfter.frames.filter((row) => row.at_ms >= activeStartMs && row.at_ms <= activeEndMs);
  const uniquePlayerFrames = new Map(playerFrames.map((row) => [row.frame_index, row]));
  const uniqueSpectatorFrames = new Map(spectatorFrames.map((row) => [row.frame_index, row]));
  const common = [...uniquePlayerFrames.keys()].filter((index) => uniqueSpectatorFrames.has(index));
  const delays = common.map((index) => uniqueSpectatorFrames.get(index).at_ms - uniquePlayerFrames.get(index).at_ms);
  const elapsedSeconds = (activeEndMs - activeStartMs) / 1000;
  const result = {
    browsers: 2,
    player_nickname: player.nickname,
    spectator_nickname: spectator.nickname,
    active_seconds: elapsedSeconds,
    player_unique_pictures: uniquePlayerFrames.size,
    player_pictures_per_second: uniquePlayerFrames.size / elapsedSeconds,
    comparison_baseline_pictures_per_second: 3.354,
    spectator_unique_pictures: uniqueSpectatorFrames.size,
    matched_frame_count: common.length,
    spectator_fanout_delay_ms: {
      median: percentile(delays, 0.5),
      p95: percentile(delays, 0.95),
      max: delays.length ? Math.max(...delays) : null,
    },
    stage: stageName,
    idle_pause: afterPause ? {
      phase: afterPause.phase,
      paused_reason: afterPause.paused_reason,
      spend_before_fogo: beforePause.spend_fogo,
      spend_after_fogo: afterPause.spend_fogo,
      delta_fogo: Number((afterPause.spend_fogo - beforePause.spend_fogo).toFixed(9)),
    } : null,
    final_public_state: afterPause ?? await publicState(player.token),
  };
  console.log(JSON.stringify(result, null, 2));
  preserveBrowsers = stageName === "active" || stageName === "continue";
  }
} finally {
  if (!preserveBrowsers) {
    await stopBrowsers();
    if (stageName !== "complete") rmSync(persistentProfiles, {recursive: true, force: true});
  }
}
