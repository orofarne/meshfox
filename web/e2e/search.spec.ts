import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/search.canvas.md, in the default read-only mode
// (search is available whether or not the canvas is being edited — see
// App.tsx's `searchOpen`/`searchQuery` state). `branch-alpha`/`branch-beta`
// start folded in every test here (via `beforeEach`) so navigating between
// their two matching children exercises `revealAndFocus`'s fold/fold-back
// behavior, not just plain camera panning.

// Deliberately not a plain English word — see search.canvas.md's own doc
// comment for why (search scans every node's title/body, including
// whatever prose describes the fixture itself).
const QUERY = "zzzneedle";

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

function searchInput(page: Page) {
  return page.locator(".search-bar input");
}

/** Every string currently registered under `useSearchHighlight`'s shared
 * `mesh-search-match` CSS Custom Highlight — the actual matched text of
 * each `Range` in it, case preserved as found (not the lowercased query),
 * skipped entirely (empty array, not a throw) on a browser without the
 * API at all. */
function highlightedTexts(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const h = typeof CSS !== "undefined" && "highlights" in CSS ? CSS.highlights.get("mesh-search-match") : undefined;
    if (!h) return [];
    const out: string[] = [];
    for (const r of h) out.push(r.toString());
    return out;
  });
}

/** How many `Range`s are currently registered under `useSearchHighlight`'s
 * shared `mesh-search-match-current` — the *current* occurrence's own
 * highlight, painted over the plain one above (higher `.priority`) so it
 * reads as visually distinct from every other match, not just another
 * same-colored highlight. Should be exactly 1 whenever a search candidate
 * is focused, 0 otherwise (never more than 1 — at most one occurrence
 * anywhere in the app is ever "current" at a time). Returns `-1` on a
 * browser without the API at all (skipped entirely, not a throw), so a
 * test asserting an exact size never mistakes "unsupported" for "empty". */
function currentHighlightCount(page: Page): Promise<number> {
  return page.evaluate(() => {
    const h =
      typeof CSS !== "undefined" && "highlights" in CSS ? CSS.highlights.get("mesh-search-match-current") : undefined;
    return h ? h.size : -1;
  });
}

/** The current occurrence's own on-screen vertical position — used where
 * `QUERY` occurs identically several times in one node (so the matched
 * *text* alone can't tell two occurrences apart) to confirm stepping
 * actually moved which one is current, not just that the count stayed 1. */
function currentHighlightTop(page: Page): Promise<number | null> {
  return page.evaluate(() => {
    const h = typeof CSS !== "undefined" && "highlights" in CSS ? CSS.highlights.get("mesh-search-match-current") : undefined;
    if (!h || h.size === 0) return null;
    const [r] = [...h];
    return (r as Range).getBoundingClientRect().top;
  });
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await node(page, "branch-alpha").locator(".mesh-node-fold-toggle").click();
  await node(page, "branch-beta").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "leaf-alpha")).toHaveCount(0);
  await expect(node(page, "leaf-beta")).toHaveCount(0);
});

test("pressing / opens the search bar and focuses its input", async ({ page }) => {
  await expect(page.locator(".search-bar")).toHaveCount(0);

  await page.keyboard.press("/");

  await expect(page.locator(".search-bar")).toBeVisible();
  await expect(searchInput(page)).toBeFocused();
});

test("the toolbar button also opens and closes the search bar", async ({ page }) => {
  await page.getByRole("button", { name: "🔍 search" }).click();
  await expect(page.locator(".search-bar")).toBeVisible();

  await page.getByRole("button", { name: "🔍 search" }).click();
  await expect(page.locator(".search-bar")).toHaveCount(0);
});

test("typing a query shows a live match count, including zero matches", async ({ page }) => {
  await page.keyboard.press("/");

  await searchInput(page).fill(QUERY);
  await expect(page.locator(".search-bar-count")).toHaveText("1/2");

  await searchInput(page).fill("no-such-text");
  await expect(page.locator(".search-bar-count")).toHaveText("0/0");

  await searchInput(page).fill("");
  await expect(page.locator(".search-bar-count")).toHaveText("");
});

