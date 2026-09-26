// The input panel's arithmetic: what it says about a keypress, and whether that
// is true. No DOM, because the part worth testing is which fate a tic gets and
// what the summary counts; the drawing is `document` calls over those answers.
//
// The numbers come from the 2026-09-26 play session
// (`out/play/relayer-session.json` and `input-batches.json` on Tokyo):
// `input_tics_staged: 7076`, `input_tics_deposited: 3436`,
// `input_command_tics_deposited: 215`, and the chain's cursor frozen at 45,656
// for the last 328 s of the session. The page reported none of that.

import test from "node:test";
import assert from "node:assert/strict";

import {InputLog, LANDED, MISSED, QUEUED_NEVER, REFUSED, TRIMMED, OVERWRITTEN, DROPPED, SPENT, PENDING} from "../inputlog.js";

const b64 = (bytes) => Buffer.from(bytes).toString("base64");

const cmd = (fwd, turn, buttons) => Uint8Array.from([fwd & 0xff, 0, turn & 0xff, buttons & 0xff]);
const FORWARD = cmd(50, 0, 0);
const LEFT = cmd(0, 4, 0);

function log() {
  return new InputLog({link: () => null});
}

// A batch the page signed, offered to the open presses.
function offer(instance, firstSeq, count, commands) {
  instance.noteBatch({firstSeq, count, commands, at: 0});
}

test("a keypress is one row, and it closes when the key comes up", () => {
  const instance = log();
  const press = instance.press("forward", "FORWARD");
  offer(instance, 1000, 16, new Uint8Array(16 * 4));
  assert.equal(instance.presses.length, 1);
  assert.equal(press.upAt, null);
  instance.release(press);
  assert.notEqual(press.upAt, null);
  // A second batch after the release is not this press's: the player is not
  // holding the key, so a tic written now belongs to no press.
  offer(instance, 1016, 16, new Uint8Array(16 * 4));
  assert.equal(press.tics.size, 16);
});

test("a DEPOSIT carrying the same bytes is landed, with the transaction", () => {
  const instance = log();
  const press = instance.press("forward", "FORWARD");
  const commands = new Uint8Array(16 * 4);
  commands.set(FORWARD, 0);
  offer(instance, 1000, 16, commands);
  instance.observe({
    cursor: 1000,
    input_capacity: 65_536,
    input_fate: {
      zero_deposited_through: 1016,
      runs: [{seq: 1000, count: 16, commands: b64(commands), signature: "5jDepositSignature", send: "ok"}],
    },
  });
  const tics = [...press.tics.values()];
  assert.equal(tics.length, 16);
  for (const tic of tics) {
    assert.equal(tic.fate, LANDED);
    assert.equal(tic.signature, "5jDepositSignature");
  }
  const summary = instance.summary();
  assert.equal(summary.landed, 16);
  assert.equal(summary.failed, 0);
  assert.equal(summary.ratio, 1);
});

test("a DEPOSIT carrying different bytes is overwritten, and says which", () => {
  const instance = log();
  const press = instance.press("left", "LEFT");
  const offered = new Uint8Array(16 * 4);
  offered.set(LEFT, 0);
  const deposited = new Uint8Array(16 * 4); // all zeros: the producer read it first
  offer(instance, 2000, 16, offered);
  instance.observe({
    input_fate: {zero_deposited_through: 2016, runs: [{seq: 2000, count: 16, commands: b64(deposited), signature: "sig", send: "ok"}]},
  });
  const tic = press.tics.get(2000);
  assert.equal(tic.fate, OVERWRITTEN);
  assert.match(tic.reason, /00000000/);
  assert.match(tic.reason, /00000400/);
  // Only the tic the page held a key on differs. The other fifteen carried an
  // empty command and the chain was given an empty command, which is a tic that
  // landed: "landed" means the chain was given the bytes this page wrote for
  // that tic, not that the tic had a key in it.
  const summary = instance.summary();
  assert.equal(summary.failed, 1);
  assert.equal(summary.landed, 15);
});

test("a tic the producer deposited before the write reached it is missed, not pending", () => {
  const instance = log();
  const press = instance.press("forward", "FORWARD");
  const commands = new Uint8Array(16 * 4);
  commands.set(FORWARD, 0);
  offer(instance, 3000, 16, commands);
  // No run for those tics, and the producer is past them: this is the input
  // drop, and the panel has to name it rather than leave it "queued".
  instance.observe({input_fate: {zero_deposited_through: 3016, runs: []}});
  assert.equal(press.tics.get(3000).fate, MISSED);
  const summary = instance.summary();
  assert.equal(summary.pending, 0);
  assert.equal(summary.failed, 16);
  assert.equal(summary.ratio, 0);
});

