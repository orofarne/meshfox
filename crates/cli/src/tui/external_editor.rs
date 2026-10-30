//! Editing the source in the user's own editor (`[tui] editor` in
//! `.meshfox/config.toml`). The buffer's text is materialised in a scratch
//! file, the editor runs on the real terminal until it exits, and the file
//! is read back. Nothing here talks to the worker: the caller loads the
//! result into the source editor and saves it the ordinary way
//! (`App::save_source_editor`), so a file that changed meanwhile is merged
//! or compared exactly as for a save from the built-in editor.
//!
//! The scratch file is always named `*.canvas.md`, whatever the edited file
//! is called, so editors recognise it and apply the meshfox syntax.

use meshfox_core::open_rules::split_words;
use std::io;
use std::path::{Path, PathBuf};

/// What to edit, and where to put the editor's cursor (both 1-based).
pub struct Request {
    pub template: String,
    pub text: String,
    /// Stem for the scratch file's name — only for the editor's title bar.
    pub name: String,
    pub line: usize,
    pub col: usize,
}

pub enum Outcome {
    /// The editor exited successfully and the file differs from what it
    /// was given.
    Edited(String),
    Unchanged,
    /// The editor exited unsuccessfully (`:cq` in vim): the user backed out.
    Aborted(String),
    /// The scratch file could not be written or read, or the editor could
    /// not be started.
    Failed(String),
}

