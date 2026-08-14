//! Real-browser layout checks for the shipped `site-template/`, using
//! Playwright (already a project dependency — `web/`'s own e2e tests use
//! it) to actually render the generated HTML/CSS/JS and read back real
//! computed layout. This exists because several things reported against
//! this template turned out to be CSS layout questions that no amount of
//! HTML-structure parsing (see `site_template.rs`, which uses `scraper`)
//! can verify — flexbox behavior (in particular `align-items: center`'s
//! role in both the branch-right geometry and the parent/children-group
//! centering below) only exists once a real layout engine has run.
//!
//! The template's own layout strategy (see `_macros.html.tera`'s and
//! `style.css`'s doc comments) is now pure CSS end to end — no JS
//! measures or repositions anything (an earlier version of this template
//! did, and needed a whole round of fixes for scroll/centering bugs that
//! approach kept introducing; see git history for that chapter). Only
//! `meshfox:edge` cross-references, which can point anywhere in the tree
//! and so need a real measured position, are still drawn by a small JS
//! pass. What matters here:
//!   - root and its direct children (depth <=1) share one column with
//!     only a small nudge right (mirrors `web/src/autolayout.ts`'s
//!     `ROOT_CHILD_INDENT`), while every deeper node branches right of its
//!     own real parent in an ordinary flex row (mirrors that same file's
//!     `H_GAP`/`placeRightward`) — neither is ever repositioned by script,
//!     `position` stays `static` throughout.
//!   - a depth-1 card with a tall branch of its own legitimately pushes
//!     the next depth-1 sibling below that branch (plain flex-column
//!     flow) — but never *further* than that: no extra dead space beyond
//!     a normal sibling gap, and never an overlap with the branch itself.
//!   - two *different* depth-1 parents' own branches can never collide,
//!     structurally: they're just sequential items in the same flex
//!     column, not overlapping regions — no equivalent of the old
//!     `branchCursorY` guard is needed anymore.
//!   - the two-tier card width (depth <=1 wide, depth >=2 narrower) must
//!     actually be reached, not squeezed down.
//!   - a depth >=2 children group is centered against its own (immovable)
//!     parent's vertical middle — for free, via `.node-row`'s own
//!     `align-items: center` (see `style.css`), not a computed offset.
//!   - a real gap must separate a node's own card from its first child.
//!   - structural connectors are pure CSS ("twig and spine" pseudo-
//!     elements — see `style.css`): root's own card gets no right-side
//!     connector nub (its line to depth-1 runs down its *left* side
//!     instead, matching `web/src/MeshNode.tsx`'s root-only Left-source
//!     handle), while any other card with children does get one (right of
//!     itself, left of the child) — and the JS overlay carries no
//!     structural edges at all anymore, only `meshfox:edge` ones.
//!   - the `meshfox:edge` overlay is drawn in `.tree`-relative coordinates
//!     (each node's own `offsetLeft`/`offsetTop`), not viewport/scroll-
//!     relative ones, so it stays glued to the nodes it connects under
//!     `.canvas-wrap`'s own horizontal/vertical scroll.
//!
//! Best-effort: skips (doesn't fail the suite) if `web/node_modules`
//! hasn't been `npm install`ed locally/in CI — real browser automation is
//! useful here, but shouldn't become a hard requirement just to run
//! `cargo test` somewhere that hasn't set up the web/ toolchain.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-site-template-layout-test-{tag}-{nanos}-{n}"))
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `web/node_modules/playwright-core`'s entry point — present once `cd web
/// && npm install` has run. Returns `None` (meaning "skip this test") when
/// it hasn't.
fn playwright_core_entry() -> Option<PathBuf> {
    let entry = repo_root().join("web/node_modules/playwright-core/index.mjs");
    entry.is_file().then_some(entry)
}

