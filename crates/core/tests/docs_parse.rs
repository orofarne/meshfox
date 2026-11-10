//! The repo's documentation is a set of valid meshfox canvases in the repo
//! root (see "File format" in `REFERENCE.canvas.md`): `README.md` is the
//! entry point and links the others as `file` nodes under "Documentation".
//! This keeps that claim honest as the docs evolve.

use std::path::{Path, PathBuf};

use meshfox_core::{Canvas, NodeType};

/// Every documentation canvas in the repo root, with its root node's title.
/// `README.md` is the entry point; the rest must be linked from it.
const DOCS: &[(&str, &str)] = &[
    ("README.md", "meshfox"),
    ("REFERENCE.canvas.md", "Reference"),
    ("CLI.canvas.md", "CLI"),
    ("INTERFACES.canvas.md", "Interfaces"),
    ("EXPORT.canvas.md", "Export"),
    ("HOWTO.canvas.md", "How-to"),
    ("ARCHITECTURE.canvas.md", "Architecture"),
    ("DEVELOPMENT.canvas.md", "Development"),
    ("LICENSE.canvas.md", ""),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load(name: &str) -> Canvas {
    let path = repo_root().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{name} should exist at the repo root: {e}"));
    Canvas::from_markdown(&text).unwrap_or_else(|e| panic!("{name} should parse as a canvas: {e}"))
}

fn titles(canvas: &Canvas) -> Vec<&str> {
    canvas.nodes.iter().map(|n| n.title.as_str()).collect()
}

#[test]
fn every_doc_canvas_parses_with_one_root() {
    for (name, root_title) in DOCS {
        let canvas = load(name);
        let root = canvas
            .root()
            .unwrap_or_else(|e| panic!("{name}: exactly one root node expected: {e:?}"));
        if !root_title.is_empty() {
            assert_eq!(&root.title, root_title, "{name}: root title");
        }
    }
}

#[test]
fn readme_has_its_sections_and_links_every_doc_canvas() {
    let readme = load("README.md");
    let titles = titles(&readme);
    for expected in ["Concept", "Installation", "Documentation", "License"] {
        assert!(
            titles.contains(&expected),
            "README.md: missing section {expected:?}, found {titles:?}"
        );
    }

    // Every other doc canvas (LICENSE.canvas.md has its own node under
    // "License") is a `file` node under "Documentation" whose target exists
    // next to the README, so the static site renders it as a page of its own
    // (`meshfox static --recursive`).
    let linked: Vec<String> = readme
        .children("documentation")
        .iter()
        .map(|n| {
            assert_eq!(
                n.node_type,
                NodeType::File,
                "README.md: {:?} is a file node",
                n.title
            );
            n.target.clone().expect("a file node has a target")
        })
        .collect();
    for (name, _) in DOCS
        .iter()
        .filter(|(n, _)| *n != "README.md" && *n != "LICENSE.canvas.md")
    {
        let target = format!("./{name}");
        assert!(
            linked.contains(&target),
            "README.md: no file node under \"Documentation\" links {name} (found {linked:?})"
        );
    }
    for target in &linked {
        assert!(
            repo_root().join(target).exists(),
            "README.md links {target}, which does not exist"
        );
    }
    let license = readme.node("license-deps").expect("License node");
    assert_eq!(license.target.as_deref(), Some("./LICENSE.canvas.md"));
}

#[test]
fn reference_links_the_spec() {
    // "File format" links SPEC.md as a `file` node (the static site renders
    // it as a page of its own) — check that its target really exists.
    let reference = load("REFERENCE.canvas.md");
    for expected in ["File format", "Variables", "Configuration"] {
        assert!(
            titles(&reference).contains(&expected),
            "REFERENCE.canvas.md: missing section {expected:?}"
        );
    }
    let file_format = reference.node("file-format").expect("File format node");
    assert_eq!(file_format.node_type, NodeType::File);
    assert_eq!(file_format.target.as_deref(), Some("./SPEC.md"));
    assert!(repo_root().join("SPEC.md").exists());
}

#[test]
fn development_canvas_keeps_the_project_checks() {
    let development = load("DEVELOPMENT.canvas.md");
    for id in ["full-check", "install", "unit-tests"] {
        assert!(
            development.node(id).is_some(),
            "DEVELOPMENT.canvas.md: missing node {id:?}"
        );
    }
}
