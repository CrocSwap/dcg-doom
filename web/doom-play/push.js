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

import {decodeFrame, StripError, STRIP_COUNT} from "./strip.js";

export const DEFAULT_CROSS_CHECK_EVERY = 20;

function fromBase64(encoded) {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

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

// Compare a pushed frame against a frame read from the chain. The comparison is
// on the pixels and the identity fields, not on the whole record: a chain read
// taken a moment later can legitimately be a different frame, and the honest
// answer there is "it moved on", not "mismatch".
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
// a picture that is a frame old is still the right picture and a picture that
// is a frame out of order is a lie. Frames are therefore never dropped here --
// if the page is too slow to keep up, the queue grows and the page says so, and
// it is the operator who has to decide that a slow viewer wants skipping.
export class FrameStream {
  constructor({base, sessionId, onFrame, onCrossCheck, crossCheck, crossCheckEvery = DEFAULT_CROSS_CHECK_EVERY, EventSourceImpl = null, readChain = null} = {}) {
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
    this.skipped = 0;
    this.crossChecks = 0;
    this.crossCheckMismatches = 0;
    this.crossCheckUncomparable = 0;
    this.stopped = false;
    this.queue = [];
    this.draining = false;
  }

  get url() {
    const since = this.lastFrameIndex === null ? "" : `?since=${encodeURIComponent(this.lastFrameIndex)}`;
    return `${this.base}/v1/sessions/${encodeURIComponent(this.sessionId)}/frames${since}`;
  }

  start() {
    if (this.stopped) return this;
    if (!this.EventSourceImpl) throw new Error("this browser has no EventSource");
    this.source = new this.EventSourceImpl(this.url);
    this.source.addEventListener("frame", (event) => this.#receive(event));
    this.source.addEventListener("error", () => {
      // EventSource reconnects by itself; the `retry:` line sets the delay. All
      // this records is that the stream was not healthy, because a page that
      // looks alive while its stream is down is the failure this whole change
      // exists to remove.
      this.errors = (this.errors ?? 0) + 1;
    });
    return this;
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
    this.queue.push(record);
    if (this.draining) return;
    this.draining = true;
    try {
      while (this.queue.length > 0 && !this.stopped) {
        const next = this.queue.shift();
        if (next?.frame_index != null && this.lastFrameIndex != null
          && next.frame_index <= this.lastFrameIndex) {
          // A reconnect can hand back a frame already painted. Re-painting it
          // would put the picture backwards, which is worse than not painting
          // it, so it is counted and dropped.
          this.skipped += 1;
          continue;
        }
        await this.#handle(next);
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
    this.pushed += 1;
    if (record.frame_index != null) this.lastFrameIndex = record.frame_index;
    await this.onFrame(frame, record);
    await this.#maybeCrossCheck(record, frame);
  }

  // Every Nth frame, read the same accounts from the chain and compare. The
  // pushed frame has already been painted by this point: the cross-check is an
  // audit of the path, not a gate on the picture, so a slow chain read delays
  // the next frame's paint rather than holding one back.
  async #maybeCrossCheck(record, frame) {
    if (!this.readChain || this.crossCheckEvery <= 0) return;
    if (this.pushed % this.crossCheckEvery !== 0) return;
    this.crossChecks += 1;
    let comparison = {comparable: false, match: false, reason: "the chain read did not run"};
    try {
      const read = await this.readChain(record);
      comparison = compareFrames(frame, read);
    } catch (error) {
      comparison = {comparable: false, match: false, reason: `the chain read failed: ${error instanceof Error ? error.message : String(error)}`};
    }
    if (comparison.match) {
      this.crossCheckMatches = (this.crossCheckMatches ?? 0) + 1;
    } else if (comparison.comparable) {
      this.crossCheckMismatches += 1;
    } else {
      this.crossCheckUncomparable += 1;
    }
    this.onCrossCheck({frame, comparison, checked: this.crossChecks});
  }

  // Resolve once the queue has been drained. Painting is asynchronous (every
  // frame runs eight digest checks), so "the stream has taken this frame" and
  // "the frame is on the canvas" are different moments, and a caller that cares
  // about the second asks for this.
  async idle() {
    while (this.draining || this.queue.length > 0) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  }

  // What the panel shows. A mismatch is the number that matters: it is the one
  // way the push path can be wrong without the page looking wrong.
  summary() {
    return {
      pushed: this.pushed,
      torn: this.torn ?? 0,
      without_bytes: this.withoutBytes ?? 0,
      skipped: this.skipped,
      errors: this.errors ?? 0,
      last_frame_index: this.lastFrameIndex,
      cross_checks: this.crossChecks,
      cross_check_matches: this.crossCheckMatches ?? 0,
      cross_check_mismatches: this.crossCheckMismatches,
      cross_check_uncomparable: this.crossCheckUncomparable,
      last_error: this.lastError ?? null,
    };
  }

  stop() {
    this.stopped = true;
    this.queue = [];
    this.source?.close();
    this.source = null;
  }
}

export {StripError};