/// Runs a static build of `canvas_md` through `meshfox static`, then a
/// Playwright `page.evaluate` body (`eval_js`, a JS expression producing an
/// object) against the result, returning that object's entries as
/// `KEY=value` string pairs (kept deliberately dumb — no JSON parsing
/// dependency needed in this crate just for a few tests).
fn build_and_inspect(tag: &str, canvas_md: &str, eval_js: &str) -> std::collections::HashMap<String, String> {
    let playwright_core = playwright_core_entry().expect("caller must check playwright_core_entry() first");

    let canvas_path = unique_dir(&format!("canvas-{tag}")).join("doc.canvas.md");
    write_file(&canvas_path, canvas_md);
    let out_dir = unique_dir(&format!("out-{tag}"));

    let status = Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(repo_root().join("site-template"))
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let script_path = unique_dir(&format!("script-{tag}")).join("inspect.mjs");
    let html_url = format!("file://{}", out_dir.join("index.html").display());
    let script = format!(
        r#"
import {{ chromium }} from {playwright_core:?};

const browser = await chromium.launch();
const page = await browser.newPage({{ viewport: {{ width: 1400, height: 900 }} }});
await page.goto({html_url:?});
// The template's own layout/edge-drawing script runs once synchronously
// on page load, before the `load` event even fires — no need to wait for
// anything beyond navigation itself.

const out = await page.evaluate(() => {{
  return {eval_js};
}});

for (const [key, value] of Object.entries(out)) {{
  console.log(key + "=" + value);
}}
await browser.close();
"#,
    );
    write_file(&script_path, &script);
    let output = Command::new("node").arg(&script_path).output().expect("failed to run node");
    let _ = std::fs::remove_dir_all(script_path.parent().unwrap());
    assert!(
        output.status.success(),
        "playwright inspection script failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let kv = parse_kv(&String::from_utf8_lossy(&output.stdout));

    let _ = std::fs::remove_dir_all(&out_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    kv
}

fn parse_kv(output: &str) -> std::collections::HashMap<String, String> {
    output.lines().filter_map(|line| line.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A real bug this guards against: README.md's "Architecture" section has
/// a short card but a tall branch of its own (several children). Since
/// depth-1 children now branch right of their own parent (a flex row), a
/// tall branch of "Short"'s own legitimately makes "Short"'s whole *row*
/// tall — so "Next" (root's next child, stacked below in flex-column flow)
/// starts after that whole row, not just after "Short"'s own card. That's
/// expected now (it mirrors how the real interactive UI's own vertical
/// space works too) — what must still hold is that "Next" never overlaps
/// "Short"'s own branch, and the gap after the branch is a normal sibling
/// gap, not a further, unexplained void on top of it.
const GAP_FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    // Every node in this fixture is folded shut by default (see
    // `crates/core/src/staticgen.rs`'s `resolve_default_fold`) — this
    // test is about the *expanded* layout, so it opts the whole document
    // out via the `unfold` option, same as a reader clicking every node
    // open would get.
    "<!-- meshfox:option name=\"unfold\" -->\n",
    "\n",
    "## Short\n",
    "<!-- meshfox:node id=\"short\" -->\n",
    "\n",
    "A short card with a tall branch of its own.\n",
    "\n",
    "### Child A\n",
    "<!-- meshfox:node id=\"child-a\" -->\n",
    "\n",
    "Enough of its own text that this card has real height — not a one-liner.\n",
    "\n",
    "### Child B\n",
    "<!-- meshfox:node id=\"child-b\" -->\n",
    "\n",
    "Same idea — a second sibling with real height, so \"Short\"'s own branch is tall overall.\n",
    "\n",
    "## Next\n",
    "<!-- meshfox:node id=\"next\" -->\n",
    "\n",
    "The next top-level section — should start right after \"Short\"'s own *branch*, not overlap it, and not leave any further gap beyond a normal one.\n",
);

#[test]
fn a_short_card_with_a_tall_branch_pushes_the_next_sibling_below_the_branch_with_no_extra_gap() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    let kv = build_and_inspect(
        "gap",
        GAP_FIXTURE_CANVAS,
        r#"{
          shortHeight: document.getElementById("node-short").getBoundingClientRect().height,
          nextTop: document.getElementById("node-next").getBoundingClientRect().top,
          childATop: document.getElementById("node-child-a").getBoundingClientRect().top,
          childBBottom: document.getElementById("node-child-b").getBoundingClientRect().bottom,
        }"#,
    );
    let get = |k: &str| -> f64 { kv.get(k).unwrap_or_else(|| panic!("missing {k}")).parse().unwrap() };

    let short_height = get("shortHeight");
    let next_top = get("nextTop");
    let child_b_bottom = get("childBBottom");
    let branch_height = child_b_bottom - get("childATop");

    // Sanity: the branch really is taller than the card, i.e. this test
    // fixture actually reproduces the shape the bug needs.
    assert!(branch_height > short_height + 40.0, "fixture didn't reproduce a short-card/tall-branch shape: card_height={short_height} branch_height={branch_height}");

    // "Next" must never overlap "Short"'s own branch...
    assert!(next_top >= child_b_bottom - 0.5, "\"Next\" must not overlap \"Short\"'s own branch: next_top={next_top} child_b_bottom={child_b_bottom}");
    // ...and the gap after the branch ends should be a normal sibling gap,
    // not a further, unexplained void stacked on top of it.
    let gap = next_top - child_b_bottom;
    assert!(gap < 40.0, "gap between \"Short\"'s branch and \"Next\" should be a normal sibling gap: {gap}px");
}

/// Two depth-1 parents in a row — "Parent One" has a tall branch (three
/// children), "Parent Two" (immediately after it) has a shorter one. Both
/// branches start at roughly the same rightward lane (same depth, same
/// width tier). Since root's children are plain sequential items in one
/// flex column, "Parent Two" only ever starts after "Parent One"'s own
/// *entire row* (card + branch) ends — collision between the two branches
/// is structurally impossible, not something that needs a JS guard.
const TWO_PARENTS_FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    // See GAP_FIXTURE_CANVAS's own comment — same reason.
    "<!-- meshfox:option name=\"unfold\" -->\n",
    "\n",
    "## Parent One\n",
    "<!-- meshfox:node id=\"parent-one\" -->\n",
    "\n",
    "### A\n",
    "<!-- meshfox:node id=\"a\" -->\n",
    "\n",
    "Some real prose here so this card has non-trivial height.\n",
    "\n",
    "### B\n",
    "<!-- meshfox:node id=\"b\" -->\n",
    "\n",
    "More prose, another real card, part of Parent One's own tall branch.\n",
    "\n",
    "### C\n",
    "<!-- meshfox:node id=\"c\" -->\n",
    "\n",
    "A third child, making Parent One's branch taller than Parent Two's.\n",
    "\n",
    "## Parent Two\n",
    "<!-- meshfox:node id=\"parent-two\" -->\n",
    "\n",
    "### D\n",
    "<!-- meshfox:node id=\"d\" -->\n",
    "\n",
    "Parent Two's first child.\n",
    "\n",
    "### E\n",
    "<!-- meshfox:node id=\"e\" -->\n",
    "\n",
    "Parent Two's second child.\n",
);

