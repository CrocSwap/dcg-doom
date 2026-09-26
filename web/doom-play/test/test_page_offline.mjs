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
}
