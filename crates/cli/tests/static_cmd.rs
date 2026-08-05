//! End-to-end test for `meshfox static`: invokes the built binary against a
//! small, self-contained canvas fixture and a minimal fixture template, and
//! checks the output directory it produces — the parts unit tests inside
//! `meshfox_core::staticgen` can't reach (CLI arg parsing, walking the
//! template directory, Tera rendering, asset copying, the `--out`/`--force`
//! clobber guard).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, not-yet-existing directory under the system temp dir — created
/// on demand by `write_file`'s `create_dir_all`, not here.
fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-static-test-{tag}-{nanos}-{n}"))
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn meshfox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

const FIXTURE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    "\n",
    "## Child\n",
    "<!-- meshfox:node id=\"child\" tags=\"demo\" -->\n",
    "\n",
    "```bash name=\"build\" cache\n",
    "echo hi\n",
    "```\n",
);

#[test]
fn renders_tera_files_and_copies_other_assets() {
    let template_dir = unique_dir("template");
    write_file(
        &template_dir.join("index.html.tera"),
        "<h1>{{ site.title }}</h1>\n<p>{{ site.root.id }}: {{ site.root.html_body | safe }}</p>\n{% for n in site.root.children %}<p>{{ n.id }}: {{ n.html_body | safe }}</p>\n{% endfor %}",
    );
    write_file(&template_dir.join("style.css"), "body { color: red; }\n");

    let canvas_path = unique_dir("canvas").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out");

    let status = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let index = std::fs::read_to_string(out_dir.join("index.html")).unwrap();
    assert!(index.contains("<h1>Root</h1>"), "{index}");
    assert!(index.contains("root:"), "{index}");
    assert!(index.contains("child:"), "{index}");
    assert!(index.contains("language-bash"), "{index}");
    assert!(!index.contains("name=\"build\""), "fence attrs should be stripped: {index}");

    let css = std::fs::read_to_string(out_dir.join("style.css")).unwrap();
    assert_eq!(css, "body { color: red; }\n");

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn recursive_macro_partial_is_imported_but_never_rendered_as_its_own_page() {
    let template_dir = unique_dir("template-macro");
    // A `_`-prefixed file is a partial: importable via `{% import %}`, but
    // `static_cmd` must never render it as a standalone output page.
    write_file(
        &template_dir.join("_macros.html.tera"),
        "{% macro node(n) -%}\n<div id=\"node-{{ n.id }}\">{{ n.title }}{% for c in n.children %}{{ self::node(n=c) }}{% endfor %}</div>\n{%- endmacro node %}",
    );
    write_file(
        &template_dir.join("index.html.tera"),
        "{% import \"_macros.html.tera\" as macros %}{{ macros::node(n=site.root) }}",
    );

    let canvas_path = unique_dir("canvas-macro").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out-macro");

    let status = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let index = std::fs::read_to_string(out_dir.join("index.html")).unwrap();
    // Both the root and the (recursively rendered) child must appear.
    assert!(index.contains("id=\"node-root\""), "{index}");
    assert!(index.contains("id=\"node-child\""), "{index}");
    assert!(!out_dir.join("_macros.html").exists(), "a partial must not become its own output page");

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn refuses_to_clobber_a_non_empty_out_dir_without_force() {
    let template_dir = unique_dir("template2");
    write_file(&template_dir.join("index.html.tera"), "{{ site.title }}");

    let canvas_path = unique_dir("canvas2").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out2");
    write_file(&out_dir.join("stale.txt"), "leftover");

    let without_force = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .status()
        .expect("failed to run meshfox");
    assert!(!without_force.success());
    // The stale file must survive the refused run.
    assert!(out_dir.join("stale.txt").exists());

    let with_force = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .arg("--force")
        .status()
        .expect("failed to run meshfox");
    assert!(with_force.success());
    assert!(out_dir.join("index.html").exists());

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}