#[test]
fn different_depth_one_parents_own_branches_do_not_collide() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    let kv = build_and_inspect(
        "two-parents",
        TWO_PARENTS_FIXTURE_CANVAS,
        r#"{
          // "id:top:bottom" triples, one per depth-2 card, joined with "|"
          // — kept as a plain string (not JSON) so the Rust side needs no
          // extra parsing dependency for just this one test.
          rects: ["a", "b", "c", "d", "e"].map(id => {
            const r = document.getElementById("node-" + id).getBoundingClientRect();
            return id + ":" + r.top + ":" + r.bottom;
          }).join("|"),
        }"#,
    );

    let rects: Vec<(String, f64, f64)> = kv
        .get("rects")
        .expect("missing rects")
        .split('|')
        .map(|entry| {
            let mut parts = entry.splitn(3, ':');
            let id = parts.next().unwrap().to_string();
            let top: f64 = parts.next().unwrap().parse().unwrap();
            let bottom: f64 = parts.next().unwrap().parse().unwrap();
            (id, top, bottom)
        })
        .collect();

    // Parent One's own branch (A, B, C) must not overlap Parent Two's own
    // branch (D, E) — every one of A/B/C's vertical range must end before
    // every one of D/E's vertical range begins.
    let parent_one_bottom = rects[0..3].iter().map(|(_, _, b)| *b).fold(f64::MIN, f64::max);
    let parent_two_top = rects[3..5].iter().map(|(_, t, _)| *t).fold(f64::MAX, f64::min);
    assert!(
        parent_two_top >= parent_one_bottom - 0.5,
        "Parent One's branch (bottom={parent_one_bottom}) must not collide with Parent Two's branch (top={parent_two_top}): {rects:?}"
    );

    // Sanity: A/B/C really do form one tightly-packed, non-overlapping
    // branch among themselves too.
    for pair in rects[0..3].windows(2) {
        assert!(pair[1].1 >= pair[0].2 - 0.5, "Parent One's own children must not overlap each other: {rects:?}");
    }
}

