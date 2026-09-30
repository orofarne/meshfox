//! Local index of what meshfox has put into the system secret store
//! (`crate::secret_store`). The store itself can't be enumerated portably,
//! so every write records `(scope, name)` here — metadata only, never a
//! value — in `~/.meshfox/secrets.sqlite3`. That's what makes `meshfox
//! secret list`/`prune` (and the MCP `secret_list`) possible.
//!
//! A *scope* is the part of an account before the name: `doc:<canvas path>`,
//! `project:<canvas root>`, `global` or `global:<path= as written>` — see
//! `crate::secret_store`. [`report`] classifies each entry against what's on
//! disk now:
//!
//! - `ok`: the canvas still declares the variable as `secret`, or the config
//!   section still lists it in `secrets = [...]`;
//! - `orphan-decl`: the file is there but no longer declares it;
//! - `orphan-path`: the canvas file / project directory is gone (also what a
//!   rename or move looks like — nothing can tell them apart);
//! - `unknown`: a file that should say couldn't be read or parsed. Never
//!   treated as an orphan, so `prune` can't delete something over a typo.

use std::io;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::secret_store::{self, SecretBackend};

/// `~/.meshfox/secrets.sqlite3`.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".meshfox").join("secrets.sqlite3"))
}

#[derive(Debug, Clone)]
pub struct SecretIndex {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub scope: String,
    pub name: String,
    pub created_at: String,
}

fn sql_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(format!("secret index: {e}"))
}

impl SecretIndex {
    pub fn at(path: PathBuf) -> SecretIndex {
        SecretIndex { path }
    }

    /// The index in `$HOME`, or `None` without one.
    pub fn default_location() -> Option<SecretIndex> {
        default_path().map(SecretIndex::at)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opened per operation: these are rare, small writes, and it keeps the
    /// index free of any shared connection state across processes.
    fn connect(&self) -> io::Result<Connection> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(&self.path).map_err(sql_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_err)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS secrets (
                 scope TEXT NOT NULL,
                 name TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 PRIMARY KEY (scope, name)
             )",
        )
        .map_err(sql_err)?;
        Ok(conn)
    }

    /// Notes that `name` is stored under `scope` (keeps the original
    /// `created_at` when it already was).
    pub fn record(&self, scope: &str, name: &str) -> io::Result<()> {
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(io::Error::other)?;
        self.connect()?
            .execute(
                "INSERT OR IGNORE INTO secrets (scope, name, created_at) VALUES (?1, ?2, ?3)",
                (scope, name, now),
            )
            .map_err(sql_err)?;
        Ok(())
    }

    pub fn forget(&self, scope: &str, name: &str) -> io::Result<()> {
        self.connect()?
            .execute(
                "DELETE FROM secrets WHERE scope = ?1 AND name = ?2",
                (scope, name),
            )
            .map_err(sql_err)?;
        Ok(())
    }

    /// Every entry, ordered by scope then name. An index that was never
    /// written reads as empty and isn't created by looking.
    pub fn entries(&self) -> io::Result<Vec<IndexEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT scope, name, created_at FROM secrets ORDER BY scope, name")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(IndexEntry {
                    scope: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })
            .map_err(sql_err)?;
        rows.collect::<Result<_, _>>().map_err(sql_err)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretStatus {
    Ok,
    OrphanDecl,
    OrphanPath,
    Unknown(String),
}

