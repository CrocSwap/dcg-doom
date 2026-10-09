#!/usr/bin/env node
// Drive a public-mode Doom relayer through play, the idle pause, and a resume,
// the way the public page does (join, signed key-state input, heartbeats, the
// frame stream), and write what happened as JSON.
//
//   node scripts/doom_dcg_public_probe.mjs <relayer-url> <out.json> [play-s] [idle-s] [resume-s]
//
// Defaults: 30 s holding forward, 90 s idle (the lobby pauses after 60 s
// without input; the paused reading is taken 15 s before the idle ends), 30 s holding forward again. The verdict checks that frames
// arrive while playing, stop and spend stays flat while paused, and resume
// after the next input.

import {writeFileSync} from "node:fs";

import {BurnerWallet} from "../web/doom-play/burner.js";
import {encodeInput} from "../web/doom-play/input.js";

const [base, outPath, playS = "30", idleS = "90", resumeS = "30"] = process.argv.slice(2);
if (!base || !outPath) {
  console.error("usage: doom_dcg_public_probe.mjs <relayer-url> <out.json> [play-s] [idle-s] [resume-s]");
  process.exit(2);
}
const t0 = Date.now();
const now = () => (Date.now() - t0) / 1000;
const log = [];
const note = (what, extra = {}) => {
  const row = {t: Number(now().toFixed(2)), what, ...extra};
  log.push(row);
  console.error(JSON.stringify(row));
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function api(path, {method = "GET", body} = {}) {
  const response = await fetch(`${base}${path}`, {
    method,
    headers: body ? {"content-type": "application/json"} : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const value = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(`${method} ${path}: ${response.status} ${JSON.stringify(value).slice(0, 300)}`);
  return value;
}

const {privateKey, publicKey} = await crypto.subtle.generateKey({name: "Ed25519"}, true, ["sign", "verify"]);
const wallet = new BurnerWallet({privateKey, publicKey, publicKeyRaw: new Uint8Array(await crypto.subtle.exportKey("raw", publicKey))});
const joined = await api("/v1/public/join", {method: "POST", body: {publicKey: wallet.publicKeyHex}});
const token = joined.token;
const binding = Uint8Array.from(Buffer.from(joined.session.binding, "hex"));
note("joined", {phase: joined.state?.phase, slot: joined.session.slot});

// The frame stream: count frames as they arrive.
const frames = [];
const streamAbort = new AbortController();
(async () => {
  try {
    const response = await fetch(`${base}/v1/public/stream?token=${encodeURIComponent(token)}`, {signal: streamAbort.signal});
    const decoder = new TextDecoder();
    let buffer = "";
    for await (const chunk of response.body) {
      buffer += decoder.decode(chunk, {stream: true});
      let at;
      while ((at = buffer.indexOf("\n\n")) >= 0) {
        const block = buffer.slice(0, at);
        buffer = buffer.slice(at + 2);
        if (block.startsWith("event: frame")) frames.push(now());
      }
    }
  } catch (error) {
    if (error.name !== "AbortError") note("stream_error", {error: error.message});
  }
})();

const heartbeat = setInterval(() => void api("/v1/public/heartbeat", {method: "POST", body: {token}}).catch((e) => note("heartbeat_error", {error: e.message})), 10_000);
let seq = 0;
async function hold(keys, seconds) {
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) {
    seq += 1;
    const message = encodeInput({slot: joined.session.slot, keys, clientSeq: seq, issuedAt: Math.floor(Date.now() / 1000), binding});
    const signature = await wallet.sign(message);
    try {
      await api("/v1/public/input", {method: "POST", body: {
        token, publicKey: wallet.publicKeyHex,
        message: Buffer.from(message).toString("base64"), signature: Buffer.from(signature).toString("base64"),
      }});
    } catch (error) {
      note("input_error", {error: error.message.slice(0, 200)});
    }
    await sleep(250);
  }
}
async function state(label) {
  const value = await api(`/v1/public/state?token=${encodeURIComponent(token)}`);
  note(label, {phase: value.phase, paused: value.paused, paused_reason: value.paused_reason,
    spend_fogo: value.spend_fogo, spend_cap_fogo: value.spend_cap_fogo, death: value.death_message});
  return value;
}

const forward = {forward: true, back: false, left: false, right: false, strafeLeft: false, strafeRight: false, attack: false, use: false, run: false};
const idleKeys = {...forward, forward: false};
const phases = {};
const play = async (name, seconds) => {
  const from = now();
  await hold(forward, seconds);
  await hold(idleKeys, 1);
  phases[name] = {from, to: now(), state: await state(`${name}_end`)};
};
await state("start");
await play("play", Number(playS));
// Idle: no input, heartbeats only. The lobby pauses after 60 s.
const idleFrom = now();
await sleep(Number(idleS) * 1000 - 15_000);
const pausedState = await state("idle_paused");
const pausedAt = now();
await sleep(15_000);
phases.idle = {from: idleFrom, to: now(), paused_at: pausedAt, state: await state("idle_end")};
await play("resume", Number(resumeS));

clearInterval(heartbeat);
await fetch(`${base}/v1/public/leave?token=${encodeURIComponent(token)}`, {method: "DELETE"}).catch(() => {});
streamAbort.abort();
const count = (from, to) => frames.filter((t) => t >= from && t < to).length;
const result = {
  relayer: base,
  frames_total: frames.length,
  frames_play: count(phases.play.from, phases.play.to),
  // Frames already in flight finish after the pause; count only the window
  // after the paused state was observed.
  frames_while_paused: count(pausedAt, phases.idle.to),
  frames_resume: count(phases.resume.from, phases.resume.to),
  paused_seen: pausedState.paused === true && pausedState.paused_reason === "idle",
  spend_fogo: {play_end: phases.play.state.spend_fogo, paused: pausedState.spend_fogo,
    idle_end: phases.idle.state.spend_fogo, resume_end: phases.resume.state.spend_fogo},
  log,
};
result.verdict = result.frames_play > 0 && result.paused_seen && result.frames_while_paused === 0
  && result.frames_resume > 0 && result.spend_fogo.idle_end === result.spend_fogo.paused ? "pass" : "fail";
writeFileSync(outPath, `${JSON.stringify(result, null, 1)}\n`);
console.log(JSON.stringify({verdict: result.verdict, frames_play: result.frames_play,
  frames_while_paused: result.frames_while_paused, frames_resume: result.frames_resume, spend_fogo: result.spend_fogo}));
process.exit(result.verdict === "pass" ? 0 : 1);
