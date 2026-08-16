//! Structural checks against the *real* `site-template/` this repo ships
//! (not a fixture-in-string template — the whole point here is to catch a
//! regression in the actual shipped `_macros.html.tera`/`style.css`, not
//! just in some other test's simplified stand-in).
//!
//! These exist because of a real bug: an earlier version of
//! `_macros.html.tera` nested a child's whole `.node-row` *inside* its
//! parent's own `.node` card element, instead of as a sibling. Nothing
//! about that was a Tera syntax error or a Rust panic — the page rendered
//! fine — but every level of nesting ate into the next one's available
//! width and drew its border right on top of the level above, so the page
//! visually came out with the wrong widths and overlapping borders. A pure
//! data-level test (`meshfox_core::staticgen`'s own unit tests) can't catch
//! this at all, since the bug was entirely in how the template arranges
//! elements — so this parses the actual rendered HTML and asserts on its
//! DOM structure instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scraper::{Html, Selector};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-site-template-test-{tag}-{nanos}-{n}"))
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn site_template_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site-template")
}

fn render(canvas_md: &str, tag: &str) -> Html {
    let canvas_path = unique_dir(&format!("canvas-{tag}")).join("doc.canvas.md");
    write_file(&canvas_path, canvas_md);
    let out_dir = unique_dir(&format!("out-{tag}"));

    let status = Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(site_template_dir())
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let html = std::fs::read_to_string(out_dir.join("index.html")).unwrap();
    let doc = Html::parse_document(&html);

    let _ = std::fs::remove_dir_all(&out_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    doc
}

/// Three levels deep (root -> section -> leaf), plus a `group` node and a
/// sibling — enough nesting that a card-inside-card bug is unmistakable in
/// the resulting DOM (a two-level-only fixture could pass by accident).
const FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
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
fn no_node_card_is_nested_inside_another_nodes_card() {
    let doc = render(FIXTURE_CANVAS, "no-nest");

    // The exact bug class this test exists for: a `.node` card must never
    // contain another `.node` card as a descendant — every node's card
    // sizes and draws its border independently of how deep it's nested.
    let node_in_node = Selector::parse(".node .node").unwrap();
    assert_eq!(
        doc.select(&node_in_node).count(),
        0,
        "a .node card must never be nested inside another .node card"
    );

    // Same bug, other direction: the children container must not be
    // nested inside a card either (it needs to be a *sibling* of `.node`,
    // not a descendant — see `_macros.html.tera`'s doc comment).
    let children_in_node = Selector::parse(".node .node-children").unwrap();
    assert_eq!(
        doc.select(&children_in_node).count(),
        0,
        ".node-children must never be nested inside a .node card"
    );

    // Sanity: every node from the fixture actually made it into the page,
    // one `.node` card each (root, frame, section, leaf, sibling).
    let node_sel = Selector::parse(".node").unwrap();
    assert_eq!(doc.select(&node_sel).count(), 5);

    // And every one of those got its own `.node-row` wrapper (including
    // the root, directly under `.tree`).
    let row_sel = Selector::parse(".node-row").unwrap();
    assert_eq!(doc.select(&row_sel).count(), 5);
}

#[test]
fn each_node_children_block_is_a_sibling_of_its_own_node_card() {
    let doc = render(FIXTURE_CANVAS, "siblings");

    // Every `.node-row` that has a `.node-children` child must also have a
    // `.node` child at the same level (siblings), for every row in the
    // page — not just the root.
    let row_sel = Selector::parse(".node-row").unwrap();
    let direct_node_sel = Selector::parse(":scope > .node").unwrap();
    let direct_children_sel = Selector::parse(":scope > .node-children").unwrap();

    let mut rows_with_children = 0;
    for row in doc.select(&row_sel) {
        let has_children = row.select(&direct_children_sel).next().is_some();
        if has_children {
            rows_with_children += 1;
            assert!(
                row.select(&direct_node_sel).next().is_some(),
                "a .node-row with children must have its own .node card as a direct sibling child"
            );
        }
    }
    // frame, section, root each have children in the fixture (leaf and
    // sibling are leaves) — three rows should have a children block.
    assert_eq!(rows_with_children, 3);
}

#[test]
fn data_depth_matches_the_trees_real_depth_not_the_heading_level() {
    let doc = render(FIXTURE_CANVAS, "depth");

    let depth_of = |doc: &Html, id: &str| -> String {
        let sel = Selector::parse(&format!("#node-{id}")).unwrap();
        doc.select(&sel)
            .next()
            .unwrap()
            .value()
            .attr("data-depth")
            .unwrap()
            .to_string()
    };
    assert_eq!(depth_of(&doc, "root"), "0");
    assert_eq!(depth_of(&doc, "frame"), "1");
    assert_eq!(depth_of(&doc, "sibling"), "1");
    assert_eq!(depth_of(&doc, "section"), "2");
    assert_eq!(depth_of(&doc, "leaf"), "3");
}

#[test]
fn only_roots_own_children_block_gets_the_root_children_class() {
    let doc = render(FIXTURE_CANVAS, "root-children-class");

    // `.root-children` (the small `ROOT_CHILD_INDENT`-style nudge, see
    // style.css) belongs only to root's own `.node-children` — Frame's own
    // (wrapping Section) is one level deeper and must get the plain,
    // larger-indent `.node-children` instead.
    let root_children_sel = Selector::parse(".node-children.root-children").unwrap();
    let matches: Vec<_> = doc.select(&root_children_sel).collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one .node-children.root-children (root's own)"
    );

    let direct_node_sel = Selector::parse(":scope > .node-row > .node").unwrap();
    let ids: Vec<String> = matches[0]
        .select(&direct_node_sel)
        .map(|n| n.value().attr("id").unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["node-frame", "node-sibling"]);
}

#[test]
fn shallow_and_deep_width_tier_classes_match_depth() {
    let doc = render(FIXTURE_CANVAS, "tiers");

    let class_of = |doc: &Html, id: &str| -> String {
        let sel = Selector::parse(&format!("#node-{id}")).unwrap();
        doc.select(&sel)
            .next()
            .unwrap()
            .value()
            .attr("class")
            .unwrap()
            .to_string()
    };
    // depth <=1 (root, its direct children) -> .shallow; depth >=2 -> .deep.
    assert!(class_of(&doc, "root").contains("shallow"));
    assert!(class_of(&doc, "frame").contains("shallow"));
    assert!(class_of(&doc, "sibling").contains("shallow"));
    assert!(class_of(&doc, "section").contains("deep"));
    assert!(class_of(&doc, "leaf").contains("deep"));
}