/// A real bug this guards against: `README.md`'s "Usage" section (a wide,
/// 60vw-tier card) has a deeply/widely branching subtree of its own (five
/// children, one of which branches further into a very tall grandchild).
/// This reproduces the same shape (several levels deep, each with enough
/// text that a squeeze would be obvious) and checks the width actually
/// reached is close to the intended tier, not squeezed down.
const DEEP_FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    // See GAP_FIXTURE_CANVAS's own comment — same reason.
    "<!-- meshfox:option name=\"unfold\" -->\n",
    "\n",
    "## Wide\n",
    "<!-- meshfox:node id=\"wide\" -->\n",
    "\n",
    "This section has a fair amount of its own prose, enough that a squeezed-down box would visibly wrap onto many more lines than an unsquashed one at its intended width would.\n",
    "\n",
    "### Deep\n",
    "<!-- meshfox:node id=\"deep\" -->\n",
    "\n",
    "Same idea one level further down — a decent chunk of text, plus a grandchild of its own so this level branches too.\n",
    "\n",
    "#### Deeper\n",
    "<!-- meshfox:node id=\"deeper\" -->\n",
    "\n",
    "And a third, deepest level with yet more prose, so the whole subtree cascades wide the way \"Usage\" -> \"CLI help\" -> \"Node commands\" does in README.md.\n",
);

