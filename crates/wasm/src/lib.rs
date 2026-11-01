//! WebAssembly binding for the meshfox document model.
//!
//! Hands a browser or Node the same read pipeline `GET /api/canvas` runs,
//! without a server: raw `.canvas.md` text in, the fully annotated canvas
//! (as JSON) out. Everything the model needs from outside comes through two
//! seams this crate wires up to JavaScript:
//!
//! * **files** — `include` targets, `file` nodes read by a constraint, the
//!   content digest of runnable files. The host passes an object with a
//!   `read(path)` method (or just a function) returning a `Uint8Array`, a
//!   `string`, or `null`/`undefined` for "no such file".
//! * **the clock and randomness** — installed once at module start-up
//!   (`Date.now()` and `crypto.getRandomValues`, through `getrandom`).
//!
//! Paths the host sees are *virtual*: relative to the canvas's own
//! directory, `/`-separated, never escaping it.

use meshfox_model::file_source::{ConfineError, FileSource};
use meshfox_model::ignore_rules::IgnoreRules;
use meshfox_model::undo::{MemoryStorage, Payload, UndoLog};
use meshfox_model::{host, render};
use std::path::{Component, Path, PathBuf};
use wasm_bindgen::prelude::*;

/// Runs once when the module is instantiated.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    host::set_clock(now_nanos);
    host::set_entropy(entropy);
}

fn now_nanos() -> i128 {
    (js_sys::Date::now() * 1_000_000.0) as i128
}

fn entropy() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the host provides random bytes");
    u64::from_le_bytes(bytes)
}

/// The version of this build, for a host that wants to show or check it.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The current time as the model stamps it (`createdAt`/`updatedAt`, UTC
/// RFC3339), read through the clock installed at start-up. Mostly a probe
/// that the host wiring works: without it the model would trap on first use.
#[wasm_bindgen(js_name = hostNow)]
pub fn host_now() -> String {
    meshfox_model::timestamp::now_utc_rfc3339()
}

/// Renders `source`, the text of the canvas at `path` (only used to resolve
/// relative targets, e.g. `doc.canvas.md` or `notes/todo.canvas.md`), and
/// returns the annotated canvas as a JSON string — the shape
/// `GET /api/canvas` serves.
///
/// `files` is how the host supplies everything else; pass `undefined` for a
/// self-contained document. Errors (a canvas that does not parse, an
/// `include` that cannot be read) are thrown as `Error`.
#[wasm_bindgen(js_name = renderCanvas)]
pub fn render_canvas(source: &str, path: &str, files: JsValue) -> Result<String, JsError> {
    let provider = Provider::from_js(&files)?;
    let canvas = render::render_canvas(source, Path::new(path), &provider)
        .map_err(|e| JsError::new(&e.to_string()))?;
    serde_json::to_string(&canvas).map_err(|e| JsError::new(&e.to_string()))
}

/// Like `renderCanvas`, for a host that may hold only some of the files: an
/// `include` that cannot be read does not fail the document. Its node shows a
/// notice instead of the content, and the result is JSON
/// `{ "canvas": <the canvas>, "warnings": [{ "nodeId", "target", "reason" }] }`.
/// A document that does not parse is still an error.
#[wasm_bindgen(js_name = renderCanvasWithWarnings)]
pub fn render_canvas_with_warnings(
    source: &str,
    path: &str,
    files: JsValue,
) -> Result<String, JsError> {
    let provider = Provider::from_js(&files)?;
    let (canvas, warnings) = render::render_canvas_lenient(source, Path::new(path), &provider)
        .map_err(|e| JsError::new(&e.to_string()))?;
    serde_json::to_string(&serde_json::json!({ "canvas": canvas, "warnings": warnings }))
        .map_err(|e| JsError::new(&e.to_string()))
}

/// Lexically resolves `target` against `dir` and refuses anything that
/// leaves `dir`: the host's files are a virtual tree, so there is no disk to
/// canonicalize against, only `.`/`..` to fold. A `..` may undo what the
/// target itself added but never climbs above `dir` — checked at every step,
/// not just at the end.
fn lexical_confine(dir: &Path, target: &str) -> Result<PathBuf, ConfineError> {
    fn fold(path: &Path, parts: &mut Vec<String>, floor: usize) -> bool {
        for component in path.components() {
            match component {
                Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir => {
                    if parts.len() <= floor {
                        return false;
                    }
                    parts.pop();
                }
                // An absolute path or a drive prefix names something outside
                // the canvas's own tree.
                Component::RootDir | Component::Prefix(_) => return false,
            }
        }
        true
    }
    let outside = || ConfineError::Outside(dir.join(target));
    let mut parts: Vec<String> = Vec::new();
    if !fold(dir, &mut parts, 0) {
        return Err(outside());
    }
    let floor = parts.len();
    if !fold(Path::new(target), &mut parts, floor) {
        return Err(outside());
    }
    Ok(parts.iter().collect())
}

