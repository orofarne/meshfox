//! README.md is written as a valid meshfox canvas (see its "File format"
//! section) — this keeps that claim honest as the README evolves.

#[test]
fn readme_is_a_valid_canvas() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md");
    let readme = std::fs::read_to_string(path).expect("README.md should exist at the repo root");

    let canvas = meshfox_core::Canvas::from_markdown(&readme)
        .expect("README.md should parse as a meshfox canvas");

    let root = canvas.root().expect("exactly one root node");
    assert_eq!(root.title, "meshfox");

    let titles: Vec<&str> = canvas.nodes.iter().map(|n| n.title.as_str()).collect();
    for expected in ["Concept", "File format", "Architecture", "Development"] {
        assert!(titles.contains(&expected), "missing section {expected:?}, found {titles:?}");
    }

    // "File format" is meshfox's own `include`, dynamically pulling in
    // SPEC.md — check that it's well-formed and its target actually
    // resolves (missing file, cycle, bad parse), same as `meshfox validate`.
    let file_format = canvas.node("file-format").expect("File format node");
    assert_eq!(file_format.node_type, meshfox_core::NodeType::Include);
    assert_eq!(file_format.target.as_deref(), Some("./SPEC.md"));
    meshfox_core::include::resolve(&canvas, std::path::Path::new(path))
        .expect("README.md's include(s) should resolve cleanly");
}
