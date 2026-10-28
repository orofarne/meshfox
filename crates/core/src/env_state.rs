//! `meshfox env`: lists and removes the environments meshfox builds for a
//! canvas — `.meshfox/<canvas file>.venv` (`@python_venv`) and
//! `.meshfox/<canvas file>.node` (`@node_env`), plus the named ones inside
//! them (`.named/<name>`). Unlike [`crate::prune`], which only takes the
//! environments of canvases that are gone, this removes the ones of a canvas
//! that still exists — what a system Python upgrade calls for. Everything
//! here is rebuilt by running the environment's setup block again, which is
//! what makes removing it safe. Run history and variable caches are never
//! touched.

use std::io;
use std::path::{Path, PathBuf};

use crate::builtin_interpreter::canvas_state_dir;
use crate::prune;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Python,
    Node,
}

impl Kind {
    /// The suffix of the directory name under `.meshfox/`.
    fn suffix(self) -> &'static str {
        match self {
            Kind::Python => "venv",
            Kind::Node => "node",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Python => "python",
            Kind::Node => "node",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Environment {
    /// The canvas file the environment belongs to (it may no longer exist).
    pub canvas: PathBuf,
    pub kind: Kind,
    /// The `name=` of a named environment; `None` for the canvas's usual one.
    pub name: Option<String>,
    pub path: PathBuf,
    pub bytes: u64,
    pub canvas_exists: bool,
}

/// Directory name inside `.named/` that `remove` accepts: the same shape the
/// `name=` option allows, so a name can never walk out of `.named/`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The environments under `root` (and, when `recursive`, its subdirectories),
/// sorted by canvas, kind and name. Changes nothing.
pub fn list(root: &Path, recursive: bool) -> Vec<Environment> {
    let mut found = Vec::new();
    for dir in prune::directories(root, recursive) {
        let Ok(entries) = std::fs::read_dir(dir.join(".meshfox")) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((canvas_name, kind)) = split_state_name(&name) else {
                continue;
            };
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let canvas = dir.join(canvas_name);
            let canvas_exists = std::fs::symlink_metadata(&canvas).is_ok();
            collect(&entry.path(), &canvas, kind, canvas_exists, &mut found);
        }
    }
    found.sort_by(|a, b| (&a.canvas, a.kind, &a.name).cmp(&(&b.canvas, b.kind, &b.name)));
    found
}

fn split_state_name(name: &str) -> Option<(&str, Kind)> {
    [Kind::Python, Kind::Node].into_iter().find_map(|kind| {
        name.strip_suffix(kind.suffix())
            .and_then(|rest| rest.strip_suffix('.'))
            .filter(|canvas| !canvas.is_empty())
            .map(|canvas| (canvas, kind))
    })
}

/// The usual environment of `dir` (when it holds anything besides `.named`)
/// and each named one.
fn collect(dir: &Path, canvas: &Path, kind: Kind, canvas_exists: bool, out: &mut Vec<Environment>) {
    let named = dir.join(".named");
    let has_own_content = std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy() != ".named")
    });
    if has_own_content {
        out.push(Environment {
            canvas: canvas.to_path_buf(),
            kind,
            name: None,
            path: dir.to_path_buf(),
            bytes: prune::dir_size(dir) - prune::dir_size(&named),
            canvas_exists,
        });
    }
    let Ok(entries) = std::fs::read_dir(&named) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        out.push(Environment {
            canvas: canvas.to_path_buf(),
            kind,
            name: Some(entry.file_name().to_string_lossy().into_owned()),
            path: entry.path(),
            bytes: prune::dir_size(&entry.path()),
            canvas_exists,
        });
    }
}

/// What [`remove`] took out.
#[derive(Debug)]
pub struct Removed {
    pub path: PathBuf,
    pub bytes: u64,
}

