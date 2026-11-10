//! `meshfox mv`: moves a canvas file, or a whole directory of canvases, and
//! everything meshfox keeps for them, so nothing is lost on the way.
//!
//! What a plain `mv` gets wrong, and this does not:
//!
//! - **State beside a renamed file.** `.meshfox/<canvas file>.env` (variable
//!   cache) and `.meshfox/<canvas file>.session.sqlite3` (run history, with
//!   its `-wal`/`-shm` companions) are keyed by the canvas's *file name*. A
//!   file move renames them with it. A directory move takes the whole
//!   `.meshfox/` along, and the names inside do not change.
//! - **Secrets in the system store.** With `secret_store = "keychain"` a
//!   saved answer is filed under `doc:<absolute canvas path>` (and an
//!   `[[env]]` section's under `project:<directory>`), so a move leaves it
//!   behind under a path that no longer exists. The values are copied to the
//!   new scope first, the files move, and only then are the old ones
//!   deleted; a move that fails puts the new ones away again. With the
//!   default plaintext store the answers are in the `.env` file and just move
//!   with it.
//! - **Environments.** `.meshfox/<canvas>.venv` and `.node` hold absolute
//!   paths (shebangs, launchers), so a moved copy would be broken. They are
//!   regenerable by definition (see [`crate::prune`]), so they are deleted
//!   rather than moved and the next run of the setup block builds them again.
//!
//! Refused: a destination that exists (nothing is overwritten, and unlike
//! `mv(1)` an existing directory is not a place to move *into*), a
//! destination inside the source, and any canvas with a running worker (a
//! worker holds its file and its session database open). Like `meshfox
//! prune` and `meshfox env` this runs in the CLI's own process: with no worker
//! running there is none to ask, and a move spans files no worker owns.
//!
//! References to the moved canvas from *other* canvases are only reported
//! (see [`Plan::incoming`]), as are relative links inside a moved file that
//! now resolve from another directory ([`Plan::outgoing`]); nothing in
//! another file is rewritten.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::canvas_index::{CanvasIndex, ScanOptions};
use crate::secret_index::SecretIndex;
use crate::secret_store::{self, SecretBackend};
use crate::worker_lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

/// One saved secret whose scope names a path that is about to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMove {
    pub name: String,
    pub from_scope: String,
    pub to_scope: String,
}

/// A line of a canvas that mentions a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub canvas: PathBuf,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