/// How a host supplies file contents.
enum Provider {
    /// No files: every read is "not found".
    None,
    Js {
        read: js_sys::Function,
        this: JsValue,
    },
}

impl Provider {
    fn from_js(files: &JsValue) -> Result<Provider, JsError> {
        if files.is_undefined() || files.is_null() {
            return Ok(Provider::None);
        }
        if let Some(function) = files.dyn_ref::<js_sys::Function>() {
            return Ok(Provider::Js {
                read: function.clone(),
                this: JsValue::UNDEFINED,
            });
        }
        let read = js_sys::Reflect::get(files, &JsValue::from_str("read"))
            .ok()
            .and_then(|v| v.dyn_into::<js_sys::Function>().ok())
            .ok_or_else(|| {
                JsError::new("`files` must be a function or an object with a `read(path)` method")
            })?;
        Ok(Provider::Js {
            read,
            this: files.clone(),
        })
    }
}

impl FileSource for Provider {
    fn confine(&self, dir: &Path, target: &str) -> Result<PathBuf, ConfineError> {
        lexical_confine(dir, target)
    }

    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        use std::io::{Error, ErrorKind};
        let Provider::Js { read, this } = self else {
            return Err(ErrorKind::NotFound.into());
        };
        let virtual_path = path.to_string_lossy().replace('\\', "/");
        let value = read
            .call1(this, &JsValue::from_str(&virtual_path))
            .map_err(|e| Error::other(format!("files.read threw: {e:?}")))?;
        if value.is_undefined() || value.is_null() {
            return Err(ErrorKind::NotFound.into());
        }
        if let Some(text) = value.as_string() {
            return Ok(text.into_bytes());
        }
        if let Some(bytes) = value.dyn_ref::<js_sys::Uint8Array>() {
            return Ok(bytes.to_vec());
        }
        Err(Error::new(
            ErrorKind::InvalidData,
            "files.read must return a Uint8Array, a string, or null",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The same lexical rules over an in-memory map, so the binding's
    /// confinement and the model's include handling are testable natively.
    struct Mem(HashMap<&'static str, &'static str>);

    impl FileSource for Mem {
        fn confine(&self, dir: &Path, target: &str) -> Result<PathBuf, ConfineError> {
            lexical_confine(dir, target)
        }
        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            self.0
                .get(path.to_string_lossy().as_ref())
                .map(|s| s.as_bytes().to_vec())
                .ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }
    }

    fn confine(dir: &str, target: &str) -> Result<String, ConfineError> {
        lexical_confine(Path::new(dir), target).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn folds_dots_and_stays_inside_the_canvas_directory() {
        assert_eq!(confine(".", "./notes.md").unwrap(), "notes.md");
        assert_eq!(confine("", "a/b/../c.md").unwrap(), "a/c.md");
        assert_eq!(confine("docs", "./x/../y.md").unwrap(), "docs/y.md");
    }

    #[test]
    fn refuses_anything_that_leaves_the_directory() {
        for (dir, target) in [
            (".", "../secret.md"),
            ("docs", "../../x.md"),
            (".", "/etc/passwd"),
            ("docs", "a/../../b.md"),
            // Climbing out of `docs` and back into a sibling is still out.
            ("docs", "../other/x.md"),
        ] {
            assert!(
                matches!(confine(dir, target), Err(ConfineError::Outside(_))),
                "{dir:?} + {target:?}"
            );
        }
    }

    #[test]
    fn includes_resolve_through_the_provider_and_missing_ones_error() {
        let files = Mem(HashMap::from([("notes.md", "# Notes\n\nfrom the host\n")]));
        let raw = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Notes\n<!-- meshfox:node id=\"notes\" type=\"include\" -->\n\n[notes](./notes.md)\n",
        );
        let canvas = render::render_canvas(raw, Path::new("doc.canvas.md"), &files).unwrap();
        assert!(canvas.node("notes").unwrap().text.contains("from the host"));

        let empty = Mem(HashMap::new());
        assert!(render::render_canvas(raw, Path::new("doc.canvas.md"), &empty).is_err());
    }
}

/// What meshfox leaves out of a tree it walks (names starting with `.`, and what
/// `.meshfoxignore` files exclude): the same rules as `meshfox serve --http`.
/// A host picking a folder feeds it each `.meshfoxignore` it finds, then asks
/// about every path before reading it. Paths are `/`-separated, relative to
/// the picked folder.
#[wasm_bindgen(js_name = IgnoreRules)]
pub struct JsIgnoreRules(IgnoreRules);

#[wasm_bindgen(js_class = IgnoreRules)]
impl JsIgnoreRules {
    #[wasm_bindgen(constructor)]
    pub fn new() -> JsIgnoreRules {
        JsIgnoreRules(IgnoreRules::new())
    }

