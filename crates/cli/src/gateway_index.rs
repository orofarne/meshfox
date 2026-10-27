//! The pages `meshfox serve --http` renders itself: the canvas index and the
//! token form. Plain server-side HTML, inline styles, one small optional
//! script (the filter box). Every link is relative, so the pages work behind
//! another reverse proxy that mounts the gateway under a path prefix.
//!
//! Everything interpolated into markup goes through [`esc`]: file names and
//! the `next` target come from outside.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Most canvases the index page lists; the rest is cut with a note.
pub const MAX_LISTED: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Path relative to the root, `/`-separated.
    pub path: String,
    pub slug: String,
    pub running: bool,
    /// Seconds since the file was last modified, if known.
    pub age_secs: Option<u64>,
}

impl Row {
    fn url(&self) -> String {
        format!("c/{}/", self.slug)
    }

    fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

pub fn age_text(secs: u64) -> String {
    match secs {
        0..=44 => "just now".to_string(),
        45..=3599 => format!("{} min ago", (secs + 30) / 60),
        3600..=86_399 => format!("{} h ago", (secs + 1800) / 3600),
        _ => format!("{} d ago", (secs + 43_200) / 86_400),
    }
}

const STYLE: &str = "\
:root{color-scheme:light dark;--bg:#fff;--fg:#1d1d1f;--dim:#6e6e73;--line:#d2d2d7;--accent:#0a60d0;--ok:#1a7f37}\
@media (prefers-color-scheme:dark){:root{--bg:#161618;--fg:#ececf0;--dim:#9a9aa3;--line:#3a3a40;--accent:#6aa8ff;--ok:#56d364}}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}\
main{max-width:52rem;margin:0 auto;padding:1.5rem 1rem 3rem}\
h1{font-size:1.25rem;margin:0 0 1rem}h2{font-size:.95rem;color:var(--dim);margin:1.5rem 0 .4rem;font-weight:600}\
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}\
ul{list-style:none;margin:0;padding:0}li.row{display:flex;gap:.75rem;align-items:baseline;padding:.2rem 0;border-bottom:1px solid var(--line)}\
li.row a{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.dim{color:var(--dim);font-size:.85rem}.run{color:var(--ok);font-size:.85rem}\
details{margin:.15rem 0}details>summary{cursor:pointer;padding:.2rem 0}details>ul,details>details{margin-left:1.1rem}\
input[type=search],input[type=password]{width:100%;box-sizing:border-box;padding:.5rem .6rem;font:inherit;color:inherit;background:var(--bg);border:1px solid var(--line);border-radius:6px}\
button{padding:.5rem 1rem;font:inherit;border-radius:6px;border:1px solid var(--line);background:var(--bg);color:inherit;cursor:pointer}\
.err{color:#d1242f}.note{margin:1rem 0;padding:.5rem .75rem;border:1px solid var(--line);border-radius:6px}";

const FILTER_SCRIPT: &str = "\
const box=document.getElementById('q');if(box){box.addEventListener('input',()=>{\
const q=box.value.trim().toLowerCase();\
document.querySelectorAll('li.row').forEach(li=>{li.hidden=q&&!li.dataset.path.includes(q)});\
document.querySelectorAll('details').forEach(d=>{d.hidden=q&&!d.querySelector('li.row:not([hidden])');if(q)d.open=true});});}";

fn page(title: &str, body: &str, script: &str) -> String {
    page_with_head(title, "", body, script)
}

fn page_with_head(title: &str, head: &str, body: &str, script: &str) -> String {
    let script = if script.is_empty() {
        String::new()
    } else {
        format!("<script>{script}</script>")
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         {head}<title>{}</title><style>{STYLE}</style></head><body><main>{body}</main>{script}</body></html>",
        esc(title)
    )
}

fn row_html(row: &Row, label: &str) -> String {
    let mut out = format!(
        // A new tab: the index stays where it is, ready for the next canvas.
        "<li class=\"row\" data-path=\"{}\"><a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a>",
        esc(&row.path.to_lowercase()),
        esc(&row.url()),
        esc(label)
    );
    if row.running {
        out.push_str("<span class=\"run\">● running</span>");
    }
    if let Some(age) = row.age_secs {
        let _ = write!(out, "<span class=\"dim\">{}</span>", esc(&age_text(age)));
    }
    out.push_str("</li>");
    out
}

#[derive(Default)]
struct Dir<'a> {
    dirs: BTreeMap<&'a str, Dir<'a>>,
    files: Vec<&'a Row>,
}

