// The in-memory slot pool.
//
// A slot is one provisioned chain context: the context account, its SIM2 meta
// and input buffer, its state and mirror, and the render buffer's snapshot and
// eight strip accounts. Players cannot make their own, because a context costs
// about 73 FOGO of rent (71.70 FOGO measured for R8i, plus a 1 FOGO authority
// reserve), so the relayer hands out slots from a pool it was provisioned with.
//
// Today the pool is a plain list and a slot is single-use: once a session ends
// the slot is spent, because the deployed program refuses EXTEND-CAPACITY and
// there is no in-place reset. See docs/design/doom-relayer-api-2026-09-25.md
// for what a slot pool needs from the program to be reusable.

import {randomUUID, createHash} from "node:crypto";

export const SLOT_STATE = {
  IDLE: "idle",
  IN_USE: "in_use",
  SPENT: "spent",
  BROKEN: "broken",
};

export class PoolError extends Error {
  constructor(message, code, detail = {}) {
    super(message);
    this.name = "PoolError";
    this.code = code;
    Object.assign(this, detail);
  }
}

// The identity a slot binds to. Everything the relayer needs to address the
// chain lives here and nowhere else.
export function makeSlot({slot, context, meta, inputBuffer, state, mirror, authority, snapshot, strips, inputCapacity, label = null}) {
  if (slot === undefined || slot === null) throw new PoolError("a slot needs an index", "slot_missing");
  if (!Array.isArray(strips) || strips.length !== 8) {
    throw new PoolError("a slot needs exactly 8 strip accounts", "strips_missing", {count: strips?.length});
  }
  for (const [name, value] of Object.entries({context, meta, inputBuffer, state, mirror, authority, snapshot})) {
    if (!value) throw new PoolError(`a slot needs a ${name}`, `${name}_missing`);
  }
  return {
    slot,
    label,
    accounts: {context, meta, inputBuffer, state, mirror, authority, snapshot},
    strips: [...strips],
    inputCapacity,
    state: SLOT_STATE.IDLE,
    sessionId: null,
    sessionPlayerKey: null,
    // What the relayer has actually landed on this slot. The viewer reads
    // these accounts, so the relayer is the one that knows which tic the
    // frames the player is watching belong to.
    cursor: 0,
    gametic: 0,
    nextSeq: 0,
    ticsLanded: 0,
    framesLanded: 0,
    lastFrameSignature: null,
    lastFrameAt: 0,
    createdAt: Date.now(),
  };
}

// The 32-byte value a signed input batch carries, so a signature made for one
// slot cannot be replayed onto another. Identical to the browser's slotBinding
// in web/doom-play/input.js; that module is the definition and the relayer
// imports it rather than repeating it.
export async function bindingFor(slot) {
  const {slotBinding} = await import("../../web/doom-play/input.js");
  return slotBinding({
    context: slot.accounts.context,
    meta: slot.accounts.meta,
    inputBuffer: slot.accounts.inputBuffer,
  });
}

export class SlotPool {
  constructor({slots = [], logger = null} = {}) {
    this.logger = logger;
    this.slots = [];
    for (const entry of slots) this.slots.push(makeSlot(entry));
  }

  get size() {
    return this.slots.length;
  }

  summary() {
    const counts = {idle: 0, in_use: 0, spent: 0, broken: 0};
    for (const slot of this.slots) counts[slot.state] += 1;
    return {
      total: this.slots.length,
      ...counts,
      available: counts.idle,
      ticsLanded: this.slots.reduce((sum, slot) => sum + slot.ticsLanded, 0),
      framesLanded: this.slots.reduce((sum, slot) => sum + slot.framesLanded, 0),
    };
  }

  list() {
    return this.slots.map((slot) => ({
      slot: slot.slot,
      label: slot.label,
      state: slot.state,
      session_id: slot.sessionId,
      context: slot.accounts.context,
      cursor: slot.cursor,
      gametic: slot.gametic,
      next_seq: slot.nextSeq,
      input_capacity: slot.inputCapacity,
      tics_landed: slot.ticsLanded,
      frames_landed: slot.framesLanded,
    }));
  }

  bySlot(index) {
    const slot = this.slots.find((candidate) => candidate.slot === index);
    if (!slot) throw new PoolError(`no slot ${index}`, "unknown_slot", {slot: index});
    return slot;
  }

  bySession(sessionId) {
    const slot = this.slots.find((candidate) => candidate.sessionId === sessionId);
    if (!slot) throw new PoolError(`no session ${sessionId}`, "unknown_session", {session_id: sessionId});
    return slot;
  }

  take() {
    const slot = this.slots.find((candidate) => candidate.state === SLOT_STATE.IDLE);
    if (!slot) {
      const summary = this.summary();
      throw new PoolError(
        `no free slot (${summary.total} provisioned: ${summary.idle} idle, ${summary.in_use} in use, ${summary.spent} spent)`,
        "pool_empty",
        summary,
      );
    }
    return slot;
  }

  // Hands a slot to a session. The session id is derived from the slot and the
  // player key so that re-joining with the same burner after a page reload
  // finds the same session instead of stranding a slot.
  assign({playerKey, requestedSlot = null}) {
    const slot = requestedSlot === null ? this.take() : this.bySlot(requestedSlot);
    if (slot.state !== SLOT_STATE.IDLE) {
      throw new PoolError(`slot ${slot.slot} is ${slot.state}`, "slot_unavailable", {slot: slot.slot, state: slot.state});
    }
    const sessionId = createHash("sha256")
      .update(`doom-relayer/1\0${slot.slot}\0${playerKey ?? "anonymous"}`)
      .digest("hex")
      .slice(0, 32);
    slot.state = SLOT_STATE.IN_USE;
    slot.sessionId = sessionId;
    slot.sessionPlayerKey = playerKey ?? null;
    slot.claimedAt = Date.now();
    return {slot, sessionId};
  }

  release(sessionId, {spent = true, reason = "ended"} = {}) {
    const slot = this.bySession(sessionId);
    slot.state = spent ? SLOT_STATE.SPENT : SLOT_STATE.BROKEN;
    slot.releasedAt = Date.now();
    slot.releaseReason = reason;
    slot.sessionId = null;
    slot.sessionPlayerKey = null;
    return slot;
  }

  // The session a player currently holds, if any. A page reload presents the
  // same burner key, so this is how a relayer hands the session back instead of
  // stranding the slot and taking a second one.
  sessionForPlayer(playerKey) {
    if (!playerKey) return null;
    return this.slots.find((slot) => slot.sessionPlayerKey === playerKey && slot.state === SLOT_STATE.IN_USE) ?? null;
  }
}

export function newSessionId() {
  return randomUUID();
}
