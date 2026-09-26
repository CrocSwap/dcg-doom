// The input diagnostics panel: what happened to every key press.
//
// The page's own counters could say "688 tics accepted" and the game could still
// not move, and on 2026-09-26 it did not: the play session's receipt reads
// `input_tics_staged: 7076`, `input_tics_deposited: 3436` and
// `input_command_tics_deposited: 215`, so 80% of what the player pressed never
// reached a DEPOSIT and the page said nothing. This module is the answer to
// that: it keeps one row per key press, resolves every tic the press aimed at
// against what the producer actually deposited, and names the fate.
//
// The fates, and where each comes from:
//
//   landed            the producer's DEPOSIT for that tic carried the same four
//                     command bytes the page wrote, and the transaction landed.
//                     The row links that transaction on the Fogo explorer.
//   overwritten       a DEPOSIT for that tic landed carrying different bytes:
//                     a later batch of the page's own overwrote this one before
//                     the producer read it.
//   deposit failed    the DEPOSIT transaction itself failed or was dropped, so
//                     the chain never saw those bytes.
//   missed the slot   the producer deposited that tic BEFORE this write reached
//                     it, so the chain got a tic with no keys in it. This is the
//                     input drop, and it is measured here rather than inferred
//                     from a picture that did not change.
//   queued, never     offered above the producer's staging limit and the
//   deposited         pipeline stopped before it reached them.
//   refused           the relayer would not take the batch; the reason is shown.
//   trimmed           the head of the batch was already on chain, so only the
//                     tail was written.
//   context spent     every input slot on this context is on chain.
//   session ended     the relayer or the session is gone.
//
// Nothing here is consensus-sensitive: it reads what the relayer reports and
// draws it. The relayer's half is `input_fate` in the session status.

const MAX_PRESSES = 80;
const MAX_OFFERED_PER_PRESS = 4096;
const MAX_ROWS = 40;

export const PENDING = "pending";
export const LANDED = "landed";
export const OVERWRITTEN = "overwritten";
export const MISSED = "missed-the-slot";
export const QUEUED_NEVER = "queued-never-deposited";
export const REFUSED = "refused";
export const TRIMMED = "trimmed";
export const DROPPED = "deposit-failed";
export const SPENT = "context-spent";
export const ENDED = "session-ended";

// A failure is a tic that will not be the player's key on chain. PENDING is not
// a failure: it is a tic the producer has not reached yet, which is the normal
// state of the newest few tics at any moment.
const FAILURES = new Set([OVERWRITTEN, MISSED, QUEUED_NEVER, REFUSED, TRIMMED, DROPPED, SPENT, ENDED]);

export const FATE_LABEL = {
  [LANDED]: "landed",
  [OVERWRITTEN]: "overwritten",
  [MISSED]: "missed the slot",
  [QUEUED_NEVER]: "queued, never deposited",
  [REFUSED]: "refused by the relayer",
  [TRIMMED]: "trimmed",
  [DROPPED]: "deposit failed or dropped",
  [SPENT]: "context spent",
  [ENDED]: "session ended",
  [PENDING]: "queued",
};

const group = (value) => (typeof value === "number" ? value.toLocaleString("en-US") : String(value ?? "\u2014"));

const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