/// The configured command template, if any. Only the config counts:
/// `$EDITOR` is set on nearly every machine, and picking it up implicitly
/// would take `e` away from the built-in editor for everyone.
pub fn configured(canvas_root: &Path) -> Option<String> {
    meshfox_core::config::load(canvas_root)
        .get("tui")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("editor"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Runs the editor to completion. The terminal must already be handed
/// over (raw mode and the alternate screen left).
pub async fn run(request: Request) -> Outcome {
    let scratch = match Scratch::create(&request.name, &request.text) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(format!("couldn't create the scratch file: {e}")),
    };
    let argv = match build_argv(&request.template, &scratch.file, request.line, request.col) {
        Ok(argv) => argv,
        Err(e) => return Outcome::Failed(format!("bad [tui] editor: {e}")),
    };
    let status = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .await;
    match status {
        Err(e) => Outcome::Failed(format!("couldn't run {}: {e}", argv[0])),
        Ok(status) if !status.success() => Outcome::Aborted(format!(
            "{} exited with {} — nothing saved",
            argv[0],
            status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"))
        )),
        Ok(_) => match tokio::fs::read_to_string(&scratch.file).await {
            Err(e) => Outcome::Failed(format!("couldn't read the edited file back: {e}")),
            Ok(text) if text == request.text => Outcome::Unchanged,
            Ok(text) => Outcome::Edited(text),
        },
    }
}

/// A private directory holding the one scratch file, removed on drop.
///
/// This is the one place the TUI touches the filesystem itself, a
/// deliberate exception to "the TUI is a client; all filesystem access goes
/// through the core API" (opening file nodes, includes, config grants — see
/// `FileAccess`). That rule exists so the TUI never resolves, checks or
/// reads *canvas* paths on its own: those are governed by file grants and
/// owned by the worker. The scratch file is not that kind of file:
///
/// - it is not part of the canvas or its tree — it lives under
///   `std::env::temp_dir()` in a fresh `meshfox-edit-<uuid>` directory
///   (mode 0700 on unix) that this process creates, and nothing else reads
///   or links to it;
/// - its content is a copy of text the TUI already holds, fetched from the
///   worker (`get_canvas_raw`/`get_include_raw`); the result goes back
///   through the worker (`put_canvas_raw`, with `base_rev`), never straight
///   to the canvas file — so grants, validation and conflict handling
///   still apply to everything that lands on disk;
/// - it has to be a real local file on this machine's disk: the editor is
///   a local process that takes a path, and the worker may be remote or
///   shared, so routing it through the worker would make no sense.
///
/// Do not widen this: reading or writing anything but this scratch file
/// here, or writing the canvas file directly, would break the boundary.
struct Scratch {
    dir: PathBuf,
    file: PathBuf,
}

impl Scratch {
    fn create(name: &str, text: &str) -> io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("meshfox-edit-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir)?;
        let scratch = Scratch {
            file: dir.join(format!("{name}.canvas.md")),
            dir,
        };
        std::fs::write(&scratch.file, text)?;
        Ok(scratch)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The command line for `template`. `{file}`, `{line}` and `{col}` are
/// replaced inside each word, after splitting, so a path with spaces stays
/// one argument and nothing is ever passed through a shell. A template
/// without `{file}` is a bare command: the file is appended, preceded by
/// the cursor position in the form the editor is known to take.
fn build_argv(template: &str, file: &Path, line: usize, col: usize) -> Result<Vec<String>, String> {
    let words = split_words(template)?;
    let Some(program) = words.first() else {
        return Err("empty command".into());
    };
    let file_str = file.to_string_lossy();
    let fill = |w: &str| {
        w.replace("{file}", &file_str)
            .replace("{line}", &line.to_string())
            .replace("{col}", &col.to_string())
    };
    if words.iter().any(|w| w.contains("{file}")) {
        return Ok(words.iter().map(|w| fill(w)).collect());
    }
    let mut argv: Vec<String> = words.iter().map(|w| fill(w)).collect();
    let base = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match base.as_str() {
        "vim" | "nvim" | "vi" | "nano" | "emacs" | "kak" | "ex" => {
            argv.push(format!("+{line}"));
            argv.push(file_str.into_owned());
        }
        "hx" | "helix" | "subl" | "sublime_text" | "zed" => {
            argv.push(format!("{file_str}:{line}:{col}"));
        }
        // GUI editors that return at once unless told to wait.
        "code" | "codium" | "cursor" => {
            if !argv.iter().any(|a| a == "--wait" || a == "-w") {
                argv.push("--wait".into());
            }
            argv.push("-g".into());
            argv.push(format!("{file_str}:{line}:{col}"));
        }
        _ => argv.push(file_str.into_owned()),
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(template: &str) -> Vec<String> {
        build_argv(template, Path::new("/tmp/a b/x.canvas.md"), 12, 3).unwrap()
    }

    #[test]
    fn placeholders_fill_inside_words_and_keep_a_path_with_spaces_whole() {
        assert_eq!(
            argv("myed --at={line}:{col} '{file}' -x"),
            ["myed", "--at=12:3", "/tmp/a b/x.canvas.md", "-x"]
        );
    }

    #[test]
    fn a_bare_known_editor_gets_its_own_line_syntax() {
        assert_eq!(argv("nvim"), ["nvim", "+12", "/tmp/a b/x.canvas.md"]);
        assert_eq!(argv("hx"), ["hx", "/tmp/a b/x.canvas.md:12:3"]);
        assert_eq!(
            argv("code"),
            ["code", "--wait", "-g", "/tmp/a b/x.canvas.md:12:3"]
        );
        assert_eq!(argv("code -w")[1..3], ["-w", "-g"]);
        assert_eq!(argv("unknown-ed"), ["unknown-ed", "/tmp/a b/x.canvas.md"]);
    }

    #[test]
    fn a_template_with_file_is_never_second_guessed() {
        assert_eq!(
            argv("vim -c 'normal {line}G' {file}"),
            ["vim", "-c", "normal 12G", "/tmp/a b/x.canvas.md"]
        );
    }

    #[test]
    fn an_empty_template_is_rejected() {
        assert!(build_argv("  ", Path::new("f"), 1, 1).is_err());
    }

    #[tokio::test]
    async fn the_edited_file_is_read_back_and_the_scratch_dir_removed() {
        let run_with = |template: &str, text: &str| {
            run(Request {
                template: template.into(),
                text: text.into(),
                name: "doc".into(),
                line: 1,
                col: 1,
            })
        };
        // `$0` is the scratch file: `sh -c '…' {file}`.
        let edited = run_with("sh -c 'printf more >> \"$0\"' {file}", "text\n").await;
        assert!(matches!(&edited, Outcome::Edited(t) if t == "text\nmore"));
        let same = run_with("sh -c true {file}", "text\n").await;
        assert!(matches!(same, Outcome::Unchanged));
        let quit = run_with("sh -c 'printf more >> \"$0\"; exit 3' {file}", "text\n").await;
        assert!(matches!(&quit, Outcome::Aborted(m) if m.contains("code 3")));
        let missing = run_with("meshfox-no-such-editor", "text\n").await;
        assert!(matches!(missing, Outcome::Failed(_)));
    }

    #[test]
    fn scratch_file_is_a_canvas_md_and_goes_away_with_its_guard() {
        let scratch = Scratch::create("TODO", "hi").unwrap();
        let (dir, file) = (scratch.dir.clone(), scratch.file.clone());
        assert!(file.to_string_lossy().ends_with("TODO.canvas.md"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi");
        drop(scratch);
        assert!(!dir.exists());
    }
}