    /// Registers the text of the `.meshfoxignore` that sits in `dir` (`""` is
    /// the root).
    #[wasm_bindgen(js_name = addFile)]
    pub fn add_file(&mut self, dir: &str, text: &str) {
        self.0.add(dir, text);
    }

    /// Whether `path`, or a directory above it, is left out.
    #[wasm_bindgen(js_name = isIgnored)]
    pub fn is_ignored(&self, path: &str, is_dir: bool) -> bool {
        self.0.is_ignored(path, is_dir)
    }
}

impl Default for JsIgnoreRules {
    fn default() -> Self {
        Self::new()
    }
}

/// The fingerprint of a text as the server computes it (`body_rev`): what an
/// `ETag` is made of, so two sides agree on "the same document".
#[wasm_bindgen(js_name = bodyRev)]
pub fn body_rev(text: &str) -> String {
    meshfox_model::body_rev::body_rev(text)
}

/// A document's undo/redo history, held in the page: the very same log the
/// server keeps in SQLite (`meshfox_model::undo`), over memory. Every step is
/// a document-wide splice, so it costs the size of the change, and replays
/// exactly. Sequence numbers are small integers, passed as JS numbers.
#[wasm_bindgen(js_name = UndoLog)]
pub struct JsUndoLog(UndoLog<MemoryStorage>);

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

#[wasm_bindgen(js_class = UndoLog)]
impl JsUndoLog {
    /// A log whose starting point is `initial`, the text as opened.
    #[wasm_bindgen(constructor)]
    pub fn new(initial: &str) -> Result<JsUndoLog, JsError> {
        let log = UndoLog::new(MemoryStorage::new());
        log.reconcile_startup_drift(initial).map_err(js_err)?;
        Ok(JsUndoLog(log))
    }

    /// Records that `before` became `after`; a redo tail is dropped.
    pub fn push(&self, op_kind: &str, before: &str, after: &str) -> Result<(), JsError> {
        self.0
            .push(op_kind, Payload::Raw { before }, after)
            .map_err(js_err)
    }

    /// The sequence number of the step the document is at, `0` at the start.
    pub fn cursor(&self) -> Result<f64, JsError> {
        Ok(self.0.cursor().map_err(js_err)? as f64)
    }

    #[wasm_bindgen(js_name = canUndo)]
    pub fn can_undo(&self) -> Result<bool, JsError> {
        self.0.can_undo().map_err(js_err)
    }

    #[wasm_bindgen(js_name = canRedo)]
    pub fn can_redo(&self) -> Result<bool, JsError> {
        self.0.can_redo().map_err(js_err)
    }

    /// Walks to step `seq` from the document `current`; the document there, or
    /// `undefined` when nothing moved (already there, or out of range). A step
    /// that no longer fits `current` is an error, and nothing is moved.
    #[wasm_bindgen(js_name = jumpTo)]
    pub fn jump_to(&self, seq: f64, current: &str) -> Result<Option<String>, JsError> {
        let (raw, moved) = self
            .0
            .jump_to(seq as i64, current.to_string(), |entry, at, undo| {
                entry
                    .replay_document(at, undo)
                    .ok_or_else(|| std::io::Error::other("a step no longer fits the document"))
            })
            .map_err(js_err)?;
        Ok(moved.then_some(raw))
    }

    /// One step back from `current`, if there is one.
    pub fn undo(&self, current: &str) -> Result<Option<String>, JsError> {
        let cursor = self.0.cursor().map_err(js_err)?;
        match self.0.peek_undo().map_err(js_err)? {
            Some(entry) => {
                let target = self.0.cursor_after_undoing(entry.seq).map_err(js_err)?;
                debug_assert!(target < cursor);
                self.jump_to(target as f64, current)
            }
            None => Ok(None),
        }
    }

    /// One step forward from `current`, if there is one.
    pub fn redo(&self, current: &str) -> Result<Option<String>, JsError> {
        match self.0.peek_redo().map_err(js_err)? {
            Some(entry) => self.jump_to(entry.seq as f64, current),
            None => Ok(None),
        }
    }

    /// The last `limit` applied steps and the whole redo tail, most-future
    /// first, as JSON: `[{ "seq", "createdAt", "opKind", "applied" }]`.
    pub fn history(&self, limit: u32) -> Result<String, JsError> {
        let entries: Vec<serde_json::Value> = self
            .0
            .history_around(limit as usize)
            .map_err(js_err)?
            .into_iter()
            .map(|h| {
                serde_json::json!({
                    "seq": h.entry.seq,
                    "createdAt": h.entry.created_at,
                    "opKind": h.entry.op_kind,
                    "applied": h.applied,
                })
            })
            .collect();
        serde_json::to_string(&entries).map_err(js_err)
    }
}

/// The text of a new, empty canvas whose root heading is `title`: the same
/// template `meshfox create` writes.
#[wasm_bindgen(js_name = newCanvasContent)]
pub fn new_canvas_content(title: &str) -> String {
    meshfox_model::mdcanvas::new_canvas_content(title)
}