fn build_tree(rows: &[Row]) -> Dir<'_> {
    let mut root = Dir::default();
    for row in rows {
        let mut node = &mut root;
        let mut parts = row.path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                node = node.dirs.entry(part).or_default();
            }
        }
        node.files.push(row);
    }
    root
}

fn dir_html(dir: &Dir<'_>, out: &mut String) {
    if !dir.files.is_empty() {
        out.push_str("<ul>");
        for row in &dir.files {
            out.push_str(&row_html(row, row.file_name()));
        }
        out.push_str("</ul>");
    }
    for (name, sub) in &dir.dirs {
        let _ = write!(out, "<details><summary>{}/</summary>", esc(name));
        dir_html(sub, out);
        out.push_str("</details>");
    }
}

/// Where the index is in its life: the first scan of a big tree can take a
/// while, and a root that cannot be read is not the same as an empty one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexStatus {
    /// The first scan has not finished: there is nothing to list yet.
    pub building: bool,
    /// Why the root could not be scanned.
    pub error: Option<String>,
}

/// The index page. `rows` must be sorted by path; at most [`MAX_LISTED`] are
/// shown. `more` is true when the scan itself stopped early.
#[cfg(test)]
pub fn index_html(rows: &[Row], more: bool) -> String {
    index_html_with(rows, more, &IndexStatus::default())
}

pub fn index_html_with(rows: &[Row], more: bool, status: &IndexStatus) -> String {
    let shown = &rows[..rows.len().min(MAX_LISTED)];
    let cut = rows.len() > MAX_LISTED || more;
    let mut body = String::from("<h1>meshfox</h1>");
    body.push_str("<input id=\"q\" type=\"search\" placeholder=\"Filter\" autocomplete=\"off\">");
    let running: Vec<&Row> = shown.iter().filter(|r| r.running).collect();
    if !running.is_empty() {
        body.push_str("<h2>Running</h2><ul>");
        for row in &running {
            // The whole path, not just the name: it is ambiguous outside its directory.
            let html = row_html(row, &row.path);
            body.push_str(&html);
        }
        body.push_str("</ul>");
    }
    body.push_str("<h2>Canvases</h2>");
    if let Some(error) = &status.error {
        let _ = write!(
            body,
            "<p class=\"err\">Cannot read the served root: {}</p>",
            esc(error)
        );
    } else if status.building {
        body.push_str(
            "<p class=\"note\">Scanning the served root for canvases. This page reloads by itself.</p>",
        );
    } else if shown.is_empty() {
        body.push_str(
            "<p class=\"dim\">No canvases under the served root: a <code>*.canvas.md</code> file, or a \
             <code>.md</code> file whose first line is <code>&lt;!-- meshfox:canvas --&gt;</code>.</p>",
        );
    } else {
        dir_html(&build_tree(shown), &mut body);
    }
    if cut {
        let _ = write!(
            body,
            "<p class=\"note\">Showing the first {} canvases; there are more.</p>",
            shown.len()
        );
    }
    let refresh = if status.building && status.error.is_none() {
        "<meta http-equiv=\"refresh\" content=\"2\">"
    } else {
        ""
    };
    page_with_head("meshfox", refresh, &body, FILTER_SCRIPT)
}

/// Machine-readable twin of the index page (`GET /_/index.json`).
#[cfg(test)]
pub fn index_json(rows: &[Row], truncated: bool) -> serde_json::Value {
    index_json_with(rows, truncated, &IndexStatus::default())
}

pub fn index_json_with(rows: &[Row], truncated: bool, status: &IndexStatus) -> serde_json::Value {
    serde_json::json!({
        "truncated": truncated,
        "indexing": status.building,
        "error": status.error,
        "canvases": rows.iter().map(|r| serde_json::json!({
            "path": r.path,
            "slug": r.slug,
            "url": r.url(),
            "running": r.running,
            "ageSecs": r.age_secs,
        })).collect::<Vec<_>>(),
    })
}