#[test]
fn width_tiers_are_reached_and_deep_content_branches_right_of_its_real_parent() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    // 1400px viewport: "Wide" (depth 1, shallow) should land at
    // clamp(320px, 60vw, 900px) = 840px; "Deep" (depth 2, deep tier) at
    // clamp(260px, 40vw, 640px) = 560px.
    let kv = build_and_inspect(
        "deep",
        DEEP_FIXTURE_CANVAS,
        r#"{
          wideWidth: document.getElementById("node-wide").getBoundingClientRect().width,
          deepWidth: document.getElementById("node-deep").getBoundingClientRect().width,
          wideRight: document.getElementById("node-wide").getBoundingClientRect().right,
          deepLeft: document.getElementById("node-deep").getBoundingClientRect().left,
          deepPosition: getComputedStyle(document.getElementById("node-deep")).position,
        }"#,
    );
    let get = |k: &str| -> f64 { kv.get(k).unwrap_or_else(|| panic!("missing {k}")).parse().unwrap() };

    let wide_width = get("wideWidth");
    let deep_width = get("deepWidth");
    // A generous tolerance (not exact-pixel), but nowhere near a squeezed
    // fraction of it — the actual regression check.
    assert!(wide_width > 700.0, "\"Wide\" should render close to its 840px tier, not squeezed: {wide_width}px");
    assert!(deep_width > 450.0, "\"Deep\" should render close to its 560px tier, not squeezed: {deep_width}px");

    // "Deep" (depth >=2) is an ordinary flowed flex item — pure CSS, never
    // repositioned by any script. Every `.node` has `position: relative`
    // (an anchor for the connector nub pseudo-element, see style.css), but
    // that's not the same as being *positioned* by anything: no inline
    // `left`/`top` here at all.
    assert_eq!(kv.get("deepPosition").map(String::as_str), Some("relative"), "a depth >=2 node is never repositioned by script anymore — pure CSS flow");
    // Branches right of "Wide"'s own real right edge by exactly
    // `.node-row`'s own `gap` (1.75rem = 28px).
    let gap = get("deepLeft") - get("wideRight");
    assert!((gap - 28.0).abs() < 1.0, "\"Deep\" should sit right of \"Wide\"'s own real right edge by .node-row's own gap (28px): gap={gap}px");
}

/// Root -> Frame (a `group`) -> Section -> Leaf (three levels deep), plus
/// Sibling as a second root-level child — enough to tell "root's direct
/// children" (depth 1, small nudge, no branch-right of their own) apart
/// from "everything deeper" (depth >=2, real rightward branching).
const FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    // See GAP_FIXTURE_CANVAS's own comment — same reason.
    "<!-- meshfox:option name=\"unfold\" -->\n",
    "\n",
    "## Frame\n",
    "<!-- meshfox:node id=\"frame\" type=\"group\" -->\n",
    "\n",
    "### Section\n",
    "<!-- meshfox:node id=\"section\" -->\n",
    "\n",
    "some body text\n",
    "\n",
    "#### Leaf\n",
    "<!-- meshfox:node id=\"leaf\" -->\n",
    "\n",
    "leaf body\n",
    "\n",
    "## Sibling\n",
    "<!-- meshfox:node id=\"sibling\" -->\n",
    "\n",
    "sibling body\n",
);

#[test]
fn root_and_its_direct_children_share_a_column_with_only_a_small_nudge() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    let kv = build_and_inspect(
        "indent",
        FIXTURE_CANVAS,
        r#"{
          rootPosition: getComputedStyle(document.getElementById("node-root")).position,
          framePosition: getComputedStyle(document.getElementById("node-frame")).position,
          sectionPosition: getComputedStyle(document.getElementById("node-section")).position,
          rootLeft: document.getElementById("node-root").getBoundingClientRect().left,
          frameLeft: document.getElementById("node-frame").getBoundingClientRect().left,
          siblingLeft: document.getElementById("node-sibling").getBoundingClientRect().left,
          frameTop: document.getElementById("node-frame").getBoundingClientRect().top,
          rootBottom: document.getElementById("node-root").getBoundingClientRect().bottom,
          sectionLeft: document.getElementById("node-section").getBoundingClientRect().left,
          frameRight: document.getElementById("node-frame").getBoundingClientRect().right,
        }"#,
    );
    let get = |k: &str| -> f64 { kv.get(k).unwrap_or_else(|| panic!("missing {k}")).parse().unwrap() };

    // Nobody is ever repositioned by script — every `.node` has
    // `position: relative` (an anchor for its own connector nub, see
    // style.css), but none has an inline `left`/`top` here.
    assert_eq!(kv.get("rootPosition").map(String::as_str), Some("relative"));
    assert_eq!(kv.get("framePosition").map(String::as_str), Some("relative"));
    assert_eq!(kv.get("sectionPosition").map(String::as_str), Some("relative"));

    // Frame/Sibling's own small nudge right of root (`.root-children`'s
    // 1rem margin-left) — well under the full branch-right gap (28px).
    let root_left = get("rootLeft");
    let frame_left = get("frameLeft");
    let sibling_left = get("siblingLeft");
    let nudge = frame_left - root_left;
    assert!(nudge > 0.0 && nudge < 20.0, "Frame should get only a small nudge right of root, not a full branch step: {nudge}px");
    assert_eq!(frame_left, sibling_left, "Frame and Sibling should share the same small nudge");

    // Frame directly follows root in the same column — right below its
    // card, a normal small sibling gap.
    let frame_top = get("frameTop");
    let root_bottom = get("rootBottom");
    assert!(frame_top - root_bottom < 40.0, "Frame should sit right below root's own card: frame_top={frame_top} root_bottom={root_bottom}");

    // Section (Frame's own child, depth 2) must sit strictly to the right
    // of Frame's real right edge, `.node-row`'s own gap (28px) — real
    // branching, not stacked below or overlapping it.
    let section_left = get("sectionLeft");
    let frame_right = get("frameRight");
    let gap = section_left - frame_right;
    assert!((gap - 28.0).abs() < 1.0, "Section should sit right of Frame's own real right edge by .node-row's own gap (28px): gap={gap}px");
}

