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
    latencies: s.pictureLatencies.slice(),
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
    }, {...FAST, query: "&cross-check-every=0"});
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
    }, {...FAST, query: "&cross-check-every=5"});
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
    }, {...FAST, query: "&cross-check-every=0"});
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
    }, {...FAST, query: "&cross-check-every=0"});
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
    }, {...FAST, query: "&cross-check-every=0"});
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