/// The token form. `action` and `next` are already-safe relative targets
/// chosen by the gateway; they are still escaped.
pub fn login_html(action: &str, next: &str, failed: bool) -> String {
    let error = if failed {
        "<p class=\"err\">That token was not accepted.</p>"
    } else {
        ""
    };
    let body = format!(
        "<h1>meshfox</h1><form method=\"post\" action=\"{}\">{error}\
         <p><input type=\"password\" name=\"token\" placeholder=\"Access token\" \
         autocomplete=\"current-password\" autofocus></p>\
         <input type=\"hidden\" name=\"next\" value=\"{}\">\
         <p><button type=\"submit\">Sign in</button></p></form>",
        esc(action),
        esc(next)
    );
    page("meshfox", &body, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, running: bool) -> Row {
        Row {
            path: path.to_string(),
            slug: format!("x-{}", path.len()),
            running,
            age_secs: Some(7200),
        }
    }

    #[test]
    fn escapes_markup_in_names() {
        let html = index_html(&[row("<script>alert(1)</script>.canvas.md", false)], false);
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;.canvas.md"));
        assert_eq!(esc("a\"b'c&d"), "a&quot;b&#39;c&amp;d");
    }

    #[test]
    fn shows_names_with_extension_and_relative_links() {
        let html = index_html(&[row("notes/todo.canvas.md", false)], false);
        assert!(html.contains(">todo.canvas.md</a>"));
        assert!(html.contains("href=\"c/x-20/\" target=\"_blank\" rel=\"noopener\""));
        assert!(html.contains("<summary>notes/</summary>"));
        assert!(!html.contains("href=\"/"));
    }

    #[test]
    fn running_canvases_get_their_own_block_with_the_full_path() {
        let html = index_html(
            &[row("a.canvas.md", false), row("sub/b.canvas.md", true)],
            false,
        );
        let running = html.split("<h2>Canvases</h2>").next().unwrap();
        assert!(running.contains("<h2>Running</h2>"));
        assert!(running.contains(">sub/b.canvas.md</a>"));
        assert!(running.contains("● running"));
        assert!(!running.contains("a.canvas.md"));
    }

    #[test]
    fn empty_root_says_so_and_has_no_running_block() {
        let html = index_html(&[], false);
        assert!(html.contains("No canvases under the served root"));
        assert!(html.contains("<code>*.canvas.md</code>"));
        assert!(!html.contains("<h2>Running</h2>"));
    }

    #[test]
    fn listing_is_cut_at_the_limit_with_a_note() {
        let rows: Vec<Row> = (0..MAX_LISTED + 3)
            .map(|i| row(&format!("c{i:05}.canvas.md"), false))
            .collect();
        let html = index_html(&rows, false);
        assert_eq!(html.matches("class=\"row\"").count(), MAX_LISTED);
        assert!(html.contains(&format!("Showing the first {MAX_LISTED} canvases")));
        assert!(index_html(&rows[..2], true).contains("there are more"));
    }

    #[test]
    fn a_first_scan_still_running_says_so_and_reloads_by_itself() {
        let status = IndexStatus {
            building: true,
            error: None,
        };
        let html = index_html_with(&[], false, &status);
        assert!(html.contains("Scanning the served root"));
        assert!(html.contains("http-equiv=\"refresh\""));
        // Not the "nothing here" message: that would be a lie while scanning.
        assert!(!html.contains("No canvases under the served root"));
        // Once built, no reload.
        assert!(!index_html(&[], false).contains("http-equiv"));
        assert_eq!(index_json_with(&[], false, &status)["indexing"], true);
    }

    #[test]
    fn an_unreadable_root_is_an_error_not_an_empty_list() {
        let status = IndexStatus {
            building: false,
            error: Some("Permission denied (os error 13)".to_string()),
        };
        let html = index_html_with(&[], false, &status);
        assert!(html.contains("Cannot read the served root: Permission denied"));
        assert!(!html.contains("http-equiv"));
        assert_eq!(
            index_json_with(&[], false, &status)["error"],
            "Permission denied (os error 13)"
        );
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age_text(5), "just now");
        assert_eq!(age_text(120), "2 min ago");
        assert_eq!(age_text(7200), "2 h ago");
        assert_eq!(age_text(3 * 86_400), "3 d ago");
    }

    #[test]
    fn json_mirrors_the_rows() {
        let json = index_json(&[row("a.canvas.md", true)], true);
        assert_eq!(json["truncated"], true);
        assert_eq!(json["canvases"][0]["path"], "a.canvas.md");
        assert_eq!(json["canvases"][0]["url"], "c/x-11/");
        assert_eq!(json["canvases"][0]["running"], true);
    }

    #[test]
    fn login_form_posts_relative_and_escapes_next() {
        let html = login_html("../../_/login", "/c/\"x/", true);
        assert!(html.contains("action=\"../../_/login\""));
        assert!(html.contains("value=\"/c/&quot;x/\""));
        assert!(html.contains("not accepted"));
        assert!(!login_html("_/login", "/", false).contains("not accepted"));
    }
}