test("stepping to a match unfolds only the branch hiding it and highlights it", async ({ page }) => {
  await page.keyboard.press("/");
  await searchInput(page).fill(QUERY);

  // The very first Enter after typing advances from the pre-navigation
  // index 0 (`gotoSearchMatch`'s `(0 + 1 + matches.length) % matches.length`),
  // which lands on match index 1 — `leaf-beta`, the second match in
  // document order — not `leaf-alpha`. `branch-alpha` is left untouched.
  await searchInput(page).press("Enter");

  await expect(node(page, "branch-alpha").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "branch-beta").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "leaf-alpha")).toHaveCount(0);
  await expect(node(page, "leaf-beta")).toBeVisible();
  await expect(node(page, "leaf-beta").locator(".mesh-node")).toHaveAttribute("data-focused", "true");
});

test("moving to the next match folds the previous branch back and unfolds the new one", async ({ page }) => {
  await page.keyboard.press("/");
  await searchInput(page).fill(QUERY);
  await searchInput(page).press("Enter"); // -> leaf-beta (see the test above)

  await searchInput(page).press("Enter"); // -> leaf-alpha, wrapping around

  await expect(node(page, "branch-beta").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "branch-alpha").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "leaf-beta")).toHaveCount(0);
  await expect(node(page, "leaf-alpha")).toBeVisible();
  await expect(node(page, "leaf-alpha").locator(".mesh-node")).toHaveAttribute("data-focused", "true");
});

test("Shift+Enter steps to the previous match", async ({ page }) => {
  await page.keyboard.press("/");
  await searchInput(page).fill(QUERY);
  await searchInput(page).press("Enter"); // -> leaf-beta

  await searchInput(page).press("Shift+Enter"); // back to leaf-alpha

  await expect(node(page, "leaf-alpha")).toBeVisible();
  await expect(node(page, "leaf-alpha").locator(".mesh-node")).toHaveAttribute("data-focused", "true");
  await expect(node(page, "branch-beta").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
});

test("closing the search bar leaves the fold state exactly as the last match left it", async ({ page }) => {
  await page.keyboard.press("/");
  await searchInput(page).fill(QUERY);
  await searchInput(page).press("Enter"); // -> leaf-beta
  await searchInput(page).press("Enter"); // -> leaf-alpha

  await searchInput(page).press("Escape");

  await expect(page.locator(".search-bar")).toHaveCount(0);
  await expect(node(page, "branch-alpha").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "branch-beta").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "leaf-alpha")).toBeVisible();
  await expect(node(page, "leaf-beta")).toHaveCount(0);
});

test("stepping to a match unfolds the matched node itself, not just whatever ancestor was hiding it", async ({ page }) => {
  // `self-folded-node` is a direct child of root — no ancestor fold ever
  // hides it, only its own `data-folded`, which starts `true` here
  // because it's foldable (a non-title-only node) and this fixture
  // otherwise starts from "nothing folded" (see the fixture's own doc
  // comment) rather than the client's real folded-by-default state, so
  // it has to be folded by hand to reproduce that starting point.
  await node(page, "self-folded-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "self-folded-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "self-folded-node").locator(".mesh-node-body")).toHaveCount(0);

  await page.keyboard.press("/");
  await searchInput(page).fill("zzzowntext");
  await expect(page.locator(".search-bar-count")).toHaveText("1/1");
  await searchInput(page).press("Enter");

  await expect(node(page, "self-folded-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "self-folded-node").locator(".mesh-node-body")).toBeVisible();
  await expect(node(page, "self-folded-node").locator(".mesh-node")).toHaveAttribute("data-focused", "true");
});

test("a branch left open by the user before searching is never folded back by search", async ({ page }) => {
  // Unfold branch-beta by hand, *before* opening search — it's not in
  // `searchFoldSnapshotRef`'s "folded before this session" snapshot, so
  // stepping away from it must leave it open rather than folding it back.
  await node(page, "branch-beta").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "leaf-beta")).toBeVisible();

  await page.keyboard.press("/");
  await searchInput(page).fill(QUERY);
  await searchInput(page).press("Enter"); // -> leaf-beta (already open)
  await searchInput(page).press("Enter"); // -> leaf-alpha

  await expect(node(page, "branch-beta").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "leaf-beta")).toBeVisible();
  await expect(node(page, "leaf-alpha")).toBeVisible();
});