#[test]
fn structural_connectors_are_pure_css_with_the_correct_handle_sides() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    let kv = build_and_inspect(
        "connectors",
        FIXTURE_CANVAS,
        r#"{
          // FIXTURE_CANVAS has no meshfox:edge cross-references, so the JS
          // overlay should never even be created — every connector here is
          // pure CSS.
          overlayExists: document.querySelector(".mesh-edge-overlay") ? "yes" : "no",
          // Root's own card must get no right-side connector nub (its own
          // connector to depth-1 runs down its *left* side instead — see
          // .root-children's own doc comment in style.css).
          rootNubDisplay: getComputedStyle(document.getElementById("node-root"), "::after").display,
          // Frame (depth 1) has its own child (Section, depth 2) branching
          // right of it, so it *does* get a right-side nub.
          frameNubDisplay: getComputedStyle(document.getElementById("node-frame"), "::after").display,
          frameNubColor: getComputedStyle(document.getElementById("node-frame"), "::after").backgroundColor,
          // Leaf has no children at all, so it gets no nub — no `content`
          // for the pseudo-element to even render.
          leafNubContent: getComputedStyle(document.getElementById("node-leaf"), "::after").content,
          // Frame's own row (nested under root's .root-children) still
          // gets a "twig" reaching into its own card from the left, same
          // mechanism as any deeper level.
          frameTwigColor: getComputedStyle(document.getElementById("node-frame").closest(".node-row"), "::before").backgroundColor,
          // Section's own row (frame's child, a normal branch-right case)
          // gets the same twig.
          sectionTwigColor: getComputedStyle(document.getElementById("node-section").closest(".node-row"), "::before").backgroundColor,
        }"#,
    );

    assert_eq!(kv.get("overlayExists").map(String::as_str), Some("no"), "no meshfox:edge cross-references in this fixture, so the JS overlay must not exist at all");
    assert_eq!(kv.get("rootNubDisplay").map(String::as_str), Some("none"), "root's own card must not get a right-side connector nub");
    assert_ne!(kv.get("frameNubDisplay").map(String::as_str), Some("none"), "Frame has a child branching right of it, so it must get a right-side connector nub");

    let transparent = |c: &str| c == "rgba(0, 0, 0, 0)" || c == "transparent";
    assert!(!transparent(kv.get("frameNubColor").unwrap()), "Frame's own right-side connector nub must actually be visible (a real color, not transparent)");
    assert!(!transparent(kv.get("frameTwigColor").unwrap()), "Frame's own row must draw a twig connecting it to root's left-side guide line");
    assert!(!transparent(kv.get("sectionTwigColor").unwrap()), "Section's own row must draw a twig connecting it to Frame's right-side nub");
    assert_eq!(kv.get("leafNubContent").map(String::as_str), Some("none"), "a leaf (no children) must get no connector nub at all");
}

