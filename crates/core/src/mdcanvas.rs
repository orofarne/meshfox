//! `meshfox_model::mdcanvas` plus the one disk-reading helper.

pub use meshfox_model::mdcanvas::*;

/// Whether the file at `path` counts as a canvas: a `*.canvas.md` by name
/// ([`is_canvas_path`]), or a plain `*.md` that opens with the
/// `meshfox:canvas` marker ([`has_marker`]). The name check goes first, and
/// only a `*.md` is ever read, so a `.png`/`.pdf`/etc. never is just to be
/// rejected. An unreadable file is not a canvas.
pub fn is_canvas(path: &std::path::Path) -> bool {
    meshfox_model::mdcanvas::is_canvas_path(path)
        || (path.extension().is_some_and(|ext| ext == "md")
            && std::fs::read_to_string(path)
                .is_ok_and(|contents| meshfox_model::mdcanvas::has_marker(&contents)))
}

#[cfg(test)]
mod file_node_argument_tests {
    use super::*;

    const FILE_BLOCK_DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Seed\n<!-- meshfox:node id=\"seed\" type=\"file\" interpreter=\"bash\" cache=\"true\" deps=\"setup\" -->\n\n",
        "[seed](./seed.sh)\n\nLoads the data.\n",
    );

    const FILE_ARGS_DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Extract\n<!-- meshfox:node id=\"extract\" type=\"file\" interpreter=\"python3\" -->\n\n",
        "<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" default=\"en\" -->\n",
        "<!-- meshfox:arg name=\"pages\" type=\"int\" default=\"0\" -->\n",
        "[extract](./extract.py)\n\nPulls the tables.\n",
    );

    #[test]
    fn file_node_takes_arguments_declared_right_above_its_link() {
        let canvas = parse(FILE_ARGS_DOC).unwrap();
        let node = canvas.node("extract").unwrap();
        assert_eq!(node.target.as_deref(), Some("./extract.py"));
        assert_eq!(node.caption.as_deref(), Some("Pulls the tables."));
        let signatures = crate::args::scan_node_signatures(node).unwrap();
        assert_eq!(signatures.len(), 1);
        let names: Vec<_> = signatures[0].args.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["lang", "pages"]);
        crate::args::validate(&canvas).unwrap();
    }

    #[test]
    fn file_node_application_binds_values_and_builds_argv_in_declaration_order() {
        let canvas = parse(FILE_ARGS_DOC).unwrap();
        let node = canvas.node("extract").unwrap();
        let block =
            crate::args::bind_node_block(node, "extract[pages=03,lang=hy]", &Default::default())
                .unwrap();
        assert_eq!(block.name.as_deref(), Some("extract[lang=hy,pages=3]"));
        assert_eq!(block.argv, ["--lang=hy", "--pages=3"]);
        assert_eq!(block.arguments["lang"], "hy");
        assert!(block.target.is_some());
        // Defaults fill in when the address binds nothing.
        let defaulted = crate::args::bind_node_block(node, "extract", &Default::default()).unwrap();
        assert_eq!(defaulted.argv, ["--lang=en", "--pages=0"]);
    }

    #[test]
    fn argv_false_leaves_the_environment_only() {
        let doc = FILE_ARGS_DOC.replace(
            "interpreter=\"python3\"",
            "interpreter=\"python3\" argv=\"false\"",
        );
        let canvas = parse(&doc).unwrap();
        let block = crate::args::bind_node_block(
            canvas.node("extract").unwrap(),
            "extract[lang=hy]",
            &Default::default(),
        )
        .unwrap();
        assert!(block.argv.is_empty());
        assert_eq!(block.arguments["lang"], "hy");
    }

    #[test]
    fn a_block_with_arguments_cannot_be_cached() {
        let doc = FILE_ARGS_DOC.replace(
            "interpreter=\"python3\"",
            "interpreter=\"python3\" cache=\"true\"",
        );
        let canvas = parse(&doc).unwrap();
        let err = crate::args::validate(&canvas).unwrap_err().to_string();
        assert!(err.contains("cannot use `cache`"), "{err}");
        let fence = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n<!-- meshfox:arg name=\"n\" default=\"1\" -->\n```bash name=\"x\" cache\necho $n\n```\n";
        let canvas = parse(fence).unwrap();
        assert!(crate::args::validate(&canvas).is_err());
    }

    #[test]
    fn file_node_arguments_must_sit_immediately_above_the_link() {
        let doc = FILE_ARGS_DOC.replace(
            "[extract](./extract.py)",
            "Some prose.\n\n[extract](./extract.py)",
        );
        // Prose between the signature and the link is not a link body.
        assert!(parse(&doc).is_err());
        // A non-runnable file node has no signature to declare.
        let plain = FILE_ARGS_DOC.replace(" interpreter=\"python3\"", "");
        assert!(parse(&plain).is_err());
    }

    #[test]
    fn block_attrs_are_rejected_off_a_runnable_file_node_and_tty_on_one() {
        let text_node = "# Root\n<!-- meshfox:node id=\"root\" cache=\"true\" -->\n";
        assert!(unknown_node_edge_attr(text_node).is_some());
        let link_only = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n## F\n<!-- meshfox:node id=\"f\" type=\"file\" deps=\"root\" -->\n\n[f](./f.sh)\n";
        assert!(unknown_node_edge_attr(link_only).is_some());
        assert!(unknown_node_edge_attr(FILE_BLOCK_DOC).is_none());
        let tty = FILE_BLOCK_DOC.replace("cache=\"true\"", "tty=\"true\"");
        let canvas = parse(&tty).unwrap();
        assert!(matches!(
            crate::deps::validate(&canvas),
            Err(crate::deps::DepsError::FileNodeUnsupportedAttr(..))
        ));
    }

    #[test]
    fn file_node_markdown_renders_like_a_fence_with_current_output() {
        let canvas = parse(
            FILE_ARGS_DOC
                .replace(
                    " interpreter=\"python3\"",
                    " interpreter=\"python3\" deps=\"root\"",
                )
                .as_str(),
        )
        .unwrap();
        let mut node = canvas.node("extract").unwrap().clone();
        node.set_target_digest(b"print(1)");
        node.block_fingerprint = Some(crate::fence::fingerprint(&crate::fence::target_block(
            &node,
        )));
        let md = crate::fence::file_node_markdown(&node, "print(1)").unwrap();
        // The fence is found, named after the node, with its arguments and deps.
        let blocks = crate::fence::scan_runnable_blocks("extract", &md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "print(1)");
        assert_eq!(blocks[0].deps.len(), 1);
        let signatures = crate::args::scan_signatures("extract", &md).unwrap();
        assert_eq!(signatures[0].args.len(), 2);
        assert!(md.contains("[extract](./extract.py)"));
        assert!(md.contains("Pulls the tables."));

        // Cached output whose hash is the node's current fingerprint reads as current.
        let out = crate::output::ExecOutput {
            exit_code: 0,
            output: "x\n".into(),
            duration_ms: 1,
            stdout: "x\n".into(),
            stderr: String::new(),
        };
        let mut cached = canvas.node("extract").unwrap().clone();
        cached.set_target_digest(b"print(1)");
        cached.block_fingerprint = Some(crate::fence::fingerprint(&crate::fence::target_block(
            &cached,
        )));
        cached.text = crate::output::write_block_output(&cached, "extract", &out).unwrap();
        let md = crate::fence::file_node_markdown(&cached, "print(1)").unwrap();
        let block = &crate::fence::scan_code_blocks(&md)[0];
        assert_eq!(
            crate::output::cached_output_hash(&md, "extract"),
            Some(crate::fence::fingerprint(block))
        );
        // ... and stays untouched (so stale) once the file changed.
        cached.block_fingerprint = Some("deadbeef".into());
        let stale = crate::fence::file_node_markdown(&cached, "print(2)").unwrap();
        let block = &crate::fence::scan_code_blocks(&stale)[0];
        assert_ne!(
            crate::output::cached_output_hash(&stale, "extract"),
            Some(crate::fence::fingerprint(block))
        );
    }
}