test("stepping to a match highlights it distinctly from other matches, and both clear once search closes", async ({
  page,
}) => {
  await page.keyboard.press("/");
  await searchInput(page).fill(QUERY);
  await searchInput(page).press("Enter"); // -> leaf-beta

  await expect.poll(() => highlightedTexts(page)).toEqual([QUERY]);
  // Exactly one occurrence anywhere is ever "current" — a second,
  // higher-priority `Highlight` (`mesh-search-match-current`) painted on
  // top of the plain one above, so the current candidate reads as
  // visually distinct rather than indistinguishable from every other
  // match search itself found.
  await expect.poll(() => currentHighlightCount(page)).toBe(1);

  // -> leaf-alpha: `branch-beta` folds back (no longer needed — see the
  // "moving to the next match" test above), unmounting `leaf-beta`, so its
  // highlight goes with it — `useSearchHighlight`'s cleanup removes
  // exactly the `Range`s a node itself added, not some other node's.
  await searchInput(page).press("Enter");

  await expect.poll(() => highlightedTexts(page)).toEqual([QUERY]);
  await expect.poll(() => currentHighlightCount(page)).toBe(1);

  await searchInput(page).press("Escape");

  await expect.poll(() => highlightedTexts(page)).toHaveLength(0);
  await expect.poll(() => currentHighlightCount(page)).toBe(0);
});