test("a stalled pipeline turns queued tics into a named failure, and says so loudly", () => {
  const instance = log();
  const press = instance.press("forward", "FORWARD");
  offer(instance, 4000, 16, new Uint8Array(16 * 4));
  instance.observe({
    cursor: 45_656,
    input_capacity: 65_536,
    input_fate: {zero_deposited_through: 4000, runs: []},
    pipeline: {stalled_seconds: 287.4, stalled: true, context_spent: false, cursor: 45_656, capacity: 65_536, alive: true},
  });
  assert.equal(press.tics.get(4015).fate, QUEUED_NEVER);
  assert.match(press.tics.get(4015).reason, /287 s/);
  const banner = instance.banner();
  assert.equal(banner.title, "PIPELINE STOPPED");
  assert.match(banner.text, /45,656/);
  // The measured play session: 19,880 of 65,536 slots were still unspent.
  assert.match(banner.text, /65,536/);
});

test("a spent context and a lost session each get their own banner", () => {
  const spent = log();
  spent.observe({pipeline: {stalled: true, stalled_seconds: 900, context_spent: true, cursor: 65_536, capacity: 65_536}});
  assert.equal(spent.banner().title, "CONTEXT SPENT");
  assert.match(spent.banner().text, /92 FOGO/);

  const ended = log();
  ended.close("the session is spent");
  assert.equal(ended.banner().title, "SESSION ENDED");
});

test("a refused batch and a trimmed head are recorded as themselves", () => {
  const instance = log();
  const refused = instance.press("forward", "FORWARD");
  instance.noteBatch({firstSeq: 5000, count: 16, commands: new Uint8Array(64), refused: true, reason: "first_seq 5000 is behind the slot's next_seq 5010", at: 0});
  assert.equal(refused.tics.get(5000).fate, REFUSED);
  assert.match(refused.tics.get(5000).reason, /next_seq/);

  const trimmed = instance.press("fire", "FIRE");
  const commands = new Uint8Array(16 * 4);
  commands.set(cmd(0, 0, 1), 0);
  instance.noteBatch({firstSeq: 6000, count: 16, commands, accepted: 12, acceptedFrom: 6004, trimmedFromSeq: 6004, at: 0});
  assert.equal(trimmed.tics.get(6000).fate, TRIMMED);
  assert.equal(trimmed.tics.get(6004).fate, PENDING);
  assert.ok(trimmed.notes.some((n) => /trimmed/.test(n.text)));
});

test("a deposit transaction that did not land is its own fate", () => {
  const instance = log();
  const press = instance.press("use", "USE");
  const commands = new Uint8Array(16 * 4);
  commands.set(cmd(0, 0, 2), 0);
  offer(instance, 7000, 16, commands);
  instance.observe({input_fate: {zero_deposited_through: 7016, runs: [{seq: 7000, count: 16, commands: b64(commands), signature: "sig", send: "exception:TimeoutError"}]}});
  assert.equal(press.tics.get(7000).fate, DROPPED);
  assert.match(press.tics.get(7000).reason, /TimeoutError/);
});

test("fates are grouped into runs, one per transaction", () => {
  const instance = log();
  const press = instance.press("forward", "FORWARD");
  const commands = new Uint8Array(16 * 4);
  commands.set(FORWARD, 0);
  offer(instance, 8000, 16, commands);
  instance.observe({input_fate: {zero_deposited_through: 8016, runs: [{seq: 8000, count: 16, commands: b64(commands), signature: "sig", send: "ok"}]}});
  const rows = instance.rows(press);
  assert.equal(rows.length, 1);
  assert.deepEqual([rows[0].from, rows[0].to, rows[0].count], [8000, 8015, 16]);
  assert.equal(rows[0].fate, LANDED);
});

test("the summary counts presses, landed, failed and the lead in ms", () => {
  const instance = log();
  instance.press("forward", "FORWARD");
  const press = instance.open()[0];
  const commands = new Uint8Array(32 * 4);
  commands.set(FORWARD, 0);
  offer(instance, 9000, 32, commands);
  instance.observe({
    cursor: 9000,
    input_fate: {
      zero_deposited_through: 9032,
      runs: [{seq: 9000, count: 16, commands: b64(commands.subarray(0, 64)), signature: "sig", send: "ok"}],
    },
  });
  instance.setLead(21, 25.9);
  const summary = instance.summary();
  assert.equal(summary.presses, 1);
  assert.equal(summary.landed, 16);
  assert.equal(summary.failed, 16);
  assert.equal(summary.leadTics, 21);
  assert.ok(Math.abs(summary.leadMs - (21 / 25.9) * 1000) < 1e-9);
  assert.ok(Math.abs(summary.ratio - 0.5) < 1e-9);
  assert.equal(press.upAt, null);
});

test("SPENT beats stalled when both are true", () => {
  const instance = log();
  const press = instance.press("forward", "FORWARD");
  offer(instance, 10_000, 16, new Uint8Array(64));
  instance.observe({
    cursor: 65_536,
    input_fate: {zero_deposited_through: 10_000, runs: []},
    pipeline: {stalled: true, stalled_seconds: 400, context_spent: true, cursor: 65_536, capacity: 65_536},
  });
  assert.equal(press.tics.get(10_000).fate, SPENT);
});