impl SecretStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretStatus::Ok => "ok",
            SecretStatus::OrphanDecl => "orphan-decl",
            SecretStatus::OrphanPath => "orphan-path",
            SecretStatus::Unknown(_) => "unknown",
        }
    }

    pub fn is_orphan(&self) -> bool {
        matches!(self, SecretStatus::OrphanDecl | SecretStatus::OrphanPath)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReport {
    pub entry: IndexEntry,
    pub status: SecretStatus,
}

/// The filesystem path a scope refers to, for `doc:`/`project:` scopes.
pub fn scope_path(scope: &str) -> Option<&Path> {
    scope
        .strip_prefix("doc:")
        .or_else(|| scope.strip_prefix("project:"))
        .map(Path::new)
}

/// Classifies every index entry against the files as they are now.
pub fn report(index: &SecretIndex) -> io::Result<Vec<SecretReport>> {
    report_with_home(index, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

fn report_with_home(index: &SecretIndex, home: Option<&Path>) -> io::Result<Vec<SecretReport>> {
    Ok(index
        .entries()?
        .into_iter()
        .map(|entry| {
            let status = status_of(&entry, home);
            SecretReport { entry, status }
        })
        .collect())
}

fn status_of(entry: &IndexEntry, home: Option<&Path>) -> SecretStatus {
    let scope = entry.scope.as_str();
    if let Some(path) = scope.strip_prefix("doc:") {
        return doc_status(Path::new(path), &entry.name);
    }
    let (config_path, project_root) = if let Some(root) = scope.strip_prefix("project:") {
        let root = Path::new(root);
        if !root.is_dir() {
            return SecretStatus::OrphanPath;
        }
        (crate::config::local_config_path(root), Some(root))
    } else if scope == "global" || scope.starts_with("global:") {
        match crate::config::global_config_path() {
            Some(p) => (p, None),
            None => return SecretStatus::Unknown("no HOME".to_string()),
        }
    } else {
        return SecretStatus::Unknown(format!("unrecognised scope {scope:?}"));
    };
    match crate::config::read_table_strict(&config_path) {
        Ok(table) => {
            let declared = crate::shared_env::declared_secrets(&table, home, project_root);
            if declared.contains(&(entry.scope.clone(), entry.name.clone())) {
                SecretStatus::Ok
            } else {
                SecretStatus::OrphanDecl
            }
        }
        Err(e) => SecretStatus::Unknown(e),
    }
}

fn doc_status(path: &Path, name: &str) -> SecretStatus {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return SecretStatus::OrphanPath,
        Err(e) => return SecretStatus::Unknown(format!("{}: {e}", path.display())),
    };
    let decls = crate::Canvas::from_markdown(&raw)
        .map_err(|e| e.to_string())
        .and_then(|canvas| crate::vars::declared_vars(&canvas).map_err(|e| e.to_string()));
    match decls {
        Ok(decls) if decls.iter().any(|d| d.name == name && d.secret) => SecretStatus::Ok,
        Ok(_) => SecretStatus::OrphanDecl,
        Err(e) => SecretStatus::Unknown(format!("{}: {e}", path.display())),
    }
}

/// Deletes one secret from `backend` and the index. Deleting something the
/// store no longer has is fine (`Ok(false)`); the index row goes either way.
pub fn remove(
    index: &SecretIndex,
    backend: &dyn SecretBackend,
    scope: &str,
    name: &str,
) -> io::Result<bool> {
    let removed = backend.delete(&secret_store::account_for(scope, name))?;
    index.forget(scope, name)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::MemoryBackend;

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "meshfox-secret-index-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
        .to_string_lossy()
        .replace(['(', ')'], "");
        let d = PathBuf::from(d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn index(dir: &Path) -> SecretIndex {
        SecretIndex::at(dir.join("secrets.sqlite3"))
    }

    #[test]
    fn a_never_written_index_is_empty_and_not_created_by_reading() {
        let dir = tempdir("empty");
        let idx = index(&dir);
        assert!(idx.entries().unwrap().is_empty());
        assert!(!idx.path().exists());
    }

    #[test]
    fn record_is_idempotent_and_forget_removes() {
        let dir = tempdir("record");
        let idx = index(&dir);
        idx.record("global", "A").unwrap();
        let first = idx.entries().unwrap();
        idx.record("global", "A").unwrap();
        idx.record("global:~/w", "A").unwrap();
        let all = idx.entries().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(first[0].created_at, all.iter().find(|e| e.scope == "global").unwrap().created_at);
        idx.forget("global", "A").unwrap();
        assert_eq!(idx.entries().unwrap().len(), 1);
    }

    #[test]
    fn a_doc_secret_is_ok_orphan_decl_or_orphan_path() {
        let dir = tempdir("doc");
        let canvas = dir.join("a.canvas.md");
        std::fs::write(
            &canvas,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:var name=\"TOKEN\" secret -->\n<!-- meshfox:var name=\"PLAIN\" -->\n",
        )
        .unwrap();
        let scope = secret_store::doc_scope(&canvas);
        let idx = index(&dir);
        idx.record(&scope, "TOKEN").unwrap(); // declared secret
        idx.record(&scope, "PLAIN").unwrap(); // declared, but not secret
        idx.record(&scope, "GONE").unwrap(); // not declared
        idx.record("doc:/nonexistent/x.canvas.md", "TOKEN").unwrap();

        let by_name = |r: &[SecretReport], scope: &str, name: &str| {
            r.iter()
                .find(|r| r.entry.scope == scope && r.entry.name == name)
                .unwrap()
                .status
                .clone()
        };
        let r = report_with_home(&idx, None).unwrap();
        assert_eq!(by_name(&r, &scope, "TOKEN"), SecretStatus::Ok);
        assert_eq!(by_name(&r, &scope, "PLAIN"), SecretStatus::OrphanDecl);
        assert_eq!(by_name(&r, &scope, "GONE"), SecretStatus::OrphanDecl);
        assert_eq!(
            by_name(&r, "doc:/nonexistent/x.canvas.md", "TOKEN"),
            SecretStatus::OrphanPath
        );
    }

    #[test]
    fn an_unparsable_canvas_is_unknown_not_an_orphan() {
        let dir = tempdir("badcanvas");
        let canvas = dir.join("b.canvas.md");
        std::fs::write(&canvas, "not a canvas at all").unwrap();
        let idx = index(&dir);
        idx.record(&secret_store::doc_scope(&canvas), "TOKEN").unwrap();
        let r = report_with_home(&idx, None).unwrap();
        assert!(matches!(r[0].status, SecretStatus::Unknown(_)), "{:?}", r[0].status);
        assert!(!r[0].status.is_orphan());
    }

    #[test]
    fn a_project_secret_follows_the_config_file() {
        let dir = tempdir("project");
        let root = dir.join("proj");
        std::fs::create_dir_all(root.join(".meshfox")).unwrap();
        let scope = crate::shared_env::project_account_scope(&root);
        let idx = index(&dir);
        idx.record(&scope, "DB").unwrap();
        idx.record(&scope, "OLD").unwrap();
        idx.record("project:/nonexistent/proj", "DB").unwrap();

        std::fs::write(root.join(".meshfox/config.toml"), "[[env]]\nsecrets = [\"DB\"]\n").unwrap();
        let r = report_with_home(&idx, None).unwrap();
        let st = |scope: &str, name: &str| {
            r.iter()
                .find(|r| r.entry.scope == scope && r.entry.name == name)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(st(&scope, "DB"), SecretStatus::Ok);
        assert_eq!(st(&scope, "OLD"), SecretStatus::OrphanDecl);
        assert_eq!(st("project:/nonexistent/proj", "DB"), SecretStatus::OrphanPath);

        // A config that no longer parses is "unknown", never an orphan.
        std::fs::write(root.join(".meshfox/config.toml"), "[[env\n").unwrap();
        let r = report_with_home(&idx, None).unwrap();
        assert!(matches!(
            r.iter().find(|r| r.entry.scope == scope && r.entry.name == "DB").unwrap().status,
            SecretStatus::Unknown(_)
        ));
    }

    #[test]
    fn remove_deletes_from_store_and_index() {
        let dir = tempdir("remove");
        let idx = index(&dir);
        let store = MemoryBackend::new();
        store.set(&secret_store::account_for("global", "A"), "v").unwrap();
        idx.record("global", "A").unwrap();
        assert!(remove(&idx, &store, "global", "A").unwrap());
        assert!(idx.entries().unwrap().is_empty());
        assert_eq!(store.get(&secret_store::account_for("global", "A")).unwrap(), None);
        // Already gone from the store: still fine.
        idx.record("global", "A").unwrap();
        assert!(!remove(&idx, &store, "global", "A").unwrap());
        assert!(idx.entries().unwrap().is_empty());
    }
}