test("a match buried in a long body scrolls that body's own overflow into view", async ({ page }) => {
  const body = node(page, "long-body-node").locator(".mesh-node-body");
  // `long-body-node` starts folded (default: this fixture declares
  // `unfold`, so it's manually folded here first) so the match — genuinely
  // below the fold *and* off-screen within its own overflowed body once
  // unfolded — exercises both `revealAndFocus`'s unfold and
  // `useSearchHighlight`'s scroll-into-view in one step, the way a real
  // long node in a real canvas would. Folds `long-body-node` itself before
  // `long-branch` — the other order would hide `long-body-node` (and its
  // own fold toggle) the moment `long-branch` folds.
  await node(page, "long-body-node").locator(".mesh-node-fold-toggle").click();
  await node(page, "long-branch").locator(".mesh-node-fold-toggle").click();

  await page.keyboard.press("/");
  await searchInput(page).fill("zzzlongmatch");
  await expect(page.locator(".search-bar-count")).toHaveText("1/1");
  await searchInput(page).press("Enter");

  await expect(body).toBeVisible();
  await expect.poll(() => body.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  // `ensureRangeVisible` only acts once `waitForStableRect` sees the
  // target's own position hold steady for a few consecutive frames (see
  // its own doc comment) — polled here too, not a single synchronous
  // check, so this doesn't race that settle window the way a plain
  // `expect(...).toBe(true)` right after `press("Enter")` would.
  await expect
    .poll(() =>
      page.evaluate(() => {
        const h = CSS.highlights.get("mesh-search-match");
        const el = document.querySelector('.react-flow__node[data-id="long-body-node"] .mesh-node-body');
        if (!h || !el) return false;
        const bodyRect = el.getBoundingClientRect();
        for (const r of h) {
          const rect = (r as Range).getBoundingClientRect();
          if (rect.top >= bodyRect.top - 1 && rect.bottom <= bodyRect.bottom + 1) return true;
        }
        return false;
      }),
    )
    .toBe(true);
});

test("stepping between multiple occurrences within one node moves the target without touching fold state", async ({
  page,
}) => {
  // `multi-match-node` is never folded here — it starts unfolded (this
  // fixture declares `unfold`) and stays that way throughout; nothing
  // about this test should fold or unfold anything at all.
  await page.keyboard.press("/");
  await searchInput(page).fill("zzzmulti");
  await expect(page.locator(".search-bar-count")).toHaveText("1/3");

  // First Enter after typing advances from the pre-nav index 0 (same
  // `(0 + 1 + n) % n` quirk noted on the "stepping to a match" test
  // above), landing on occurrence index 1 (the counter's "2/3") — all
  // three occurrences are already visible/highlighted regardless of which
  // one is "current" (see the highlight test above), so what's actually
  // under test here is the *counter* — and the *current* highlight —
  // advancing per occurrence, not a newly-appearing plain highlight.
  await searchInput(page).press("Enter");
  await expect(page.locator(".search-bar-count")).toHaveText("2/3");
  await expect.poll(() => currentHighlightCount(page)).toBe(1);
  const topAt2 = await currentHighlightTop(page);

  await searchInput(page).press("Enter");
  await expect(page.locator(".search-bar-count")).toHaveText("3/3");
  await expect.poll(() => currentHighlightCount(page)).toBe(1);
  const topAt3 = await currentHighlightTop(page);
  // Same query text at every occurrence, so the only way to confirm the
  // *current* one actually moved (not just that the counter did) is that
  // its on-screen position changed between steps.
  expect(topAt3).not.toBeNull();
  expect(topAt3).not.toBe(topAt2);

  await searchInput(page).press("Enter");
  await expect(page.locator(".search-bar-count")).toHaveText("1/3");
  await expect.poll(() => currentHighlightCount(page)).toBe(1);

  await expect(node(page, "multi-match-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect.poll(() => highlightedTexts(page)).toHaveLength(3);
});

// The canvas-pan-for-an-uncapped-over-tall-node case (no internal scroll
// possible at all — a genuinely different case from `long-body-node`
// above) lives in its own search-pan.spec.ts/fixture/port instead of here:
// a node tall enough to actually exceed the browser viewport is also tall
// enough to wreck `clickFitViewAndWait`'s fit-to-everything zoom for every
// *other* test sharing this file's fixture/beforeEach — confirmed
// directly, it zoomed the rest of this canvas down to a sliver under the
// toolbar and broke every single test here, not just its own.

test("a match scrolled out horizontally in a wide run-output line still lands on-screen, not just vertically", async ({
  page,
}) => {
  // `wide-output-node` starts unfolded (this fixture declares `unfold`),
  // no folding needed here — this test is purely about the *horizontal*
  // half of `ensureRangeVisible`'s ancestor-scroll walk, which used to be
  // entirely missing: `getBoundingClientRect()` reports a range's
  // geometric position regardless of what clips it away, so without a
  // `scrollLeft` adjustment here the window-visibility check and
  // canvas-pan target both used an x coordinate that was never the
  // match's real on-screen position — confirmed directly against a real
  // canvas, this exact gap landed the camera on empty space.
  const pre = node(page, "wide-output-node").locator(".mesh-code-output pre");
  await expect.poll(() => pre.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
  await expect.poll(() => pre.evaluate((el) => el.scrollLeft)).toBe(0);

  await page.keyboard.press("/");
  await searchInput(page).fill("zzzwidematch");
  await expect(page.locator(".search-bar-count")).toHaveText("1/1");
  await searchInput(page).press("Enter");

  await expect.poll(() => pre.evaluate((el) => el.scrollLeft)).toBeGreaterThan(0);
  // The real assertion: whatever's actually painted at the current
  // occurrence's own reported center point is inside the *same* node the
  // range itself belongs to — not just "some coordinate within the
  // window", which a wrong x can trivially satisfy by accident (that's
  // exactly how this bug hid from a naive in-viewport check before).
  // Polled, not a single synchronous read — `ensureRangeVisible` only
  // acts once `waitForStableRect` sees a few consecutive stable frames
  // (see its own doc comment), same reasoning as the "long body" test
  // above.
  await expect
    .poll(() =>
      page.evaluate(() => {
        const h = CSS.highlights.get("mesh-search-match-current");
        if (!h || h.size === 0) return false;
        const [r] = [...h];
        const rect = (r as Range).getBoundingClientRect();
        const elAtPoint = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
        const rangeNode = r.startContainer.parentElement?.closest(".react-flow__node");
        const pointNode = elAtPoint?.closest(".react-flow__node");
        return !!rangeNode && rangeNode === pointNode;
      }),
    )
    .toBe(true);
});
