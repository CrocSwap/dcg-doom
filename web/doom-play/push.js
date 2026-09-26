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

import {decodeFrame, STRIP_COUNT, STRIP_HEADER_BYTES} from "./strip.js";

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
  const {slot, payloads} = await reader.getAccounts(addresses, {dataSlice: {offset: 0, length: STRIP_HEADER_BYTES}});
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
// a picture that is a frame old is still the right picture and a picture that
// is a frame out of order is a lie. Frames are therefore never dropped here --
// if the page is too slow to keep up, the queue grows and the page says so, and
// it is the operator who has to decide that a slow viewer wants skipping.
export class FrameStream {
  constructor({base, sessionId, onFrame, onCrossCheck, crossCheck, crossCheckEvery = DEFAULT_CROSS_CHECK_EVERY, EventSourceImpl = null, readChain = null, retryDelayMs = 500} = {}) {
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
    // Wait this long before reading the chain a second time after a
    // disagreement. Long enough for a re-render to be announced, short enough
    // that a check does not hold the stream up.
    this.retryDelayMs = retryDelayMs;
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
    await this.#maybeCrossCheck(record, frame);
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

  // Resolve once the queue has been drained. Painting is asynchronous (every
  // frame runs eight digest checks, and a cross-check reads the chain), so "the
  // stream has taken this frame" and "the frame is on the canvas" are different
  // moments, and a caller that cares about the second asks for this.
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
      // Disagreements that survived a second read. This is the only bucket that
      // means the push path is wrong.
      cross_check_retries: this.crossCheckRetries,
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
    this.queue = [];
    this.source?.close();
    this.source = null;
  }
}