#[test]
fn a_real_gap_separates_a_card_from_its_first_child() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    let kv = build_and_inspect(
        "root-gap",
        FIXTURE_CANVAS,
        r#"{
          rootBottom: document.getElementById("node-root").getBoundingClientRect().bottom,
          frameTop: document.getElementById("node-frame").getBoundingClientRect().top,
        }"#,
    );
    let get = |k: &str| -> f64 { kv.get(k).unwrap_or_else(|| panic!("missing {k}")).parse().unwrap() };

    let gap = get("frameTop") - get("rootBottom");
    assert!(gap > 4.0, "root's own card and its first child (Frame) must have a real gap between them, not run together: {gap}px");
}

/// One depth-1 parent with three depth-2 children — enough that the
/// children group's total span is clearly taller than the parent's own
/// card, so a "just start level with the top" bug is unmistakable: the
/// parent would then read as sitting at the *top* of its own branch, not
/// the middle of it.
const CENTERING_FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    // See GAP_FIXTURE_CANVAS's own comment — same reason.
    "<!-- meshfox:option name=\"unfold\" -->\n",
    "\n",
    "## Parent\n",
    "<!-- meshfox:node id=\"parent\" -->\n",
    "\n",
    "A short card.\n",
    "\n",
    "### A\n",
    "<!-- meshfox:node id=\"a\" -->\n",
    "\n",
    "First child, real height.\n",
    "\n",
    "### B\n",
    "<!-- meshfox:node id=\"b\" -->\n",
    "\n",
    "Second child, real height.\n",
    "\n",
    "### C\n",
    "<!-- meshfox:node id=\"c\" -->\n",
    "\n",
    "Third child, real height, making the group much taller than Parent's own card.\n",
);

#[test]
fn depth_two_children_group_is_centered_against_its_immovable_parent() {
    if playwright_core_entry().is_none() {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    }

    let kv = build_and_inspect(
        "centering",
        CENTERING_FIXTURE_CANVAS,
        r#"{
          parentTop: document.getElementById("node-parent").getBoundingClientRect().top,
          parentHeight: document.getElementById("node-parent").getBoundingClientRect().height,
          aTop: document.getElementById("node-a").getBoundingClientRect().top,
          cBottom: document.getElementById("node-c").getBoundingClientRect().bottom,
        }"#,
    );
    let get = |k: &str| -> f64 { kv.get(k).unwrap_or_else(|| panic!("missing {k}")).parse().unwrap() };

    let parent_center = get("parentTop") + get("parentHeight") / 2.0;
    let group_center = (get("aTop") + get("cBottom")) / 2.0;

    // Sanity: the group really is much taller than the parent's own card —
    // otherwise centering vs. top-alignment would look the same and this
    // test wouldn't actually distinguish the two.
    let group_span = get("cBottom") - get("aTop");
    assert!(group_span > get("parentHeight") * 2.0, "fixture didn't reproduce a much-taller-than-parent children group: parent_height={} group_span={group_span}", get("parentHeight"));

    // `.node-row`'s own `align-items: center` gives this for free: the
    // shorter flex item (the card) is centered within the row's own
    // cross-axis size, which the taller item (the children column)
    // defines — no JS measurement or computed offset needed.
    assert!(
        (parent_center - group_center).abs() < 2.0,
        "the depth-2 children group should be centered against Parent's own vertical middle: parent_center={parent_center} group_center={group_center}"
    );
}

