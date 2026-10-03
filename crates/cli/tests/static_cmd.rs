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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolates this process from the real machine's own
    // `~/.meshfox/config.toml` — a developer with `server_socket` set for
    // daily use would otherwise have this command routed through *their*
    // external coordinator instead of a worker of its own (see
    // `validate_cmd.rs`'s `meshfox()` for the full rationale). The
    // directory doesn't need to exist: a missing global config is read as
    // "none".
    cmd.env("HOME", unique_dir("home"));
    cmd
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
    assert!(
        !index.contains("name=\"build\""),
        "fence attrs should be stripped: {index}"
    );

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
    assert!(
        !out_dir.join("_macros.html").exists(),
        "a partial must not become its own output page"
    );

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn template_toml_supplies_links_base_url_and_icons_but_is_never_copied_to_out() {
    let template_dir = unique_dir("template-config");
    write_file(
        &template_dir.join("index.html.tera"),
        "{% for icon in icons %}<link rel=\"{{ icon.rel }}\" href=\"{{ icon.href }}\">\n{% endfor %}{{ site.root.html_body | safe }}",
    );
    write_file(
        &template_dir.join("template.toml"),
        "api_version = 1\nlinks_base_url = \"https://example.com/repo\"\n\n[[icons]]\nrel = \"icon\"\nhref = \"favicon.ico\"\n",
    );
    // A real (if tiny) asset `icons` points at — proves `template.toml`
    // itself doesn't need to be the only new file in the template dir for
    // this to work.
    write_file(
        &template_dir.join("favicon.ico"),
        "not a real ico, just bytes",
    );

    let canvas_path = unique_dir("canvas-config").join("doc.canvas.md");
    // A plain Markdown link in the root's own body — the kind
    // `links_base_url` actually prefixes (see meshfox_core::staticgen's own
    // doc comment); a literal `<a>` written directly in a template's own
    // HTML never goes through that resolution at all, so it wouldn't
    // exercise this.
    write_file(&canvas_path, "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n[LICENSE](./LICENSE)\n");

    let out_dir = unique_dir("out-config");

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
    assert!(
        index.contains(r#"<link rel="icon" href="favicon.ico">"#),
        "{index}"
    );
    // links_base_url from template.toml, not a CLI flag (static has no such flag).
    assert!(
        index.contains(r#"href="https://example.com/repo/LICENSE""#),
        "{index}"
    );

    assert!(
        out_dir.join("favicon.ico").exists(),
        "an ordinary asset icons points at is still copied verbatim"
    );
    assert!(
        !out_dir.join("template.toml").exists(),
        "template.toml is the template's own config, not one of its pages/assets"
    );

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn a_template_with_no_template_toml_gets_an_empty_config() {
    // Same as this file's other fixtures (no template.toml at all) — must
    // keep working exactly as before this file existed: empty `icons`,
    // no `links_base_url` prefixing.
    let template_dir = unique_dir("template-no-config");
    write_file(
        &template_dir.join("index.html.tera"),
        "icons:{% for icon in icons %}{{ icon.href }}{% endfor %}:end",
    );

    let canvas_path = unique_dir("canvas-no-config").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out-no-config");

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
    assert!(
        index.contains("icons::end"),
        "no template.toml means no icons at all: {index}"
    );

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

// TODO.canvas.md: "Опция sitemap.xml (+ даты обновления из git)" —
// `--sitemap`'s own end-to-end behavior (unit coverage lives in
// `meshfox_core::staticgen` for everything upstream of the CLI; `sitemap.xml`
// itself is generated entirely in `crates/cli/src/main.rs`, so it only has
// a CLI-level test to exercise it at all).
#[test]
fn sitemap_lists_the_rendered_page_with_an_absolute_url() {
    let template_dir = unique_dir("template-sitemap");
    write_file(&template_dir.join("index.html.tera"), "{{ site.title }}");
    write_file(
        &template_dir.join("template.toml"),
        "api_version = 1\nbase_url = \"https://example.com/repo\"\n",
    );

    let canvas_path = unique_dir("canvas-sitemap").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out-sitemap");

    let status = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .arg("--sitemap")
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let sitemap = std::fs::read_to_string(out_dir.join("sitemap.xml")).unwrap();
    assert!(
        sitemap.contains("<loc>https://example.com/repo/index.html</loc>"),
        "{sitemap}"
    );
    assert!(
        !sitemap.contains("<lastmod>"),
        "no --sitemap-git-dates means no <lastmod> at all: {sitemap}"
    );

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn sitemap_refuses_to_run_without_a_base_url() {
    let template_dir = unique_dir("template-sitemap-no-base-url");
    write_file(&template_dir.join("index.html.tera"), "{{ site.title }}");

    let canvas_path = unique_dir("canvas-sitemap-no-base-url").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out-sitemap-no-base-url");

    let status = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .arg("--sitemap")
        .status()
        .expect("failed to run meshfox");
    assert!(!status.success());
    assert!(
        !out_dir.exists() || std::fs::read_dir(&out_dir).unwrap().next().is_none(),
        "a refused run shouldn't have written anything"
    );

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn sitemap_git_dates_refuses_to_run_without_sitemap() {
    let template_dir = unique_dir("template-sitemap-git-dates-alone");
    write_file(&template_dir.join("index.html.tera"), "{{ site.title }}");
    write_file(
        &template_dir.join("template.toml"),
        "api_version = 1\nbase_url = \"https://example.com/repo\"\n",
    );

    let canvas_path = unique_dir("canvas-sitemap-git-dates-alone").join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);

    let out_dir = unique_dir("out-sitemap-git-dates-alone");

    let status = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .arg("--sitemap-git-dates")
        .status()
        .expect("failed to run meshfox");
    assert!(!status.success());

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn sitemap_git_dates_sets_lastmod_from_the_canvass_own_commit_date() {
    let template_dir = unique_dir("template-sitemap-git-dates");
    write_file(&template_dir.join("index.html.tera"), "{{ site.title }}");
    write_file(
        &template_dir.join("template.toml"),
        "api_version = 1\nbase_url = \"https://example.com/repo\"\n",
    );

    // A throwaway git repo, committed once, so `git log` has exactly one,
    // known commit date to check `<lastmod>` against — proves the fix for
    // a real bug caught by hand (pairing `-C <dir>` with a pathspec that's
    // relative to the *original* cwd, not to `<dir>`, silently finds no
    // history at all rather than erroring — see `canvas_git_lastmod`'s own
    // doc comment).
    let repo_dir = unique_dir("canvas-sitemap-git-dates-repo");
    let canvas_path = repo_dir.join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(&repo_dir)
            .args(args)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["add", "doc.canvas.md"]);
    // `--date=` alone only sets the *author* date — `canvas_git_lastmod`
    // reads `%cI`, the *committer* date, which `git commit` otherwise
    // always stamps as "now" regardless of `--date`; `GIT_COMMITTER_DATE`
    // is the only way to pin that one too.
    let status = Command::new("git")
        .current_dir(&repo_dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "add doc.canvas.md",
            "--date=2020-01-02T03:04:05+00:00",
        ])
        .env("GIT_COMMITTER_DATE", "2020-01-02T03:04:05+00:00")
        .status()
        .expect("failed to run git commit");
    assert!(status.success(), "git commit failed");

    let out_dir = unique_dir("out-sitemap-git-dates");

    let status = meshfox()
        .arg("static")
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("--out")
        .arg(&out_dir)
        .arg("--sitemap")
        .arg("--sitemap-git-dates")
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let sitemap = std::fs::read_to_string(out_dir.join("sitemap.xml")).unwrap();
    assert!(
        sitemap.contains("<lastmod>2020-01-02T03:04:05"),
        "{sitemap}"
    );

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(&repo_dir);
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Tera only escapes a template whose name ends in `.html`/`.htm`/`.xml`,
/// and ours end in `.tera` — `meshfox static` turns escaping on for them
/// itself, so a title or tag with markup in it reaches the page as text.
#[test]
fn node_titles_and_tags_are_html_escaped_in_the_output() {
    let template_dir = unique_dir("template-escape");
    write_file(
        &template_dir.join("index.html.tera"),
        "<title>{{ site.root.title }}</title>{% for c in site.root.children %}<h2>{{ c.title }}</h2>{% for t in c.tags %}<i>{{ t }}</i>{% endfor %}{% endfor %}",
    );
    let canvas_path = unique_dir("canvas-escape").join("doc.canvas.md");
    write_file(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n",
            "# Root <img src=x onerror=alert(1)>\n",
            "<!-- meshfox:node id=\"root\" -->\n\n",
            "## Child <script>alert(2)</script>\n",
            "<!-- meshfox:node id=\"child\" tags=\"a<b\" -->\n",
        ),
    );
    let out_dir = unique_dir("out-escape");

    let output = meshfox()
        .args(["static"])
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("-o")
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let index = std::fs::read_to_string(out_dir.join("index"))
        .unwrap_or_else(|_| std::fs::read_to_string(out_dir.join("index.html")).unwrap());
    assert!(
        !index.contains("<img"),
        "title markup reached the page: {index}"
    );
    assert!(
        !index.contains("<script"),
        "title markup reached the page: {index}"
    );
    assert!(
        index.contains("&lt;img src=x onerror=alert(1)&gt;"),
        "{index}"
    );
    assert!(index.contains("<i>a&lt;b</i>"), "tag not escaped: {index}");

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Runs `meshfox static` on a one-page template whose `template.toml` is
/// `toml`, expecting it to refuse, and returns what it said on stderr.
fn static_refuses_template_toml(tag: &str, toml: &str) -> String {
    let template_dir = unique_dir(&format!("template-{tag}"));
    write_file(&template_dir.join("index.html.tera"), "{{ site.title }}");
    write_file(&template_dir.join("template.toml"), toml);
    let canvas_path = unique_dir(&format!("canvas-{tag}")).join("doc.canvas.md");
    write_file(&canvas_path, FIXTURE_CANVAS);
    let out_dir = unique_dir(&format!("out-{tag}"));

    let output = meshfox()
        .args(["static"])
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("-o")
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(!output.status.success(), "should have been refused");
    assert!(
        !out_dir.exists(),
        "nothing should be written for a refused template"
    );

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn template_toml_with_an_unknown_key_is_refused() {
    let stderr =
        static_refuses_template_toml("unknown-key", "api_version = 1\nlinks_base_ulr = \"x\"\n");
    assert!(stderr.contains("links_base_ulr"), "{stderr}");
}

#[test]
fn template_toml_with_an_unknown_icon_key_is_refused() {
    let stderr = static_refuses_template_toml(
        "unknown-icon-key",
        "api_version = 1\n[[icons]]\nrel = \"icon\"\nhref = \"a.ico\"\nsize = \"16x16\"\n",
    );
    assert!(stderr.contains("size"), "{stderr}");
}

#[test]
fn template_toml_without_an_api_version_is_refused() {
    let stderr =
        static_refuses_template_toml("no-api-version", "base_url = \"https://example.com\"\n");
    assert!(stderr.contains("api_version"), "{stderr}");
}

#[test]
fn template_toml_for_another_api_version_is_refused() {
    let stderr = static_refuses_template_toml("other-api-version", "api_version = 2\n");
    assert!(stderr.contains("version 2"), "{stderr}");
}

/// `json_encode` leaves `</script>` as it is, so an edge label containing it
/// would end the `<script type="application/json">` element a template puts
/// the edges in; `script_json` writes `<`, `>` and `&` as `\uXXXX`.
#[test]
fn script_json_cannot_end_the_script_element_it_sits_in() {
    let template_dir = unique_dir("template-script-json");
    write_file(
        &template_dir.join("index.html.tera"),
        "<script id=\"d\" type=\"application/json\">{{ site.edges | script_json | safe }}</script>",
    );
    let canvas_path = unique_dir("canvas-script-json").join("doc.canvas.md");
    write_file(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n",
            "<!-- meshfox:edge from=\"a\" label=\"</script><img src=x onerror=alert(9)> & more\" -->\n",
        ),
    );
    let out_dir = unique_dir("out-script-json");

    let output = meshfox()
        .args(["static"])
        .arg(&canvas_path)
        .arg("--template")
        .arg(&template_dir)
        .arg("-o")
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let index = std::fs::read_to_string(out_dir.join("index.html")).unwrap();
    assert_eq!(
        index.matches("</script>").count(),
        1,
        "the element ended early: {index}"
    );
    assert!(!index.contains("<img"), "{index}");
    assert!(index.contains("\\u003c/script\\u003e\\u003cimg"), "{index}");
    // Still the same JSON once a parser has read it.
    let json = index
        .split_once("type=\"application/json\">")
        .and_then(|(_, rest)| rest.split_once("</script>"))
        .map(|(json, _)| json)
        .unwrap();
    let edges: serde_json::Value = serde_json::from_str(json).unwrap();
    let label = edges
        .as_array()
        .unwrap()
        .iter()
        .find_map(|e| e["label"].as_str())
        .unwrap();
    assert_eq!(label, "</script><img src=x onerror=alert(9)> & more");

    let _ = std::fs::remove_dir_all(&template_dir);
    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&out_dir);
}
