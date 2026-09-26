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
    leadTics: s.leadTics,
    aimedAt: s.aimedAt,
    inputsSent: s.inputsSent,
    batchesSigned: s.batchesSigned,
    refusals: s.refusals,
    roundTripMs: s.roundTripMs,
    // The two keypress meters. stampLatencies is keydown to the first painted
    // frame at or past the tic the input was stamped for, and is the latency a
    // player feels; nextPictureLatencies is keydown to the next frame of any
    // kind, which is not a keypress latency.
    stampLatencies: s.stampLatencies.slice(),
    stampSamples: (s.stampSamples ?? []).slice(),
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
      // And the status poll the join owes has run: the producer's position is
      // stamped, not merely present.
      const readAt = await page.evaluate("window.doomPlay.state.producerReadAt");
      assert.ok(readAt > 0, "the join-time status poll never ran");
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

  test("the page aims at the producer's published lead, and that lead is most of the latency", {timeout: 180_000}, async () => {
    // The producer resolves a lead of its own from how long a staged tic takes to
    // reach the chain -- 45-48 tics on R15, which is 1.77 s -- and this page
    // takes the larger of that and its own hop. On the 2026-09-26 baseline that
    // put the stamped tic 90 tics past the chain's cursor, and the measured
    // keypress-to-picture median was 4,270 ms against 90/22.2 tic/s = 4.05 s of
    // arithmetic: the page pays the delivery lead twice, once inside the
    // producer's position and once in its own aim.
    //
    // The obvious fix -- aim the page's own hop alone -- was measured twice on
    // R15 and broke both sessions: input refusals went from 1 in a 110 s session
    // to 100 and then 176, no command tics were deposited, the producer stopped,
    // the chain's cursor stuck at 45,890 and `tic-sender-result.json` was never
    // written. The chain was fine -- the meta read afterwards at `M_HALT 0`,
    // `M_CURSOR 45890`, `M_LAST_SWEEP_TICS 4` -- so that is a client-side
    // producer hang and it is not diagnosed. A 16-tic aim is inside the race the
    // 2026-09-26 input round measured, and mostly-zeros becomes a refusal flood
    // rather than a cheaper keypress. The comment on `aimLead` carries the
    // numbers; this test is here so the behaviour is pinned and the next attempt
    // starts from the measurement rather than from the idea.
    //
    // What it asserts is the two halves: the page reads the published lead, and
    // its aim is the producer's next tic plus that lead. It fails on a page that
    // ignores the published lead.
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const producerNext = await page.evaluate("window.doomPlay.state.producerNextCursor");
      await page.key("keyDown", "ArrowRight", "ArrowRight", 39);
      // Long enough for the aim to be consumed: the fake marks a staged tic
      // landed once the chain cursor is a whole lead past the producer's
      // generated cursor, so a 61-tic lead at 35 tic/s is 1.7 s before the first
      // tic of a keypress is read. That is the price of the aim, and it is why
      // this hold is longer than the other tests'.
      await sleep(3_500);
      const held = await snapshot(page);
      await page.key("keyUp", "ArrowRight", "ArrowRight", 39);
      const batch = relayer.batches[0] ?? null;
      assert.ok(batch, "the page staged no batch");
      const published = await page.evaluate("window.doomPlay.state.producerLeadTics");
      assert.equal(published, relayer.leadTics, `the page never read the producer's lead (${JSON.stringify(published)})`);
      assert.ok(relayer.leadTics > 16, `this relayer published ${relayer.leadTics}, which is not above the 16 the page used to aim`);
      assert.equal(
        held.leadTics,
        relayer.leadTics,
        `the page aimed ${JSON.stringify(held.leadTics)} tics ahead of the producer's next tic ` +
          `where the producer published ${relayer.leadTics}`,
      );
      // And the aim really is the producer's next tic plus the published lead.
      // `producerNext` was read before the keydown and the fake's producer
      // advances 2 tics every 60 ms, so the measured hop also carries the page's
      // push cadence.
      const hop = batch.firstSeq - producerNext;
      assert.ok(
        hop >= relayer.leadTics && hop <= relayer.leadTics + 40,
        `the batch started ${hop} tics past the producer's next tic, where the published lead was ${relayer.leadTics}`,
      );
      // And the aim is inside the relayer's own window, so the lead did not turn
      // a deposit race into a batch_refused.
      assert.equal(held.refusals, 0, "the aim was refused");
      assert.ok(relayer.acceptedBatches > 0, "the relayer accepted nothing");
      assert.ok(relayer.landedTics > 0, `the engine consumed none of the ${relayer.stagedTics} tics staged in 3.5 s`);
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("a relayer that publishes no lead falls back to the page's own arithmetic", {timeout: 180_000}, async () => {
    // A page that can only aim with a published lead is a page that cannot play
    // against a relayer that has not published one, which is every mock relayer
    // and every session whose producer has not resolved a lead yet.
    await withPage(async ({page, relayer}) => {
      await sleep(2_000);
      const published = await page.evaluate("window.doomPlay.state.producerLeadTics");
      assert.equal(published, null, `this relayer published ${JSON.stringify(published)} and was meant to publish nothing`);
      await page.key("keyDown", "ArrowRight", "ArrowRight", 39);
      await sleep(1_200);
      const held = await snapshot(page);
      await page.key("keyUp", "ArrowRight", "ArrowRight", 39);
      assert.ok(held.batchesSigned > 0, "the page signed nothing");
      assert.equal(typeof held.leadTics, "number", `a batch was signed without an aim: ${JSON.stringify(held)}`);
      assert.ok(
        held.leadTics >= 16,
        `with no published lead the page aimed ${JSON.stringify(held.leadTics)} tics, under the floor of 16`,
      );
    }, {relayerOptions: {...FAST, publishLead: false}, query: "&cross-check-every=0"});
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
    // The two numbers, and the fault this test exists for.
    //
    // The page measured keydown -> the next frame past the gametic that was on
    // screen, and called it input-to-picture. On R15 that read 408 ms on a
    // session whose lead was 2,155 ms, because the frame it timed was the next
    // picture to arrive and had nothing to do with the key. The honest meter is
    // keydown -> the first frame at or past the tic the relayer stamped this
    // hold's input for, which is the frame the key is actually in.
    //
    // The fake relayer publishes at 60 ms and advances 2 tics a frame at 35
    // tic/s, and publishes a lead of 61 tics. So the next-picture meter has to
    // read tens of milliseconds and the keypress meter has to read about the
    // lead -- 61 tics is 1.74 s at this rate. A page that reports the same
    // number for both is timing the first one twice.
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
      assert.equal(released.holdsOpen, 1, "the press stopped being measured at the keyup");
      // Long enough for the stamped tic to be reached: the lead plus the frame
      // cadence, which is the price of a 61-tic lead at 35 tic/s.
      await sleep(6_000);
      const after = await snapshot(page);

      assert.equal(after.holdsStarted, before.holdsStarted + 1, "the press was not counted");
      assert.equal(after.holdsUnstamped, 0, `a press produced no stamped answer: ${JSON.stringify(after)}`);
      assert.equal(after.holdsOpen, 0, "the press is still open after its picture arrived");
      const stamps = after.stampSamples.slice(before.stampSamples.length);
      const nexts = after.nextPictureLatencies.slice(before.nextPictureLatencies.length);
      assert.equal(stamps.length, 1, `the keypress meter recorded ${JSON.stringify(stamps)} samples for one press`);
      assert.ok(nexts.length >= 1, `the next-picture meter recorded nothing: ${JSON.stringify(after)}`);

      // The definitional check: the frame that ended the wait was at or past the
      // tic the relayer stamped, so the keypress is in that picture.
      assert.ok(after.lastStampSample !== null, "no sample recorded the pair it waited on");
      assert.ok(
        after.lastStampSample.gametic >= after.lastStampSample.tic,
        `the wait ended on gametic ${after.lastStampSample.gametic} against a stamped tic of ${after.lastStampSample.tic}`,
      );
      // And the two meters are not the same quantity. The next picture is one
      // frame away; the keypress is a whole lead away.
      const stamp = stamps[0].ms;
      const next = nexts[0];
      const leadMs = (relayer.leadTics / relayer.ticsPerSecond) * 1000;
      assert.ok(
        stamp >= 0.5 * leadMs,
        `the keypress meter read ${stamp.toFixed(0)} ms against a lead of ${leadMs.toFixed(0)} ms ` +
          `(stamped tic ${after.lastStampedTic}), so it timed a frame rather than the keypress`,
      );
      assert.ok(
        next < stamp,
        `the next-picture meter (${next.toFixed(0)} ms) is not below the keypress meter (${stamp.toFixed(0)} ms), ` +
          "so one of them is timing the wrong frame",
      );
      assert.ok(after.painted > before.painted, "the page painted nothing during the press");
      assert.equal(after.holdsOpen, 0, "the press never closed");
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
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
          `lead ${JSON.stringify(after.leadTics)}, refused ${after.refusals}, error ${JSON.stringify(after.error)})`,
      );
      assert.equal(typeof after.leadTics, "number", `a batch was signed without an aim: ${JSON.stringify(after)}`);
      assert.ok(after.roundTripMs > 0, "the page never saw a completed POST");
      assert.ok(relayer.acceptedBatches > 0, "the relayer accepted nothing");
      assert.equal(relayer.badSignatures, 0, "the relayer refused a signature the burner made");
      assert.equal(relayer.refusals.length, 0, `the relayer refused ${JSON.stringify(relayer.refusals.slice(0, 2))}`);
      assert.ok(relayer.landedTics > 0, `the engine consumed none of the ${relayer.stagedTics} staged tics`);
      assert.ok(held.keys.includes("right"), "the page does not think right is held");
      assert.equal(after.error, null, `the page raised: ${after.error}`);
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
  });

  test("a keypress shows up in the picture, and the four directions differ", {timeout: 300_000}, async () => {
    // Each key has to change the ticcmd the page signs, and the four directions
    // have to differ from one another. This reads what the relayer verified, not
    // what the page believes about itself.
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
        // The first batch staged *while the key was down*. The batch after the
        // release carries no keys, which is correct and would say nothing.
        seen[name] = relayer.batches[0] ?? null;
        await page.key("keyUp", code, keyName, vk);
        await sleep(300);
      }
      return seen;
    }, {relayerOptions: {...FAST}, query: "&cross-check-every=0"});
    for (const [name, batch] of Object.entries(directions)) {
      assert.ok(batch, `pressing ${name} staged no batch: ${JSON.stringify(directions)}`);
    }
    // 50 forward, 50 back, and the two turn steps: 4 is counter-clockwise
    // (left) and 252 is clockwise (right). The bytes are the engine's, so these
    // are the numbers a Doom ticcmd is made of rather than the page's names.
    const d = (name) => JSON.stringify(directions[name]);
    assert.equal(directions.forward.forward, 50, `forward is not 50 forward: ${d("forward")}`);
    assert.deepEqual(directions.forward.turns, [], `forward turned the player: ${d("forward")}`);
    assert.equal(directions.back.forward, 206, `back is not 50 back (50 is -50 as a byte): ${d("back")}`);
    assert.deepEqual(directions.back.turns, [], `back turned the player: ${d("back")}`);
    assert.deepEqual(directions.left.turns, [4], `left is not one step counter-clockwise: ${d("left")}`);
    assert.equal(directions.left.forward, 0, `left also moved the player: ${d("left")}`);
    assert.deepEqual(directions.right.turns, [252], `right is not one step clockwise: ${d("right")}`);
    assert.equal(directions.right.forward, 0, `right also moved the player: ${d("right")}`);
  });
}