/// Removes the environments of `canvas`: with `name`, only that named one;
/// without, the whole usual environment together with every named one.
/// `kinds` limits it to python and/or node environments. Refuses while a
/// worker is running for the canvas, since it may be using the environment,
/// and when there is nothing to remove. A symlink is never followed.
pub fn remove(canvas: &Path, name: Option<&str>, kinds: &[Kind]) -> io::Result<Vec<Removed>> {
    let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidInput, msg);
    if let Some(name) = name {
        if !valid_name(name) {
            return Err(invalid(format!("{name:?} isn't a valid environment name")));
        }
    }
    if crate::worker_lock::is_held(canvas) {
        return Err(invalid(format!(
            "a worker is running for {} — stop it first",
            canvas.display()
        )));
    }
    let mut removed = Vec::new();
    for kind in kinds {
        let base = canvas_state_dir(canvas, kind.suffix());
        let target = match name {
            Some(name) => base.join(".named").join(name),
            None => base,
        };
        let Ok(meta) = std::fs::symlink_metadata(&target) else {
            continue;
        };
        if !meta.is_dir() {
            return Err(invalid(format!(
                "won't remove {}: not an environment directory",
                target.display()
            )));
        }
        let bytes = prune::dir_size(&target);
        std::fs::remove_dir_all(&target)?;
        removed.push(Removed {
            path: target,
            bytes,
        });
    }
    if removed.is_empty() {
        return Err(invalid(format!(
            "{} has no {}environment to remove",
            canvas.display(),
            name.map(|n| format!("{n:?} ")).unwrap_or_default()
        )));
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-env-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup(dir: &Path) -> PathBuf {
        let canvas = dir.join("a.canvas.md");
        std::fs::write(&canvas, "<!-- meshfox:canvas -->\n# Root\n").unwrap();
        let state = dir.join(".meshfox");
        std::fs::create_dir_all(state.join("a.canvas.md.venv/bin")).unwrap();
        std::fs::write(state.join("a.canvas.md.venv/bin/python3"), "123").unwrap();
        std::fs::create_dir_all(state.join("a.canvas.md.venv/.named/xls")).unwrap();
        std::fs::write(state.join("a.canvas.md.venv/.named/xls/f"), "12").unwrap();
        std::fs::create_dir_all(state.join("a.canvas.md.node/.named/web")).unwrap();
        std::fs::write(state.join("a.canvas.md.session.sqlite3"), "history").unwrap();
        std::fs::write(state.join("a.canvas.md.env"), "X=1").unwrap();
        canvas
    }

    #[test]
    fn lists_usual_and_named_environments_of_a_live_canvas() {
        let dir = temp_dir("list");
        setup(&dir);
        let found = list(&dir, false);
        let shown: Vec<_> = found
            .iter()
            .map(|e| (e.kind.label(), e.name.clone(), e.bytes, e.canvas_exists))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("python", None, 3, true),
                ("python", Some("xls".to_string()), 2, true),
                ("node", Some("web".to_string()), 0, true),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_a_named_environment_keeps_the_rest() {
        let dir = temp_dir("named");
        let canvas = setup(&dir);
        let removed = remove(&canvas, Some("xls"), &[Kind::Python, Kind::Node]).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!dir.join(".meshfox/a.canvas.md.venv/.named/xls").exists());
        assert!(dir.join(".meshfox/a.canvas.md.venv/bin/python3").exists());
        assert!(dir.join(".meshfox/a.canvas.md.node/.named/web").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_without_a_name_takes_the_whole_environment_but_not_history() {
        let dir = temp_dir("whole");
        let canvas = setup(&dir);
        remove(&canvas, None, &[Kind::Python]).unwrap();
        assert!(!dir.join(".meshfox/a.canvas.md.venv").exists());
        assert!(dir.join(".meshfox/a.canvas.md.node").exists());
        assert!(dir.join(".meshfox/a.canvas.md.session.sqlite3").exists());
        assert!(dir.join(".meshfox/a.canvas.md.env").exists());
        let err = remove(&canvas, None, &[Kind::Python]).unwrap_err();
        assert!(err.to_string().contains("no environment"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_cannot_leave_the_named_directory() {
        let dir = temp_dir("escape");
        let canvas = setup(&dir);
        for bad in ["..", "../x", "a/b", ""] {
            assert!(
                remove(&canvas, Some(bad), &[Kind::Python]).is_err(),
                "{bad}"
            );
        }
        assert!(dir.join(".meshfox/a.canvas.md.venv/bin/python3").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
