import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold, viewportTransform } from "./helpers";

// Drives web/e2e/fixtures/scroll.canvas.md — five single-node documents,
// each sized via an explicit w=/h= so its overflow combination (none,
// vertical only, horizontal only, both, or overflow confined to an inner
// code block) is deterministic rather than depending on auto-layout's
// content-fit sizing.
//
// Two things get checked per node, matching how a real user actually
// interacts with a node's body: `.mesh-node-body`'s `onWheel` handler
// (`stopWheelIfScrollable` in MeshNode.tsx) decides, per wheel event,
// whether to let it scroll something inside the node or let it fall
// through to React Flow's `panOnScroll` canvas pan —
//   a) an axis that genuinely overflows actually scrolls when wheeled.
//   b) an axis that does *not* overflow at that exact cursor position
//      falls through and pans the canvas instead of being silently
//      swallowed (`hasOverflow` finding overflow on the *other* axis and
//      stopping propagation anyway would be exactly that bug).

function nodeBody(page: Page, nodeId: string): Locator {
  return page.locator(`.react-flow__node[data-id="${nodeId}"] .mesh-node-body`);
}

function nodeWrapper(page: Page, nodeId: string): Locator {
  return page.locator(`.react-flow__node[data-id="${nodeId}"]`);
}

function block(page: Page, nodeId: string, blockName: string): Locator {
  // Same DOM-id scheme as deps.spec.ts's `block()` — see web/src/deps.ts's
  // `blockDomId`.
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

function fileCodePreview(page: Page, nodeId: string): Locator {
  return page.locator(`.react-flow__node[data-id="${nodeId}"] .mesh-file-code-preview`);
}

async function overflowOf(el: Locator): Promise<{ v: boolean; h: boolean }> {
  return el.evaluate((e) => ({
    v: e.scrollHeight > e.clientHeight,
    h: e.scrollWidth > e.clientWidth,
  }));
}

async function scrollPos(el: Locator): Promise<{ top: number; left: number }> {
  return el.evaluate((e) => ({ top: e.scrollTop, left: e.scrollLeft }));
}

/** Polls the viewport's transform until two consecutive reads agree — the
 * app applies a pan instantly with no ongoing animation today, so this
 * typically resolves on the very first check, but it's the honest way to
 * wait for "panning has stopped" rather than assuming a fixed delay. */
async function waitForPanToSettle(page: Page) {
  let previous = await viewportTransform(page);
  await expect
    .poll(async () => {
      const current = await viewportTransform(page);
      const stable = current === previous;
      previous = current;
      return stable;
    })
    .toBe(true);
}

/** Wheels over `target` and asserts the canvas pans (its viewport transform
 * changes) — i.e. the wheel event fell through rather than being consumed
 * by something under the cursor that turned out not to actually scroll. */
async function expectWheelPansCanvas(page: Page, target: Locator, deltaX: number, deltaY: number) {
  await target.hover();
  const before = await viewportTransform(page);
  await page.mouse.wheel(deltaX, deltaY);
  await expect.poll(() => viewportTransform(page)).not.toBe(before);
}

/** Wheels over `target` and asserts it actually scrolls itself (rather than
 * the canvas panning) — `expect.poll` rather than an immediate read since a
 * dispatched wheel event's scroll effect isn't guaranteed to have applied
 * yet by the time `page.mouse.wheel` resolves. */
async function expectWheelScrolls(
  target: Locator,
  deltaX: number,
  deltaY: number,
): Promise<{ top: number; left: number }> {
  await target.hover();
  const before = await scrollPos(target);
  await target.page().mouse.wheel(deltaX, deltaY);
  await expect.poll(() => scrollPos(target)).not.toEqual(before);
  return scrollPos(target);
}

test.beforeEach(async ({ page }) => {
  // This suite is about scroll/pan physics inside a node's own body, not
  // fold — every node here needs to be visible/expanded regardless of
  // App.tsx's own fold-everything-but-root-by-default behavior.
  await disableDefaultFold(page, "root");
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  // The app opens centered on the root node at a fixed, readable zoom (not
  // fitted to content — see App.tsx's `INITIAL_ZOOM`), so with this
  // fixture's five sibling nodes spread well beyond the viewport, most of
  // them wouldn't otherwise be on screen at all. These tests are about
  // scroll/pan interaction physics, which assumes every fixture node is
  // simultaneously visible and at a known scale — fitting the view gets
  // back to that.
  await clickFitViewAndWait(page);
});

test("1. text fits the node — no scrollbars, wheeling over it pans the canvas", async ({ page }) => {
  const body = nodeBody(page, "fits-node");
  await expect(body).toContainText("Short text that fits");
  expect(await overflowOf(body)).toEqual({ v: false, h: false });

  await expectWheelPansCanvas(page, body, 0, 300);
});

test("2. text overflows by height — vertical scroll works, horizontal wheel falls through to pan", async ({
  page,
}) => {
  const body = nodeBody(page, "vscroll-node");
  expect(await overflowOf(body)).toEqual({ v: true, h: false });

  const before = await scrollPos(body);
  const after = await expectWheelScrolls(body, 0, 300);
  expect(after.top).toBeGreaterThan(before.top);
  expect(after.left).toBe(before.left);

  // Nothing to scroll horizontally here — a horizontal wheel shouldn't be
  // eaten by this element just because it happens to overflow vertically.
  await expectWheelPansCanvas(page, body, 300, 0);
});

test("3. text overflows by width — horizontal scroll works, vertical wheel falls through to pan", async ({
  page,
}) => {
  const body = nodeBody(page, "hscroll-node");
  expect(await overflowOf(body)).toEqual({ v: false, h: true });

  const before = await scrollPos(body);
  const after = await expectWheelScrolls(body, 300, 0);
  expect(after.left).toBeGreaterThan(before.left);
  expect(after.top).toBe(before.top);

  // Nothing to scroll vertically here — a vertical wheel shouldn't be eaten
  // by this element just because it happens to overflow horizontally.
  await expectWheelPansCanvas(page, body, 0, 300);
});

test("4. text overflows by both height and width — both scrolls work", async ({ page }) => {
  const body = nodeBody(page, "both-scroll-node");
  expect(await overflowOf(body)).toEqual({ v: true, h: true });

  const before = await scrollPos(body);
  const afterV = await expectWheelScrolls(body, 0, 300);
  expect(afterV.top).toBeGreaterThan(before.top);

  const afterH = await expectWheelScrolls(body, 300, 0);
  expect(afterH.left).toBeGreaterThan(afterV.left);
});

// Test 5 is deliberately split into independent tests (each its own fresh
// page load) rather than one test wheeling the same `pre` twice in a row.
// Firefox's wheel-input model ("wheel transactions", see Gecko's own docs
// on async wheel handling) hit-tests a scrollable target's async scroll
// frame once — either on the first wheel event over it, *or* pre-emptively
// during an unrelated reflow like the Controls panel's "fit view" (which
// `beforeEach` above clicks for every test in this file) — and then routes
// further wheel events there without re-hit-testing, and (per still-open
// Gecko nested-scrollframe reports, e.g. bugzilla.mozilla.org/1216488 and
// /1090028) without correctly chaining an unsuccessful local scroll
// attempt up to the ancestor pane either. Verified directly across many
// combinations (fresh vs. already-touched frame, `hover()` vs. raw
// `mouse.move`, pauses up to 3s, moving the mouse away and back): a wheel
// over `pre` that has nothing to scroll locally along its axis reliably
// gets silently absorbed in Firefox once `pre`'s scroll frame has been
// touched by anything — including just this file's own `beforeEach`.
// Chromium doesn't do any of this. 5a's horizontal wheel (something *does*
// scroll locally) is unaffected and passes on both browsers; only 5b's
// vertical, pass-through-expecting wheel is Firefox-skipped below.

test("5a. a too-wide code block scrolls horizontally on its own", async ({ page }) => {
  const body = nodeBody(page, "code-hscroll-node");
  await expect(body).toContainText("Short intro");
  // The node's own body has nothing to scroll — only the `pre` inside the
  // code block does.
  expect(await overflowOf(body)).toEqual({ v: false, h: false });

  const pre = block(page, "code-hscroll-node", "wide").locator("pre");
  expect(await overflowOf(pre)).toEqual({ v: false, h: true });

  const before = await scrollPos(pre);
  const after = await expectWheelScrolls(pre, 300, 0);
  expect(after.left).toBeGreaterThan(before.left);
});

test("5b. a vertical wheel over a too-wide code block falls through to pan the canvas", async ({
  page,
  browserName,
}) => {
  test.skip(browserName === "firefox", "Gecko nested-scrollframe wheel-chaining limitation — see comment above 5a.");

  const pre = block(page, "code-hscroll-node", "wide").locator("pre");
  // Horizontal overflow only: a vertical wheel has nothing to scroll here
  // and should fall through to pan the canvas instead of being silently
  // swallowed.
  expect(await overflowOf(pre)).toEqual({ v: false, h: true });
  await expectWheelPansCanvas(page, pre, 0, 300);
});

test("5c. a vertical wheel over a fitting node's plain text still falls through to pan the canvas", async ({
  page,
}) => {
  // Same node as 5a/5b, but hovered somewhere that isn't the code block at
  // all (the intro paragraph) — the body itself has no overflow in either
  // direction, so any wheel here should just pan. No nested `pre` involved,
  // so unlike 5b this isn't subject to the Firefox limitation above.
  const body = nodeBody(page, "code-hscroll-node");
  const intro = body.locator("p", { hasText: "Short intro" });
  await expectWheelPansCanvas(page, intro, 0, 300);
});

/**
 * Shared flow for tests 6-8: the mouse starts fixed in the blank gap above
 * `target` — between `aboveNodeId`'s node and `belowNodeId`'s (the fixture
 * always stacks these two directly on top of each other, so that gap is
 * guaranteed empty canvas, never any node) — and never moves again. A
 * *vertical* wheel scroll pans the canvas until `target` itself has slid up
 * under that fixed cursor (alignment is always vertical: the fixture only
 * ever stacks nodes vertically, so that's the only axis panning can bring
 * one to a fixed point on, regardless of which axis `target`'s own content
 * scrolls on — see the caller for why `target` can sit deeper than
 * `belowNodeId`'s own top, e.g. inside a code block further down the body).
 * Once settled, continued wheeling *along `axis`* should scroll `target`'s
 * own content first, and only resume panning the canvas (along that same
 * axis) once that content is exhausted — not vanish into a dead zone.
 */
async function testScrollHandoff(
  page: Page,
  aboveNodeId: string,
  belowNodeId: string,
  target: Locator,
  axis: "vertical" | "horizontal",
) {
  const aboveBox = await nodeWrapper(page, aboveNodeId).boundingBox();
  const belowBox = await nodeWrapper(page, belowNodeId).boundingBox();
  if (!aboveBox || !belowBox) throw new Error("missing node box");
  const mouseX = belowBox.x + belowBox.width / 2;
  const mouseY = (aboveBox.y + aboveBox.height + belowBox.y) / 2;
  expect(mouseY).toBeGreaterThan(aboveBox.y + aboveBox.height);
  expect(mouseY).toBeLessThan(belowBox.y);

  // The mouse never moves from here on — only the canvas underneath it does.
  await page.mouse.move(mouseX, mouseY);

  const mouseOverTarget = async () => {
    const box = await target.boundingBox();
    return !!box && mouseY >= box.y && mouseY <= box.y + box.height;
  };
  for (let i = 0; i < 60 && !(await mouseOverTarget()); i++) {
    await page.mouse.wheel(0, 40);
  }
  expect(await mouseOverTarget()).toBe(true);

  // Let the pan settle before continuing.
  await waitForPanToSettle(page);
  const settledTransform = await viewportTransform(page);

  // Continue scrolling in place along `axis`: `target`'s own content should
  // now capture it — scrolling — without the canvas moving at all. Each
  // tick waits for its own effect to actually land before the next one is
  // sent (a dispatched wheel event's scroll isn't guaranteed to have
  // applied yet the instant `page.mouse.wheel` resolves — see
  // `expectWheelScrolls` above) so the loop's own view of the scroll
  // position never reads stale and overshoots into the next phase early.
  // A bigger step than the alignment phase's — the fixture's horizontal
  // overflow (a long unbroken string) is thousands of pixels wide, well
  // beyond what 30 ticks of the alignment phase's small step would cover.
  const step = 200;
  const deltaX = axis === "horizontal" ? step : 0;
  const deltaY = axis === "vertical" ? step : 0;
  const posAlongAxis = async () => {
    const pos = await scrollPos(target);
    return axis === "horizontal" ? pos.left : pos.top;
  };
  const maxScroll = await target.evaluate((e, ax) => (ax === "horizontal" ? e.scrollWidth - e.clientWidth : e.scrollHeight - e.clientHeight), axis);
  for (let i = 0; i < 30; i++) {
    const pos = await posAlongAxis();
    if (pos >= maxScroll - 1) break;
    await page.mouse.wheel(deltaX, deltaY);
    await expect.poll(posAlongAxis).not.toBe(pos);
  }
  expect(await posAlongAxis()).toBeGreaterThanOrEqual(maxScroll - 1);
  // The canvas shouldn't have moved a pixel while the target's own content
  // was still absorbing the scroll.
  expect(await viewportTransform(page)).toBe(settledTransform);

  // The target's content is now maxed out along `axis` — further scrolling
  // there has nowhere left to go locally, so it should fall through and
  // resume panning the canvas rather than vanishing into a dead zone.
  await page.mouse.wheel(deltaX, deltaY);
  await expect.poll(() => viewportTransform(page)).not.toBe(settledTransform);
}

test("6. scrolling the canvas onto a node hands the wheel to its content, then back to the canvas once exhausted (vertical)", async ({
  page,
}) => {
  await testScrollHandoff(page, "fits-node", "vscroll-node", nodeBody(page, "vscroll-node"), "vertical");
});

/**
 * A user scrolling down through a canvas keeps spinning the same (vertical)
 * mouse wheel the whole time — they don't switch gestures just because the
 * cursor happens to pass over something that only scrolls sideways. Same
 * setup as `testScrollHandoff` (mouse fixed above `target`, vertical wheel
 * pans it under the cursor), but `target` here has *no* vertical overflow
 * at all, so there's nothing to hand the wheel off to: every continued
 * vertical tick should keep panning the canvas straight through it, never
 * freezing on it, and `target`'s own scroll position should never move.
 *
 * Every wheel tick here carries a couple of stray horizontal units
 * (`deltaX: 2`) alongside the real vertical intent (`deltaY: 40`) — a real
 * mouse or trackpad's "vertical" scroll is essentially never a
 * mathematically pure `deltaX: 0`. `deltaX: 0` exclusively would've missed
 * the actual bug this test exists to catch: `canScrollAlong` checking both
 * axes independently meant that sliver of horizontal noise alone was
 * enough for `target` (which does have horizontal room) to claim the
 * whole event, silently swallowing the vertical pan the user was actually
 * doing — the canvas would visibly freeze over `target` while it crept
 * sideways by a couple of imperceptible pixels per tick.
 *
 * `tickCount` (default 5): how many *further* wheel ticks to send once
 * settled over `target`, on top of the one landing tick already sent
 * during alignment. Callers passing a scrollable element nested inside
 * another (e.g. a code fence's `pre`, itself inside `.mesh-node-body`)
 * should pass 1 — see the call site for why more than that isn't
 * cross-browser-reliable to assert.
 */
async function testVerticalScrollPassesThroughHorizontalContent(
  page: Page,
  aboveNodeId: string,
  belowNodeId: string,
  target: Locator,
  tickCount = 5,
) {
  const aboveBox = await nodeWrapper(page, aboveNodeId).boundingBox();
  const belowBox = await nodeWrapper(page, belowNodeId).boundingBox();
  if (!aboveBox || !belowBox) throw new Error("missing node box");
  const mouseX = belowBox.x + belowBox.width / 2;
  const mouseY = (aboveBox.y + aboveBox.height + belowBox.y) / 2;
  expect(mouseY).toBeGreaterThan(aboveBox.y + aboveBox.height);
  expect(mouseY).toBeLessThan(belowBox.y);

  // The mouse never moves from here on — only the canvas underneath it does.
  await page.mouse.move(mouseX, mouseY);

  const mouseOverTarget = async () => {
    const box = await target.boundingBox();
    return !!box && mouseY >= box.y && mouseY <= box.y + box.height;
  };
  for (let i = 0; i < 60 && !(await mouseOverTarget()); i++) {
    await page.mouse.wheel(2, 40);
  }
  expect(await mouseOverTarget()).toBe(true);

  // Let the pan settle, right with the cursor sitting over `target`.
  await waitForPanToSettle(page);

  const initialTargetScroll = await scrollPos(target);
  // Keep scrolling vertically (plus the same horizontal noise), exactly the
  // same gesture as before landing — each tick should keep panning (no
  // stall/dead zone on `target`, even though it's genuinely scrollable,
  // just not on the axis this gesture actually means).
  for (let i = 0; i < tickCount; i++) {
    const before = await viewportTransform(page);
    await page.mouse.wheel(2, 40);
    await expect.poll(() => viewportTransform(page)).not.toBe(before);
  }
  // And this really was the canvas panning underneath a stationary cursor —
  // target's own scroll position never budged.
  expect(await scrollPos(target)).toEqual(initialTargetScroll);
}

test("7. a vertical scroll gesture passes straight through a horizontal-only-scroll node, never freezing on it", async ({
  page,
}) => {
  await testVerticalScrollPassesThroughHorizontalContent(
    page,
    "vscroll-node",
    "hscroll-node",
    nodeBody(page, "hscroll-node"),
  );
});

test("8. a vertical scroll gesture passes straight through a horizontal-only-scroll code block, never freezing on it", async ({
  page,
}) => {
  const pre = block(page, "code-hscroll-node", "wide").locator("pre");
  // This used to be pinned to a single post-landing tick (`tickCount: 1`)
  // on the theory that it was hitting the same Gecko "wheel transaction"
  // routing behavior as 5b above — but direct event instrumentation
  // (logging every wheel event's real `target` plus a wrapped
  // `stopPropagation`) showed the actual failure was a genuine app bug,
  // reproducible on *any* browser once the wheel event's target happens to
  // land on the `<code>` text itself rather than `<pre>`: `canScrollAlong`
  // (MeshNode.tsx) compared raw `scrollHeight`/`clientHeight` without
  // first checking the element's own `overflow` CSS actually enables
  // scrolling. An inline element like `<code>` always reports
  // `clientHeight === 0` (inline boxes have no client box) while
  // `scrollHeight` still measures real content extent, so that comparison
  // finds spurious "room to scroll" on any inline element with enough
  // text — even one with `overflow-y: visible` — and swallows the wheel
  // event instead of letting it pass through to `pre` (the real scroll
  // container, correctly reporting no vertical room) or the canvas pan
  // beneath it. Fixed by requiring `overflow-y`/`overflow-x` to actually
  // be `auto`/`scroll` before considering an element a scroll candidate at
  // all. Restored to the same sustained-tick count as test 7 now that the
  // real cause is fixed, rather than working around it.
  await testVerticalScrollPassesThroughHorizontalContent(page, "both-scroll-node", "code-hscroll-node", pre);
});

// Tests 9-10: a `file` node's `display="code"` preview (`FileCodePreview` in
// MeshNode.tsx) fetches its target's content asynchronously, so the
// wheel-listened `.mesh-file-code-preview` div doesn't exist yet on this
// component's very first render (it shows "loading preview…" instead) — it
// only mounts once the fetch resolves. `useStopWheelIfScrollable` used to
// attach its native `wheel` listener from inside a plain `useRef` read by a
// `useEffect(..., [])`, which only ever runs once, right after that first
// render — so for this specific caller it always found `ref.current` still
// `null` and permanently attached nothing, no matter how long afterward the
// div actually showed up. Every wheel over a file preview's code therefore
// always fell straight through to pan the canvas, however much the preview
// itself had to scroll. Fixed by switching to a callback ref, which reruns
// on every mount of the actual underlying DOM node rather than only once at
// the owning component's first commit.

test("9. a file node's code preview scrolls vertically like any other overflowing body", async ({ page }) => {
  const preview = fileCodePreview(page, "file-vscroll-node");
  await expect(preview).toContainText("line 01 of a long file");
  expect(await overflowOf(preview)).toEqual({ v: true, h: false });

  const before = await scrollPos(preview);
  const after = await expectWheelScrolls(preview, 0, 300);
  expect(after.top).toBeGreaterThan(before.top);
});

test("10. scrolling onto a file node's code preview hands the wheel to its content, then back to the canvas once exhausted (vertical)", async ({
  page,
}) => {
  await testScrollHandoff(page, "code-hscroll-node", "file-vscroll-node", fileCodePreview(page, "file-vscroll-node"), "vertical");
});