function fromBase64(text) {
  if (typeof text !== "string" || text.length === 0) return new Uint8Array(0);
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function clock(ms) {
  const at = new Date(ms);
  return `${String(at.getUTCHours()).padStart(2, "0")}:${String(at.getUTCMinutes()).padStart(2, "0")}:${String(at.getUTCSeconds()).padStart(2, "0")}`;
}

export class InputLog {
  // link(signature) -> an anchor for the explorer, or null when there is none.
  constructor({link}) {
    this.link = link;
    this.presses = [];
    this.runs = new Map();
    this.runIndex = new Map();
    this.zeroThrough = null;
    this.cursor = null;
    this.capacity = null;
    this.stalledSeconds = null;
    this.stalled = false;
    this.contextSpent = false;
    this.sessionEnded = false;
    this.leadTics = null;
    this.ticsPerSecond = null;
    this.dirty = true;
  }

  reset() {
    this.presses = [];
    this.runs = new Map();
    this.runIndex = new Map();
    this.zeroThrough = null;
    this.dirty = true;
  }

  // -- writing ------------------------------------------------------------

  press(action, label) {
    const record = {
      id: this.presses.length ? Math.max(...this.presses.map((p) => p.id)) + 1 : 1,
      action,
      label,
      downAt: performance.now(),
      upAt: null,
      offered: new Map(),
      tics: new Map(),
      notes: [],
    };
    this.presses.push(record);
    while (this.presses.length > MAX_PRESSES) this.presses.shift();
    this.dirty = true;
    return record;
  }

  release(record) {
    if (!record || record.upAt !== null) return;
    record.upAt = performance.now();
    this.dirty = true;
  }

  // The open presses, which is where a freshly signed batch's tics go.
  open() {
    return this.presses.filter((record) => record.upAt === null);
  }

  // A batch the page signed and offered. `commands` is what it aimed at, so the
  // panel can compare it byte for byte against what the chain was given.
  noteBatch({firstSeq, count, commands, accepted = count, acceptedFrom = firstSeq, trimmedFromSeq = null, queuedAboveStageLimit = null, refused = null, reason = null, at = performance.now()}) {
    for (const record of this.open()) {
      for (let i = 0; i < count; i += 1) {
        const seq = firstSeq + i;
        const bytes = commands.subarray(i * 4, i * 4 + 4);
        if (refused) {
          record.tics.set(seq, {fate: REFUSED, reason: reason ?? "refused"});
          continue;
        }
        if (trimmedFromSeq !== null && seq < trimmedFromSeq) {
          record.tics.set(seq, {fate: TRIMMED, reason: `at or below the chain's fixed input frontier ${trimmedFromSeq - 1}`});
          continue;
        }
        if (seq < acceptedFrom) continue;
        record.offered.set(seq, Uint8Array.from(bytes));
        record.tics.set(seq, {fate: PENDING, reason: null});
      }
      if (queuedAboveStageLimit !== null && queuedAboveStageLimit !== undefined) {
        const over = Math.max(0, firstSeq + count - queuedAboveStageLimit);
        if (over > 0) {
          record.notes.push({at, text: `aimed ${over} tics above the producer's staging limit ${queuedAboveStageLimit}: queued, and read only if the chain reaches it`});
        }
      }
      if (trimmedFromSeq !== null) {
        record.notes.push({at, text: `the head of the batch was already on chain; ${trimmedFromSeq - firstSeq} tics trimmed, ${accepted} written`});
      }
      if (refused) record.notes.push({at, text: `refused: ${reason ?? "no reason given"}`});
    }
    for (const record of this.presses) {
      while (record.offered.size > MAX_OFFERED_PER_PRESS) {
        record.offered.delete(record.offered.keys().next().value);
      }
    }
    this.dirty = true;
  }

  // -- reading ------------------------------------------------------------

  // Everything the relayer's status answer says about what the chain did. It is
  // called on the same 2 s poll as everything else, so it costs nothing.
  observe(payload) {
    if (!payload) return;
    const fate = payload.input_fate ?? null;
    if (fate) {
      if (typeof fate.zero_deposited_through === "number") this.zeroThrough = fate.zero_deposited_through;
      for (const run of fate.runs ?? []) {
        if (typeof run.seq !== "number") continue;
        this.runs.set(run.seq, {
          seq: run.seq,
          count: run.count,
          bytes: fromBase64(run.commands),
          signature: run.signature ?? null,
          send: run.send ?? null,
        });
      }
      this.runIndex.clear();
      // The producer's ledger is bounded, so drop what it has let go of.
      if (this.zeroThrough !== null) {
        for (const seq of [...this.runs.keys()]) {
          if (seq + (this.runs.get(seq)?.count ?? 0) <= this.zeroThrough - 4096) this.runs.delete(seq);
        }
      }
    }
    const pipeline = payload.pipeline ?? null;
    if (pipeline) {
      this.stalledSeconds = pipeline.stalled_seconds ?? null;
      this.stalled = Boolean(pipeline.stalled);
      this.contextSpent = Boolean(pipeline.context_spent);
      if (typeof pipeline.cursor === "number") this.cursor = pipeline.cursor;
      if (typeof pipeline.capacity === "number") this.capacity = pipeline.capacity;
    }
    if (typeof payload.cursor === "number") this.cursor = payload.cursor;
    if (typeof payload.input_capacity === "number") this.capacity = payload.input_capacity;
    this.resolve();
  }

  // The pipeline stopped, the context is spent, or the session is gone. Every
  // tic still waiting is named rather than left as "queued", because "queued"
  // is what a page shows when it is swallowing a keystroke.
  close(reason) {
    this.sessionEnded = true;
    this.endedReason = reason ?? null;
    this.resolve();
  }

  setLead(tics, ticsPerSecond) {
    if (tics !== null && tics !== undefined) this.leadTics = tics;
    if (ticsPerSecond) this.ticsPerSecond = ticsPerSecond;
    this.dirty = true;
  }

  // The deposit run that covers a tic, or null. The producer's runs are bounded
  // (128 rows, dropped from the front), so this is a scan and it is memoised:
  // a resolve pass asks about every offered tic once, and a run covers four.
  findRun(seq) {
    const cached = this.runIndex.get(seq);
    if (cached !== undefined) return cached;
    let found = null;
    for (const run of this.runs.values()) {
      if (seq >= run.seq && seq < run.seq + run.count) {
        found = run;
        break;
      }
    }
    if (this.runIndex.size > 8192) this.runIndex.clear();
    this.runIndex.set(seq, found);
    return found;
  }

  resolve() {
    const stop = this.contextSpent ? SPENT : this.sessionEnded ? ENDED : null;
    for (const record of this.presses) {
      for (const [seq, offered] of record.offered) {
        const current = record.tics.get(seq);
        if (!current || current.fate !== PENDING) continue;
        const run = this.findRun(seq);
        if (run) {
          const at = (seq - run.seq) * 4;
          const theirs = run.bytes.subarray(at, at + 4);
          const same = theirs.length === 4 && hex(theirs) === hex(offered);
          if (run.send && run.send !== "ok") {
            record.tics.set(seq, {fate: DROPPED, reason: `the DEPOSIT transaction did not land (${run.send})`, signature: run.signature});
          } else if (same) {
            record.tics.set(seq, {fate: LANDED, reason: null, signature: run.signature});
          } else {
            record.tics.set(seq, {
              fate: OVERWRITTEN,
              reason: `the chain was given ${hex(theirs)} for this tic, not ${hex(offered)}: a later batch overwrote it first`,
              signature: run.signature,
            });
          }
          continue;
        }
        if (this.zeroThrough !== null && seq < this.zeroThrough) {
          record.tics.set(seq, {
            fate: MISSED,
            reason: "the producer deposited this tic before the write reached it, so the chain ran it with no keys",
            signature: null,
          });
          continue;
        }
        if (stop) {
          record.tics.set(seq, {
            fate: this.contextSpent ? SPENT : QUEUED_NEVER,
            reason: this.contextSpent
              ? "every input slot on this context is on chain, so no keypress can be written"
              : `the pipeline stopped with the chain cursor at ${group(this.cursor)} and never reached this tic`,
            signature: null,
          });
          continue;
        }
        if (this.stalled) {
          record.tics.set(seq, {
            fate: QUEUED_NEVER,
            reason: `the chain cursor has not moved for ${(this.stalledSeconds ?? 0).toFixed(0)} s (at ${group(this.cursor)} of ${group(this.capacity)})`,
            signature: null,
          });
        }
      }
    }
    this.dirty = true;
  }

  // -- reading it back ----------------------------------------------------

  // One row per fate, with the tics it covers, so a 900-tic hold does not draw
  // 900 chips. Consecutive tics of the same fate and the same transaction are
  // one run, which is also what a DEPOSIT is.
  rows(record) {
    const groups = new Map();
    for (const seq of [...record.tics.keys()].sort((a, b) => a - b)) {
      const entry = record.tics.get(seq);
      const key = `${entry.fate}|${entry.signature ?? ""}|${entry.reason ?? ""}`;
      const last = [...groups.values()].at(-1);
      if (last && last.key === key && last.to === seq - 1) {
        last.to = seq;
        last.count += 1;
        continue;
      }
      groups.set(key, {key, fate: entry.fate, reason: entry.reason, signature: entry.signature, from: seq, to: seq, count: 1});
    }
    return [...groups.values()];
  }

  summary() {
    let landed = 0;
    let failed = 0;
    let pending = 0;
    for (const record of this.presses) {
      for (const entry of record.tics.values()) {
        if (entry.fate === LANDED) landed += 1;
        else if (entry.fate === PENDING) pending += 1;
        else if (FAILURES.has(entry.fate)) failed += 1;
      }
    }
    const resolved = landed + failed;
    return {
      presses: this.presses.length,
      landed,
      failed,
      pending,
      ratio: resolved ? landed / resolved : null,
      leadTics: this.leadTics,
      leadMs: this.leadTics && this.ticsPerSecond ? (this.leadTics / this.ticsPerSecond) * 1000 : null,
    };
  }

  // The loudest true thing about the pipeline, or null. This is what stops a
  // frozen chain from looking like an unresponsive keyboard.
  banner() {
    if (this.contextSpent) {
      return {
        tone: "bad",
        title: "CONTEXT SPENT",
        text: `All ${group(this.capacity)} input slots on this context are on chain, and the deployed program refuses extend-capacity. Keys cannot reach the game. A fresh context costs about 92 FOGO of rent and the relayer tree has scripts/doom_r10_fresh_context.sh.`,
      };
    }
    if (this.sessionEnded) {
      return {tone: "bad", title: "SESSION ENDED", text: this.endedReason ?? "The relayer is not answering, so nothing you press is signed into a DEPOSIT. Start it again with start.sh."};
    }
    if (this.stalled) {
      return {
        tone: "bad",
        title: "PIPELINE STOPPED",
        text: `The chain's cursor has not moved for ${(this.stalledSeconds ?? 0).toFixed(0)} s (at ${group(this.cursor)} of ${group(this.capacity)}). Everything you press is queued above the producer and will not be deposited. Stop the relayer (stop.sh) and start it again; the context resumes at the cursor it stopped at.`,
      };
    }
    return null;
  }

  // -- drawing ------------------------------------------------------------

  // `bannerNode` is a parameter and not a field because it is the one node the
  // panel may be drawn without: `node --test` has no document, and the offline
  // tests that check the drawing pass `null`.  It was a free variable instead,
  // so every call raised `ReferenceError: bannerNode is not defined` on the
  // first line and the panel never drew a row, the summary line never moved off
  // "no keys pressed", and the banner never appeared.  Measured on the
  // 2026-09-26 proof run against the live R12 context: six key presses, 278
  // tics offered, 204 resolved as landed with their deposit signatures in the
  // model, and zero rows in `#inputlog-list`.
  render(list, summaryNode, leadNode, tornNode, tornReads, bannerNode = null) {
    const banner = this.banner();
    if (bannerNode) {
      bannerNode.hidden = banner === null;
      if (banner) {
        bannerNode.className = `inputlog-banner${banner.tone === "bad" ? "" : " warn"}`;
        bannerNode.replaceChildren();
        const title = document.createElement("b");
        title.textContent = banner.title;
        bannerNode.append(title, document.createTextNode(banner.text));
      }
    }
    const summary = this.summary();
    if (summaryNode) {
      summaryNode.textContent = summary.presses === 0
        ? "no keys pressed"
        : `${summary.presses} pressed · ${summary.landed} landed · ${summary.failed} failed` +
          (summary.pending ? ` · ${summary.pending} queued` : "") +
          (summary.ratio === null ? "" : ` · ${(summary.ratio * 100).toFixed(0)}% landed`);
    }
    if (leadNode) {
      leadNode.textContent = summary.leadTics === null
        ? "—"
        : `${summary.leadTics} tics` + (summary.leadMs === null ? "" : ` · ${summary.leadMs.toFixed(0)} ms`) +
          (this.ticsPerSecond ? ` @ ${this.ticsPerSecond.toFixed(1)} tic/s` : "");
    }
    if (tornNode) tornNode.textContent = String(tornReads ?? 0);
    if (!list) return;
    if (this.presses.length === 0) {
      if (list.dataset.empty !== "1") {
        list.dataset.empty = "1";
        const empty = document.createElement("li");
        empty.className = "inputlog-empty";
        empty.textContent = "Press a key. Every press appears here with the tics it aimed at, whether they reached a DEPOSIT, and the transaction that carried them.";
        list.replaceChildren(empty);
      }
      return;
    }
    delete list.dataset.empty;
    const newest = [...this.presses].reverse().slice(0, MAX_ROWS);
    const fragment = document.createDocumentFragment();
    for (const record of newest) fragment.append(this.renderRow(record));
    list.replaceChildren(fragment);
  }

  renderRow(record) {
    const row = document.createElement("li");
    row.className = "inputlog-row";
    row.dataset.action = record.action;

    const head = document.createElement("div");
    head.className = "inputlog-head";
    const key = document.createElement("span");
    key.className = "inputlog-key";
    key.textContent = record.label;
    const when = document.createElement("span");
    when.className = "inputlog-when";
    when.textContent = record.upAt === null
      ? `${clock(record.downAt)} → held`
      : `${clock(record.downAt)} → ${clock(record.upAt)} · ${((record.upAt - record.downAt) / 1000).toFixed(2)} s`;
    const tics = document.createElement("span");
    tics.className = "inputlog-tics";
    const seqs = [...record.tics.keys()];
    tics.textContent = seqs.length
      ? `tics ${Math.min(...seqs)}–${Math.max(...seqs)}`
      : "no tics offered";
    head.append(key, when, tics);

    const fate = document.createElement("div");
    fate.className = "inputlog-fate";
    for (const group of this.rows(record)) {
      const chip = document.createElement("span");
      chip.className = `fate ${group.fate === PENDING ? "" : group.fate}`;
      const count = document.createElement("b");
      count.textContent = `${group.count} `;
      chip.append(count, document.createTextNode(FATE_LABEL[group.fate] ?? group.fate));
      if (group.signature && group.fate === LANDED) {
        const anchor = this.link?.(group.signature);
        if (anchor) {
          anchor.className = "";
          anchor.title = `${group.signature} on the Fogo testnet explorer`;
          chip.append(" ", anchor);
        }
      }
      fate.append(chip);
    }

    row.append(head, fate);
    const reasons = [...new Set([...this.rows(record)].map((g) => g.reason).filter(Boolean))];
    const notes = record.notes.slice(-2).map((n) => n.text);
    if (reasons.length || notes.length) {
      const detail = document.createElement("p");
      detail.className = "inputlog-detail";
      for (const text of [...reasons, ...notes]) {
        if (detail.childNodes.length) detail.append(document.createElement("br"));
        detail.append(document.createTextNode(text));
      }
      row.append(detail);
    }
    return row;
  }
}