/// Wide -> Deep -> Deeper (three levels, cascading wide the same way
/// DEEP_FIXTURE_CANVAS does), plus a `meshfox:edge` cross-reference from
/// "Deeper" back up to "Wide" — the one kind of connector still drawn by
/// JS (it can't be expressed as DOM-adjacent CSS, since it doesn't follow
/// the tree).
const EXTRA_EDGE_FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    // See GAP_FIXTURE_CANVAS's own comment — same reason.
    "<!-- meshfox:option name=\"unfold\" -->\n",
    "\n",
    "## Wide\n",
    "<!-- meshfox:node id=\"wide\" -->\n",
    "\n",
    "This section has a fair amount of its own prose, enough that a squeezed-down box would visibly wrap onto many more lines than an unsquashed one at its intended width would.\n",
    "\n",
    "### Deep\n",
    "<!-- meshfox:node id=\"deep\" -->\n",
    "\n",
    "Same idea one level further down.\n",
    "\n",
    "#### Deeper\n",
    "<!-- meshfox:node id=\"deeper\" -->\n",
    "<!-- meshfox:edge from=\"wide\" -->\n",
    "\n",
    "A cross-reference back up to \"Wide\" lives on this node.\n",
);

#[test]
fn edge_overlay_stays_glued_to_nodes_when_the_canvas_scrolls() {
    let Some(playwright_core) = playwright_core_entry() else {
        eprintln!(
            "skipping: web/node_modules isn't installed locally (run `cd web && npm install` once to enable this test)"
        );
        return;
    };

    let canvas_path = unique_dir("canvas-scroll").join("doc.canvas.md");
    write_file(&canvas_path, EXTRA_EDGE_FIXTURE_CANVAS);
    let out_dir = unique_dir("out-scroll");

    let status = Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(repo_root().join("site-template"))
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let script_path = unique_dir("script-scroll").join("inspect.mjs");
    let html_url = format!("file://{}", out_dir.join("index.html").display());
    // Narrow viewport forces `.canvas-wrap` to actually need horizontal
    // scroll for this (deep, wide) fixture.
    let script = format!(
        r#"
import {{ chromium }} from {playwright_core:?};

const browser = await chromium.launch();
const page = await browser.newPage({{ viewport: {{ width: 500, height: 900 }} }});
await page.goto({html_url:?});

const before = await page.evaluate(() => {{
  const path = document.querySelector(".mesh-edge-overlay > path");
  const node = document.getElementById("node-deeper");
  return {{ pathLeft: path.getBoundingClientRect().left, nodeLeft: node.getBoundingClientRect().left }};
}});

await page.evaluate(() => {{
  document.querySelector(".canvas-wrap").scrollLeft = 200;
}});

const after = await page.evaluate(() => {{
  const path = document.querySelector(".mesh-edge-overlay > path");
  const node = document.getElementById("node-deeper");
  return {{ pathLeft: path.getBoundingClientRect().left, nodeLeft: node.getBoundingClientRect().left }};
}});

console.log("pathShift=" + (before.pathLeft - after.pathLeft));
console.log("nodeShift=" + (before.nodeLeft - after.nodeLeft));
await browser.close();
"#,
    );
    write_file(&script_path, &script);
    let output = Command::new("node").arg(&script_path).output().expect("failed to run node");
    let _ = std::fs::remove_dir_all(script_path.parent().unwrap());
    assert!(
        output.status.success(),
        "playwright inspection script failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let kv = parse_kv(&String::from_utf8_lossy(&output.stdout));
    let get = |k: &str| -> f64 { kv.get(k).unwrap_or_else(|| panic!("missing {k}")).parse().unwrap() };

    let path_shift = get("pathShift");
    let node_shift = get("nodeShift");
    // Sanity: the scroll actually happened and moved the node.
    assert!(node_shift > 50.0, "scrolling .canvas-wrap should have visibly moved node-deeper: node_shift={node_shift}px");
    // The overlay's own path must have shifted by the same amount — glued
    // to the node it connects, not left behind in viewport coordinates.
    assert!(
        (path_shift - node_shift).abs() < 1.0,
        "edge overlay should scroll together with its nodes: path_shift={path_shift}px node_shift={node_shift}px"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
}
