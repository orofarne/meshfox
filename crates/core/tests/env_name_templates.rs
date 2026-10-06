use meshfox_core::{deps::BlockAddr, Canvas};
use std::collections::HashMap;

fn canvas(kind: &str) -> Canvas {
    Canvas::from_markdown(&format!(
        r#"# Root
<!-- meshfox:node id="root" -->
<!-- meshfox:var name="URL_en" from="observe" -->
<!-- meshfox:var name="URL_hy" from="observe" -->
```bash name="observe" always
true
```
## Fetch
<!-- meshfox:node id="fetch" -->
<!-- meshfox:arg name="lang" {kind} -->
```bash name="fetch" env="URL=URL_${{lang}}"
echo "$URL"
```
"#
    ))
    .unwrap()
}

#[test]
fn selected_env_is_lazy_and_freshness_is_independent() {
    let canvas = canvas("type=\"string\"");
    meshfox_core::vars::validate_env_refs(&canvas).unwrap();
    meshfox_core::vars::validate_var_scope(&canvas).unwrap();
    let addr = BlockAddr::new("fetch", "fetch[lang=hy]");
    let chain =
        meshfox_core::resolve_run_chain(&canvas, &["fetch"], &addr.block_name, true).unwrap();
    assert_eq!(chain[0], BlockAddr::new("root", "observe"));
    let needed = meshfox_core::env_var_names_for_chain(&canvas, &chain);
    assert!(needed.contains("URL_hy"));
    assert!(!needed.contains("URL_en"));
    let block = meshfox_core::deps::find_block(&canvas, &addr).unwrap();
    let raw = meshfox_core::scan_runnable_blocks("fetch", &canvas.node("fetch").unwrap().text)
        .pop()
        .unwrap();
    assert_eq!(
        meshfox_core::fingerprint(&raw),
        meshfox_core::fingerprint(&block)
    );
    let mut values = HashMap::from([
        ("URL_en".into(), "en-a".into()),
        ("URL_hy".into(), "hy-a$literal".into()),
    ]);
    assert_eq!(
        meshfox_core::args::block_env(&block, &values)["URL"],
        "hy-a$literal"
    );
    let before = meshfox_core::closure_fingerprint(&canvas, &addr, &values).unwrap();
    values.insert("URL_en".into(), "en-b".into());
    assert_eq!(
        before,
        meshfox_core::closure_fingerprint(&canvas, &addr, &values).unwrap()
    );
    values.insert("URL_hy".into(), "hy-b".into());
    assert_ne!(
        before,
        meshfox_core::closure_fingerprint(&canvas, &addr, &values).unwrap()
    );
}

#[test]
fn open_strings_validate_but_unknown_selected_name_fails_planning() {
    let canvas = canvas("type=\"string\"");
    let error = meshfox_core::resolve_run_chain(&canvas, &["fetch"], "fetch[lang=fr]", false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("URL_fr"), "{error}");
    assert!(error.contains("fetch[lang=fr]"), "{error}");
}

#[test]
fn finite_choices_and_template_syntax_validate_statically() {
    meshfox_core::vars::validate_env_refs(&canvas("type=\"select\" choices=\"en,hy\"")).unwrap();
    assert!(
        meshfox_core::vars::validate_env_refs(&canvas("type=\"select\" choices=\"en,fr\""))
            .unwrap_err()
            .to_string()
            .contains("URL_fr")
    );
    let source = canvas("type=\"string\"").to_markdown();
    for bad in ["URL_${missing}", "URL_${lang", "URL_$lang"] {
        let canvas = Canvas::from_markdown(&source.replace("URL_${lang}", bad)).unwrap();
        assert!(
            meshfox_core::vars::validate_env_refs(&canvas).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn dynamic_refs_do_not_escape_node_scopes_or_reinterpret_arguments() {
    let source = "# Root\n<!-- meshfox:node id=\"root\" -->\n## Private\n<!-- meshfox:node id=\"private\" -->\n<!-- meshfox:var name=\"URL_hy\" default=\"secret\" -->\n## Fetch\n<!-- meshfox:node id=\"fetch\" -->\n<!-- meshfox:arg name=\"lang\" type=\"string\" -->\n```bash name=\"fetch\" env=\"URL=URL_${lang}\"\necho hi\n```\n";
    let canvas = Canvas::from_markdown(source).unwrap();
    assert!(
        meshfox_core::resolve_run_chain(&canvas, &["fetch"], "fetch[lang=hy]", true)
            .unwrap_err()
            .to_string()
            .contains("subtree")
    );
    assert!(
        meshfox_core::resolve_run_chain(&canvas, &["fetch"], "fetch[lang=\"${other}\"]", true)
            .is_err()
    );
}

#[test]
fn selected_canvas_variable_survives_argument_shadowing() {
    let canvas = Canvas::from_markdown(
        r#"# Root
<!-- meshfox:node id="root" -->
<!-- meshfox:var name="lang" from="observe" -->
```bash name="observe"
true
```
<!-- meshfox:arg name="lang" -->
<!-- meshfox:arg name="selector" -->
```bash name="fetch" env="URL=${selector}"
true
```
"#,
    )
    .unwrap();
    let chain =
        meshfox_core::resolve_run_chain(&canvas, &[], "fetch[lang=local,selector=lang]", true)
            .unwrap();
    assert_eq!(chain[0], BlockAddr::new("root", "observe"));
    assert!(meshfox_core::env_var_names_for_chain(&canvas, &chain).contains("lang"));
    let block = meshfox_core::deps::find_block(&canvas, chain.last().unwrap()).unwrap();
    let values = HashMap::from([("lang".into(), "global-url".into())]);
    let env = meshfox_core::args::block_env(&block, &values);
    assert_eq!(env["URL"], "global-url");
    assert_eq!(env["lang"], "local");
}