#[derive(Debug)]
pub struct Plan {
    pub kind: Kind,
    /// Canonical.
    pub src: PathBuf,
    /// The canonical parent plus the name asked for.
    pub dst: PathBuf,
    /// File moves only: `.meshfox/` files that follow a renamed canvas.
    pub state_moves: Vec<(PathBuf, PathBuf)>,
    /// Environment directories deleted instead of moved (source-side paths).
    pub dropped: Vec<PathBuf>,
    /// File moves only: the old name's `.worker.lock`, which is only
    /// discovery metadata and is not carried over.
    pub stale_locks: Vec<PathBuf>,
    pub secrets: Vec<SecretMove>,
    /// Canvases (source-side paths) that have a running worker.
    pub busy: Vec<PathBuf>,
    /// Lines of other canvases under the source's parent directory that look
    /// like they refer to the source.
    pub incoming: Vec<Mention>,
    /// File moves into another directory: relative links of the moved canvas.
    pub outgoing: Vec<Mention>,
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Works out what moving `src` to `dst` involves, changing nothing.
/// `index` is where saved secrets are looked up (`None`: there are none).
pub fn plan(src: &Path, dst: &Path, index: Option<&SecretIndex>) -> io::Result<Plan> {
    let src_canon = src
        .canonicalize()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", src.display())))?;
    let kind = if src_canon.is_dir() {
        Kind::Directory
    } else if src_canon.is_file() {
        Kind::File
    } else {
        return Err(invalid(format!(
            "{} is neither a file nor a directory",
            src.display()
        )));
    };

    let dst_abs = if dst.is_absolute() {
        dst.to_path_buf()
    } else {
        std::env::current_dir()?.join(dst)
    };
    let name = dst_abs
        .file_name()
        .ok_or_else(|| invalid(format!("{} has no file name", dst.display())))?
        .to_owned();
    let parent = dst_abs
        .parent()
        .ok_or_else(|| invalid(format!("{} has no parent directory", dst.display())))?;
    let parent = parent.canonicalize().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("destination directory {}: {e}", parent.display()),
        )
    })?;
    if !parent.is_dir() {
        return Err(invalid(format!("{} isn't a directory", parent.display())));
    }
    let dst_canon = parent.join(name);
    if std::fs::symlink_metadata(&dst_canon).is_ok() {
        return Err(invalid(format!(
            "{} already exists — nothing is overwritten",
            dst_canon.display()
        )));
    }
    if dst_canon == src_canon || dst_canon.starts_with(&src_canon) {
        return Err(invalid(format!(
            "can't move {} into itself",
            src_canon.display()
        )));
    }

    let mut plan = Plan {
        kind,
        src: src_canon,
        dst: dst_canon,
        state_moves: Vec::new(),
        dropped: Vec::new(),
        stale_locks: Vec::new(),
        secrets: Vec::new(),
        busy: Vec::new(),
        incoming: Vec::new(),
        outgoing: Vec::new(),
    };
    match kind {
        Kind::File => plan_file(&mut plan)?,
        Kind::Directory => plan_directory(&mut plan)?,
    }
    plan_secrets(&mut plan, index)?;
    plan.incoming = find_incoming(&plan)?;
    Ok(plan)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn plan_file(plan: &mut Plan) -> io::Result<()> {
    if worker_lock::is_held(&plan.src) {
        plan.busy.push(plan.src.clone());
    }
    let old = file_name(&plan.src);
    let new = file_name(&plan.dst);
    let from_dir = plan.src.parent().unwrap_or(Path::new("/")).join(".meshfox");
    let to_dir = plan.dst.parent().unwrap_or(Path::new("/")).join(".meshfox");
    if let Ok(entries) = std::fs::read_dir(&from_dir) {
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for entry in names {
            let Some(rest) = entry.strip_prefix(&old) else {
                continue;
            };
            let from = from_dir.join(&entry);
            if rest == ".env" || rest.starts_with(".session.sqlite3") {
                let to = to_dir.join(format!("{new}{rest}"));
                if std::fs::symlink_metadata(&to).is_ok() {
                    return Err(invalid(format!(
                        "{} already exists — nothing is overwritten",
                        to.display()
                    )));
                }
                plan.state_moves.push((from, to));
            } else if rest == ".venv" || rest == ".node" {
                plan.dropped.push(from);
            } else if rest == ".worker.lock" {
                plan.stale_locks.push(from);
            }
        }
    }
    if plan.src.parent() != plan.dst.parent() {
        plan.outgoing = relative_links(&plan.src);
    }
    Ok(())
}

fn plan_directory(plan: &mut Plan) -> io::Result<()> {
    let index = CanvasIndex::scan(&plan.src, &ScanOptions::default())?;
    if index.truncated {
        return Err(invalid(format!(
            "{} holds too many files to scan for running workers",
            plan.src.display()
        )));
    }
    for entry in index.entries() {
        let canvas = plan.src.join(&entry.rel);
        if worker_lock::is_held(&canvas) {
            plan.busy.push(canvas.clone());
        }
        let name = file_name(&canvas);
        let state = canvas.parent().unwrap_or(&plan.src).join(".meshfox");
        for suffix in [".venv", ".node"] {
            let dir = state.join(format!("{name}{suffix}"));
            if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
                plan.dropped.push(dir);
            }
        }
    }
    Ok(())
}

/// Index entries whose scope names a path under the source, with the scope
/// they get at the destination.
fn plan_secrets(plan: &mut Plan, index: Option<&SecretIndex>) -> io::Result<()> {
    let Some(index) = index else {
        return Ok(());
    };
    for entry in index.entries()? {
        let Some((prefix, path)) = entry.scope.split_once(':') else {
            continue;
        };
        let path = Path::new(path);
        let relative = match (plan.kind, prefix) {
            // One file: only that canvas's own saved answers.
            (Kind::File, "doc") if path == plan.src => PathBuf::new(),
            (Kind::Directory, "doc" | "project") => match path.strip_prefix(&plan.src) {
                Ok(rest) => rest.to_path_buf(),
                Err(_) => continue,
            },
            _ => continue,
        };
        plan.secrets.push(SecretMove {
            name: entry.name,
            from_scope: entry.scope.clone(),
            to_scope: format!("{prefix}:{}", join_nonempty(&plan.dst, &relative).display()),
        });
    }
    Ok(())
}

/// `base.join(rest)`, except that an empty `rest` leaves `base` as it is
/// (`Path::join("")` would add a trailing separator).
fn join_nonempty(base: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

fn find_incoming(plan: &Plan) -> io::Result<Vec<Mention>> {
    let Some(root) = plan.src.parent() else {
        return Ok(Vec::new());
    };
    let index = CanvasIndex::scan(root, &ScanOptions::default())?;
    let mut found = Vec::new();
    for entry in index.entries() {
        let canvas = root.join(&entry.rel);
        if canvas.starts_with(&plan.src) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&canvas) else {
            continue;
        };
        let from_dir = canvas.parent().unwrap_or(root);
        let needle = relative_path(from_dir, &plan.src);
        for (n, line) in text.lines().enumerate() {
            if mentions(line, &needle, plan.kind == Kind::Directory) {
                found.push(Mention {
                    canvas: canvas.clone(),
                    line: n + 1,
                    text: shorten(line),
                });
            }
        }
    }
    Ok(found)
}

/// `to` as a `/`-separated path relative to the directory `from`; both
/// absolute.
fn relative_path(from: &Path, to: &Path) -> String {
    let from: Vec<Component> = from.components().collect();
    let to_parts: Vec<Component> = to.components().collect();
    let common = from
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to_parts[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Whether `line` holds `needle` as a whole path: not glued to more name
/// characters before it, and (for a directory) followed by `/`, since a
/// directory's bare name is mostly just a word.
fn mentions(line: &str, needle: &str, directory: bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(at) = line[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        let before_ok = line[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_name_char(c));
        let after = line[end..].chars().next();
        let after_ok = if directory {
            after == Some('/')
        } else {
            after.is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-')))
        };
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// `](target)` links of the canvas that point at a local relative path.
fn relative_links(canvas: &Path) -> Vec<Mention> {
    let Ok(text) = std::fs::read_to_string(canvas) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("](") {
            rest = &rest[at + 2..];
            let Some(close) = rest.find(')') else {
                break;
            };
            let target = rest[..close].trim();
            rest = &rest[close..];
            let local = !target.is_empty()
                && !target.starts_with('#')
                && !target.starts_with('/')
                && !target.contains("://")
                && !target.starts_with("mailto:");
            if local {
                found.push(Mention {
                    canvas: canvas.to_path_buf(),
                    line: n + 1,
                    text: shorten(line),
                });
                break;
            }
        }
    }
    found
}

fn shorten(line: &str) -> String {
    const MAX: usize = 160;
    let line = line.trim();
    if line.chars().count() <= MAX {
        line.to_string()
    } else {
        let cut: String = line.chars().take(MAX).collect();
        format!("{cut}…")
    }
}

/// Carries out a [`Plan`]. Returns warnings: things that did not go as
/// planned but leave the move itself complete (an environment that could not
/// be deleted, an old secret that could not be removed). An `Err` means
/// nothing was moved: whatever had been done is undone first.
///
/// `index` and `backend` are only used (and then required) when the plan
/// moves secrets.
pub fn apply(
    plan: &Plan,
    index: Option<&SecretIndex>,
    backend: Option<&dyn SecretBackend>,
) -> io::Result<Vec<String>> {
    if !plan.busy.is_empty() {
        let list: Vec<String> = plan.busy.iter().map(|p| p.display().to_string()).collect();
        return Err(invalid(format!(
            "a worker is running for {} — stop it first (`meshfox cores kill`)",
            list.join(", ")
        )));
    }
    let store = match (plan.secrets.is_empty(), index, backend) {
        (true, _, _) => None,
        (false, Some(index), Some(backend)) => Some((index, backend)),
        (false, _, _) => return Err(invalid("saved secrets need the secret store".to_string())),
    };

    // 1. Copy the secrets to their new scope; the old ones stay until the
    //    files have moved.
    let mut copied: Vec<&SecretMove> = Vec::new();
    let mut leftovers: Vec<&SecretMove> = Vec::new();
    if let Some((index, backend)) = store {
        for item in &plan.secrets {
            match copy_secret(index, backend, item) {
                Ok(true) => {
                    copied.push(item);
                    leftovers.push(item);
                }
                Ok(false) => leftovers.push(item),
                Err(e) => {
                    undo_secrets(index, backend, &copied);
                    return Err(io::Error::new(
                        e.kind(),
                        format!("copying secret {}: {e}", item.name),
                    ));
                }
            }
        }
    }

    // 2. The files.
    if let Err(e) = move_files(plan) {
        if let Some((index, backend)) = store {
            undo_secrets(index, backend, &copied);
        }
        return Err(e);
    }

    // 3. What the move no longer needs.
    let mut warnings = Vec::new();
    if let Some((index, backend)) = store {
        for item in leftovers {
            let removed = backend
                .delete(&secret_store::account_for(&item.from_scope, &item.name))
                .and_then(|_| index.forget(&item.from_scope, &item.name));
            if let Err(e) = removed {
                warnings.push(format!(
                    "couldn't remove the old secret {} ({}): {e}",
                    item.name, item.from_scope
                ));
            }
        }
    }
    for dir in &plan.dropped {
        let target = match plan.kind {
            Kind::File => dir.clone(),
            Kind::Directory => plan.dst.join(dir.strip_prefix(&plan.src).unwrap_or(dir)),
        };
        if let Err(e) = remove_environment(&target) {
            warnings.push(format!("couldn't delete {}: {e}", target.display()));
        }
    }
    for lock in &plan.stale_locks {
        if let Err(e) = std::fs::remove_file(lock) {
            warnings.push(format!("couldn't delete {}: {e}", lock.display()));
        }
    }
    Ok(warnings)
}

/// `Ok(true)` when a value was copied, `Ok(false)` when the store had none
/// (only the stale index row is left to remove).
fn copy_secret(
    index: &SecretIndex,
    backend: &dyn SecretBackend,
    item: &SecretMove,
) -> io::Result<bool> {
    let Some(value) = backend.get(&secret_store::account_for(&item.from_scope, &item.name))? else {
        return Ok(false);
    };
    backend.set(
        &secret_store::account_for(&item.to_scope, &item.name),
        &value,
    )?;
    index.record(&item.to_scope, &item.name)?;
    Ok(true)
}

fn undo_secrets(index: &SecretIndex, backend: &dyn SecretBackend, copied: &[&SecretMove]) {
    for item in copied {
        let _ = backend.delete(&secret_store::account_for(&item.to_scope, &item.name));
        let _ = index.forget(&item.to_scope, &item.name);
    }
}

fn move_files(plan: &Plan) -> io::Result<()> {
    move_path(&plan.src, &plan.dst)?;
    if plan.state_moves.is_empty() {
        return Ok(());
    }
    let mut done: Vec<&(PathBuf, PathBuf)> = Vec::new();
    let result: io::Result<()> = (|| {
        if let Some(dir) = plan.state_moves[0].1.parent() {
            std::fs::create_dir_all(dir)?;
        }
        for pair in &plan.state_moves {
            move_path(&pair.0, &pair.1)?;
            done.push(pair);
        }
        Ok(())
    })();
    if result.is_err() {
        for (from, to) in done.into_iter().rev() {
            let _ = move_path(to, from);
        }
        let _ = move_path(&plan.dst, &plan.src);
    }
    result
}

/// `rename`, falling back to copy-and-delete across filesystems.
fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            if let Err(e) = copy_tree(from, to) {
                let _ = remove_tree(to);
                return Err(e);
            }
            remove_tree(from)
        }
        other => other,
    }
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if meta.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        std::fs::set_permissions(to, meta.permissions())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn remove_tree(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Deletes an environment directory, never following a symlink.
fn remove_environment(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(dir),
        Ok(_) => Err(invalid("not an environment directory".to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::MemoryBackend;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!(
                "meshfox-relocate-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ))
            .to_string_lossy()
            .replace(['(', ')'], "");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        PathBuf::from(dir).canonicalize().unwrap()
    }

    fn canvas(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("<!-- meshfox:canvas -->\n# Root\n{body}")).unwrap();
    }

    fn state(dir: &Path, canvas_name: &str) {
        let meshfox = dir.join(".meshfox");
        std::fs::create_dir_all(meshfox.join(format!("{canvas_name}.venv/bin"))).unwrap();
        std::fs::write(meshfox.join(format!("{canvas_name}.env")), "A=1").unwrap();
        std::fs::write(
            meshfox.join(format!("{canvas_name}.session.sqlite3")),
            "history",
        )
        .unwrap();
        std::fs::write(
            meshfox.join(format!("{canvas_name}.session.sqlite3-wal")),
            "w",
        )
        .unwrap();
        std::fs::write(meshfox.join(format!("{canvas_name}.worker.lock")), "").unwrap();
    }

    fn secret(index: &SecretIndex, backend: &MemoryBackend, scope: &str, name: &str, value: &str) {
        backend
            .set(&secret_store::account_for(scope, name), value)
            .unwrap();
        index.record(scope, name).unwrap();
    }

    fn has(backend: &MemoryBackend, scope: &str, name: &str) -> Option<String> {
        backend
            .get(&secret_store::account_for(scope, name))
            .unwrap()
    }

    #[test]
    fn a_file_move_renames_its_state_and_drops_environments() {
        let dir = tempdir("file");
        canvas(&dir.join("a.canvas.md"), "");
        canvas(&dir.join("a.canvas.md.bak"), "");
        state(&dir, "a.canvas.md");
        std::fs::write(dir.join(".meshfox/a.canvas.md.bak.env"), "B=1").unwrap();

        let plan = plan(&dir.join("a.canvas.md"), &dir.join("b.canvas.md"), None).unwrap();
        let warnings = apply(&plan, None, None).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        assert!(!dir.join("a.canvas.md").exists());
        assert!(dir.join("b.canvas.md").exists());
        let meshfox = dir.join(".meshfox");
        assert_eq!(
            std::fs::read_to_string(meshfox.join("b.canvas.md.env")).unwrap(),
            "A=1"
        );
        assert!(meshfox.join("b.canvas.md.session.sqlite3").exists());
        assert!(meshfox.join("b.canvas.md.session.sqlite3-wal").exists());
        assert!(!meshfox.join("a.canvas.md.env").exists());
        assert!(!meshfox.join("a.canvas.md.venv").exists());
        assert!(!meshfox.join("b.canvas.md.venv").exists());
        assert!(!meshfox.join("a.canvas.md.worker.lock").exists());
        // Another canvas whose name only starts with this one's is untouched.
        assert!(dir.join("a.canvas.md.bak").exists());
        assert!(meshfox.join("a.canvas.md.bak.env").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_can_move_into_another_directory_and_its_secrets_follow() {
        let dir = tempdir("cross");
        let src = dir.join("a.canvas.md");
        canvas(&src, "");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let index = SecretIndex::at(dir.join("idx.sqlite3"));
        let backend = MemoryBackend::new();
        let old_scope = format!("doc:{}", src.display());
        secret(&index, &backend, &old_scope, "TOKEN", "s3cret");
        secret(&index, &backend, "global", "OTHER", "x");

        let plan = plan(&src, &dir.join("sub/c.canvas.md"), Some(&index)).unwrap();
        assert_eq!(plan.secrets.len(), 1);
        apply(&plan, Some(&index), Some(&backend)).unwrap();

        let new_scope = format!("doc:{}", dir.join("sub/c.canvas.md").display());
        assert_eq!(
            has(&backend, &new_scope, "TOKEN").as_deref(),
            Some("s3cret")
        );
        assert_eq!(has(&backend, &old_scope, "TOKEN"), None);
        assert_eq!(has(&backend, "global", "OTHER").as_deref(), Some("x"));
        let scopes: Vec<String> = index
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| e.scope)
            .collect();
        assert!(
            scopes.contains(&new_scope) && !scopes.contains(&old_scope),
            "{scopes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_move_keeps_state_and_rescopes_doc_and_project_secrets() {
        let dir = tempdir("dir");
        let old = dir.join("old");
        canvas(&old.join("a.canvas.md"), "");
        canvas(&old.join("deep/b.canvas.md"), "");
        state(&old, "a.canvas.md");
        state(&old.join("deep"), "b.canvas.md");
        let index = SecretIndex::at(dir.join("idx.sqlite3"));
        let backend = MemoryBackend::new();
        let doc = format!("doc:{}", old.join("deep/b.canvas.md").display());
        let project = format!("project:{}", old.display());
        secret(&index, &backend, &doc, "A", "1");
        secret(&index, &backend, &project, "B", "2");
        secret(
            &index,
            &backend,
            &format!("doc:{}", dir.join("elsewhere.md").display()),
            "C",
            "3",
        );

        let plan = plan(&old, &dir.join("new"), Some(&index)).unwrap();
        assert_eq!(plan.dropped.len(), 2);
        assert_eq!(plan.secrets.len(), 2);
        assert!(apply(&plan, Some(&index), Some(&backend))
            .unwrap()
            .is_empty());

        let new = dir.join("new");
        assert!(!old.exists());
        assert!(new.join(".meshfox/a.canvas.md.env").exists());
        assert!(new
            .join("deep/.meshfox/b.canvas.md.session.sqlite3")
            .exists());
        assert!(!new.join(".meshfox/a.canvas.md.venv").exists());
        assert!(!new.join("deep/.meshfox/b.canvas.md.venv").exists());
        let moved_doc = format!("doc:{}", new.join("deep/b.canvas.md").display());
        let moved_project = format!("project:{}", new.display());
        assert_eq!(has(&backend, &moved_doc, "A").as_deref(), Some("1"));
        assert_eq!(has(&backend, &moved_project, "B").as_deref(), Some("2"));
        assert_eq!(has(&backend, &doc, "A"), None);
        assert_eq!(has(&backend, &project, "B"), None);
        assert_eq!(index.entries().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_an_existing_destination_and_a_move_into_itself() {
        let dir = tempdir("refuse");
        canvas(&dir.join("a.canvas.md"), "");
        canvas(&dir.join("b.canvas.md"), "");
        std::fs::create_dir_all(dir.join("d/inner")).unwrap();
        std::fs::create_dir_all(dir.join("e")).unwrap();

        let e = plan(&dir.join("a.canvas.md"), &dir.join("b.canvas.md"), None).unwrap_err();
        assert!(e.to_string().contains("already exists"), "{e}");
        let e = plan(&dir.join("a.canvas.md"), &dir.join("e"), None).unwrap_err();
        assert!(e.to_string().contains("already exists"), "{e}");
        let e = plan(&dir.join("d"), &dir.join("d/inner/d2"), None).unwrap_err();
        assert!(e.to_string().contains("into itself"), "{e}");
        let e = plan(&dir.join("nope.canvas.md"), &dir.join("x.canvas.md"), None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        let e = plan(
            &dir.join("a.canvas.md"),
            &dir.join("missing/x.canvas.md"),
            None,
        )
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(dir.join("a.canvas.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_running_worker_blocks_the_move() {
        let dir = tempdir("busy");
        let src = dir.join("a.canvas.md");
        canvas(&src, "");
        let guard = match worker_lock::try_acquire(&src).unwrap() {
            worker_lock::Acquired::Us(guard) => guard,
            worker_lock::Acquired::Other { .. } => panic!("somebody else holds the lock"),
        };
        assert!(worker_lock::is_held(&src));
        let plan = plan(&src, &dir.join("b.canvas.md"), None).unwrap();
        assert_eq!(plan.busy, vec![src.clone()]);
        let e = apply(&plan, None, None).unwrap_err();
        assert!(e.to_string().contains("worker is running"), "{e}");
        assert!(src.exists() && !dir.join("b.canvas.md").exists());
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_file_move_puts_secrets_back() {
        let dir = tempdir("rollback");
        let src = dir.join("a.canvas.md");
        canvas(&src, "");
        let index = SecretIndex::at(dir.join("idx.sqlite3"));
        let backend = MemoryBackend::new();
        let old_scope = format!("doc:{}", src.display());
        secret(&index, &backend, &old_scope, "TOKEN", "v");

        let plan = plan(&src, &dir.join("b.canvas.md"), Some(&index)).unwrap();
        // The source disappears between planning and applying.
        std::fs::remove_file(&src).unwrap();
        assert!(apply(&plan, Some(&index), Some(&backend)).is_err());

        assert_eq!(has(&backend, &old_scope, "TOKEN").as_deref(), Some("v"));
        let new_scope = format!("doc:{}", dir.join("b.canvas.md").display());
        assert_eq!(has(&backend, &new_scope, "TOKEN"), None);
        let scopes: Vec<String> = index
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| e.scope)
            .collect();
        assert_eq!(scopes, vec![old_scope]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_canvases_that_refer_to_the_source_are_reported() {
        let dir = tempdir("refs");
        canvas(&dir.join("a.canvas.md"), "");
        canvas(
            &dir.join("sub/user.canvas.md"),
            "\nSee [it](../a.canvas.md#root) and not-a.canvas.md.\n",
        );
        canvas(
            &dir.join("sibling.canvas.md"),
            "\nInclude ./a.canvas.md here.\n",
        );
        canvas(&dir.join("unrelated.canvas.md"), "\nNothing about it.\n");

        let plan = plan(&dir.join("a.canvas.md"), &dir.join("z.canvas.md"), None).unwrap();
        let places: Vec<(String, usize)> = plan
            .incoming
            .iter()
            .map(|m| (file_name(&m.canvas), m.line))
            .collect();
        assert_eq!(
            places,
            vec![
                ("sibling.canvas.md".to_string(), 4),
                ("user.canvas.md".to_string(), 4)
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_is_mentioned_only_through_a_path_into_it() {
        let dir = tempdir("dirrefs");
        canvas(&dir.join("docs/a.canvas.md"), "");
        canvas(
            &dir.join("top.canvas.md"),
            "\nSee [a](docs/a.canvas.md). The docs are good.\n",
        );
        canvas(&dir.join("prose.canvas.md"), "\nRead the docs.\n");

        let plan = plan(&dir.join("docs"), &dir.join("manual"), None).unwrap();
        assert_eq!(plan.incoming.len(), 1);
        assert_eq!(file_name(&plan.incoming[0].canvas), "top.canvas.md");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_links_are_listed_when_a_file_changes_directory() {
        let dir = tempdir("outgoing");
        canvas(
            &dir.join("a.canvas.md"),
            "\n[data](data.csv)\n[web](https://example.com)\n[top](#root)\n![i](img/x.png)\n",
        );
        std::fs::create_dir_all(dir.join("sub")).unwrap();

        let moved = plan(&dir.join("a.canvas.md"), &dir.join("sub/a.canvas.md"), None).unwrap();
        let lines: Vec<usize> = moved.outgoing.iter().map(|m| m.line).collect();
        assert_eq!(lines, vec![4, 7]);
        let renamed = plan(&dir.join("a.canvas.md"), &dir.join("b.canvas.md"), None).unwrap();
        assert!(renamed.outgoing.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_paths_climb_out_of_the_starting_directory() {
        let from = Path::new("/w/x/y");
        assert_eq!(relative_path(from, Path::new("/w/x/a.md")), "../a.md");
        assert_eq!(relative_path(from, Path::new("/w/x/y/a.md")), "a.md");
        assert_eq!(relative_path(from, Path::new("/w/q/a.md")), "../../q/a.md");
    }
}
