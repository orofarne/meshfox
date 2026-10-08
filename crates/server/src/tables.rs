//! `display="table"` — read-only, windowed access to a tabular `file`
//! node's target (CSV, TSV, Parquet, JSON lines, ...), served by driving the
//! `duckdb` command-line tool as a long-lived child process, the way
//! `meshfox pdf` drives a headless Chrome: the dependency is only needed
//! when a canvas actually has such a node, is never downloaded or installed
//! by meshfox, and its absence is a clear, platform-neutral error.
//!
//! One [`Session`] per table node (keyed by node id, and invalidated when the
//! target's size or mtime changes), each owning one `duckdb` process over a
//! cache database under [`TableManager`]'s cache directory:
//!
//! - **normal canvas**: `<canvas dir>/.meshfox/tables/<key>/`
//! - **read-only canvas**: `<system temp>/meshfox-readonly-<hash>/tables-<pid>/<key>/`
//!   (owner-only, like the `python_venv` exception — see SPEC.md "Read-only
//!   canvases"). Per-pid, because two workers may serve the same read-only
//!   canvas side by side and a `.duckdb` file has a single writer; dirs of
//!   dead pids are swept on first use. There is deliberately **no**
//!   in-memory fallback: if the directory can't be created, the table fails
//!   with an error.
//!
//! Importing a CSV-like file into a `t` table happens once, in the
//! background; until it's done a quick direct read of the first rows
//! ([`PREVIEW_ROWS`]) answers instead. Parquet is exposed as a view over the
//! file — no copy. Sorting/filtering/searching materializes the result into
//! a temp table (`v`), so scrolling through a sorted 100M-row table is a
//! cheap `rowid` range read after the first request.
//!
//! After the data is in, the process is locked down
//! (`enable_external_access=false`, only the source file and the cache dir
//! allowed, configuration locked), so nothing the client sends can reach
//! other files or the network; the client never sends SQL anyway — only a
//! [`ViewSpec`] whose column references are indexes validated against the
//! table's own columns and whose literals are quoted by [`sql_str`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

/// Rows served straight from the file while the import is still running.
pub const PREVIEW_ROWS: usize = 200;
/// Most rows one page request may ask for.
pub const MAX_PAGE_ROWS: usize = 1000;
/// Cells longer than this are cut when paged out (the table keeps them).
const CELL_MAX_CHARS: usize = 10_000;
const QUERY_TIMEOUT: Duration = Duration::from_secs(120);
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(60);
const TOUCH_EVERY: Duration = Duration::from_secs(30);
const DUCKDB_MEMORY_LIMIT: &str = "2GB";
const DUCKDB_THREADS: u32 = 4;
const MAX_SORT_KEYS: usize = 4;
const MAX_FILTERS: usize = 32;

/// What the UI should say when no `duckdb` can be found.
pub const DUCKDB_MISSING_MESSAGE: &str = "DuckDB CLI not found. Table preview needs the `duckdb` \
executable: put it on PATH, or set `duckdb_path` under `[tables]` in .meshfox/config.toml \
(or the MESHFOX_DUCKDB environment variable).";

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TableState {
    Importing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableFailure {
    /// `duckdb-missing`, `cache`, `import` or `limit`.
    pub kind: &'static str,
    pub message: String,
}

impl TableFailure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub index: usize,
    pub name: String,
    /// DuckDB's own type name (`VARCHAR`, `BIGINT`, `DECIMAL(18,3)`, ...).
    #[serde(rename = "type")]
    pub col_type: String,
    /// Coarse class for alignment/filter UI: `number`, `text`, `temporal`,
    /// `bool` or `other`.
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableMeta {
    pub state: TableState,
    pub error: Option<TableFailure>,
    pub columns: Vec<Column>,
    /// Row count of the whole table; `None` until the import finishes.
    pub total_rows: Option<u64>,
    pub file_size: u64,
    pub mtime_ms: u64,
    /// Changes whenever the target does — a client holding rows from another
    /// version should drop them.
    pub version: String,
    /// `true` while `columns` come from the quick direct read, not the
    /// imported table (sorting/filtering aren't available yet).
    pub preview: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowsPage {
    pub state: TableState,
    pub version: String,
    pub offset: usize,
    /// One entry per row, one per column; `None` is SQL NULL. Everything
    /// else is DuckDB's `VARCHAR` rendering, so no precision is lost to JSON.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows matching the view (== `total_rows` for the unfiltered view);
    /// `None` while importing.
    pub matched_rows: Option<u64>,
    pub total_rows: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSpec {
    #[serde(default)]
    pub sort: Vec<SortKey>,
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Case-insensitive substring searched in every column.
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub column: usize,
    #[serde(default)]
    pub desc: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub column: usize,
    pub op: FilterOp,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    StartsWith,
    EndsWith,
    IsNull,
    NotNull,
}

impl ViewSpec {
    pub fn is_empty(&self) -> bool {
        self.sort.is_empty() && self.filters.is_empty() && self.search.is_none()
    }

    /// Trims the search text and drops it when blank, so two specs that mean
    /// the same compare equal.
    fn normalized(mut self) -> Self {
        self.search = self
            .search
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self
    }
}

#[derive(Debug)]
pub enum TableError {
    /// The target is missing, not a regular file, or outside the canvas.
    Target(String),
    /// The request itself is wrong (bad column index, bad value, ...).
    BadRequest(String),
    /// The table can't be served at all right now.
    Unavailable(TableFailure),
    /// DuckDB rejected a query.
    Query(String),
}

impl TableError {
    pub fn message(&self) -> String {
        match self {
            TableError::Target(m) | TableError::BadRequest(m) | TableError::Query(m) => m.clone(),
            TableError::Unavailable(f) => f.message.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// SQL building
// ---------------------------------------------------------------------------

/// A single-quoted SQL string literal. DuckDB string literals have no
/// backslash escapes, so doubling `'` is complete. Line breaks and NUL are
/// refused instead of quoted: statements go over a line-oriented pipe.
fn sql_str(s: &str) -> Result<String, String> {
    if s.contains(['\0', '\n', '\r']) {
        return Err("value contains a line break or NUL character".to_string());
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

fn sql_ident(s: &str) -> Result<String, String> {
    if s.contains(['\0', '\n', '\r']) {
        return Err(format!(
            "column name {s:?} contains a line break, which table preview can't address"
        ));
    }
    Ok(format!("\"{}\"", s.replace('"', "\"\"")))
}

fn sql_path(p: &Path) -> Result<String, String> {
    sql_str(&p.to_string_lossy())
}

fn column_kind(col_type: &str) -> &'static str {
    let t = col_type.to_ascii_uppercase();
    let head = t.split(['(', '[', ' ']).next().unwrap_or("");
    match head {
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT"
        | "UINTEGER" | "UBIGINT" | "UHUGEINT" | "FLOAT" | "DOUBLE" | "DECIMAL" | "REAL" | "INT" => {
            "number"
        }
        "BOOLEAN" => "bool",
        "DATE" | "TIME" | "TIMESTAMP" | "TIMESTAMPTZ" | "TIMETZ" | "INTERVAL" => "temporal",
        "VARCHAR" | "TEXT" | "STRING" | "UUID" => "text",
        _ => "other",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Imported into table `t` in the cache database.
    Table,
    /// A view over the file itself (Parquet).
    View,
}

/// `.csv.gz` → `csv`: the extension that decides the reader.
fn effective_extension(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let name = ["gz", "zst"]
        .iter()
        .find_map(|c| name.strip_suffix(&format!(".{c}")))
        .unwrap_or(&name);
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_string())
        .unwrap_or_default()
}

/// The DuckDB table function reading `path`, and whether the result is
/// imported (CSV-like, JSON) or just viewed (Parquet). `all_varchar` is the
/// CSV retry when type inference over the sample guessed wrong.
fn reader_sql(path: &Path, all_varchar: bool) -> Result<(String, Source), String> {
    let lit = sql_path(path)?;
    Ok(match effective_extension(path).as_str() {
        "parquet" | "parq" => (format!("read_parquet({lit})"), Source::View),
        "json" | "jsonl" | "ndjson" => (format!("read_json_auto({lit})"), Source::Table),
        _ => (
            format!(
                "read_csv({lit}, null_padding=true{})",
                if all_varchar {
                    ", all_varchar=true"
                } else {
                    ""
                }
            ),
            Source::Table,
        ),
    })
}

fn contains_sql(col: &str, needle: &str, func: &str) -> String {
    format!("{func}(lower(CAST({col} AS VARCHAR)), lower({needle}))")
}

fn where_sql(spec: &ViewSpec, columns: &[Column]) -> Result<Option<String>, TableError> {
    let bad = |m: String| TableError::BadRequest(m);
    let mut parts = Vec::new();
    for f in &spec.filters {
        let col = columns
            .get(f.column)
            .ok_or_else(|| bad(format!("filter column {} out of range", f.column)))?;
        let id = sql_ident(&col.name).map_err(bad)?;
        let value = || -> Result<String, TableError> {
            let v = f
                .value
                .as_deref()
                .ok_or_else(|| bad(format!("filter on {:?} needs a value", col.name)))?;
            sql_str(v).map_err(bad)
        };
        parts.push(match f.op {
            FilterOp::Eq => format!("{id} = {}", value()?),
            FilterOp::Ne => format!("{id} <> {}", value()?),
            FilterOp::Lt => format!("{id} < {}", value()?),
            FilterOp::Le => format!("{id} <= {}", value()?),
            FilterOp::Gt => format!("{id} > {}", value()?),
            FilterOp::Ge => format!("{id} >= {}", value()?),
            FilterOp::Contains => contains_sql(&id, &value()?, "contains"),
            FilterOp::StartsWith => contains_sql(&id, &value()?, "starts_with"),
            FilterOp::EndsWith => contains_sql(&id, &value()?, "ends_with"),
            FilterOp::IsNull => format!("{id} IS NULL"),
            FilterOp::NotNull => format!("{id} IS NOT NULL"),
        });
    }
    if let Some(s) = &spec.search {
        let needle = sql_str(s).map_err(bad)?;
        let any = columns
            .iter()
            .map(|c| Ok(contains_sql(&sql_ident(&c.name)?, &needle, "contains")))
            .collect::<Result<Vec<_>, String>>()
            .map_err(bad)?
            .join(" OR ");
        parts.push(format!("({any})"));
    }
    Ok(if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    })
}

fn order_sql(spec: &ViewSpec, columns: &[Column]) -> Result<Option<String>, TableError> {
    if spec.sort.is_empty() {
        return Ok(None);
    }
    let keys = spec
        .sort
        .iter()
        .map(|k| {
            let col = columns.get(k.column).ok_or_else(|| {
                TableError::BadRequest(format!("sort column {} out of range", k.column))
            })?;
            let id = sql_ident(&col.name).map_err(TableError::BadRequest)?;
            Ok(format!(
                "{id} {} NULLS LAST",
                if k.desc { "DESC" } else { "ASC" }
            ))
        })
        .collect::<Result<Vec<_>, TableError>>()?;
    Ok(Some(keys.join(", ")))
}

fn validate_spec(spec: &ViewSpec) -> Result<(), TableError> {
    if spec.sort.len() > MAX_SORT_KEYS {
        return Err(TableError::BadRequest(format!(
            "at most {MAX_SORT_KEYS} sort keys"
        )));
    }
    if spec.filters.len() > MAX_FILTERS {
        return Err(TableError::BadRequest(format!(
            "at most {MAX_FILTERS} filters"
        )));
    }
    Ok(())
}

fn materialize_sql(spec: &ViewSpec, columns: &[Column]) -> Result<String, TableError> {
    validate_spec(spec)?;
    let mut sql = String::from("CREATE OR REPLACE TEMP TABLE v AS SELECT * FROM t");
    if let Some(w) = where_sql(spec, columns)? {
        sql.push_str(" WHERE ");
        sql.push_str(&w);
    }
    if let Some(o) = order_sql(spec, columns)? {
        sql.push_str(" ORDER BY ");
        sql.push_str(&o);
    }
    sql.push(';');
    Ok(sql)
}

fn select_list(columns: &[Column]) -> Result<String, TableError> {
    columns
        .iter()
        .map(|c| {
            Ok(format!(
                "left(CAST({} AS VARCHAR), {CELL_MAX_CHARS}) AS c{}",
                sql_ident(&c.name)?,
                c.index
            ))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|v| v.join(", "))
        .map_err(TableError::BadRequest)
}

fn parse_rows(lines: &[String], ncols: usize) -> Result<Vec<Vec<Option<String>>>, String> {
    lines
        .iter()
        .map(|line| {
            let obj: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(line).map_err(|e| format!("unreadable duckdb row: {e}"))?;
            Ok((0..ncols)
                .map(|i| match obj.get(&format!("c{i}")) {
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(serde_json::Value::Null) | None => None,
                    Some(other) => Some(other.to_string()),
                })
                .collect())
        })
        .collect()
}

fn parse_columns(lines: &[String]) -> Result<Vec<Column>, String> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let v: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("unreadable DESCRIBE row: {e}"))?;
            let name = v["column_name"].as_str().unwrap_or("").to_string();
            let col_type = v["column_type"].as_str().unwrap_or("").to_string();
            Ok(Column {
                index,
                kind: column_kind(&col_type),
                name,
                col_type,
            })
        })
        .collect()
}

fn parse_count(lines: &[String]) -> Result<u64, String> {
    lines
        .first()
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .and_then(|v| v["n"].as_u64())
        .ok_or_else(|| "unreadable row count from duckdb".to_string())
}

// ---------------------------------------------------------------------------
// The duckdb process
// ---------------------------------------------------------------------------

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A running `duckdb` CLI reading statements from stdin. Each statement
/// batch is followed by two sentinels — a marker row on stdout and a
/// deliberate error on stderr — so both streams can be read to a known
/// point regardless of how DuckDB interleaves them. The marker embeds a
/// per-process nonce the data in a file can't predict.
struct DuckProc {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: mpsc::UnboundedReceiver<String>,
    nonce: String,
    seq: u64,
    dead: bool,
}

impl DuckProc {
    fn spawn(duckdb: &Path, db: Option<&Path>) -> Result<Self, String> {
        let mut cmd = Command::new(duckdb);
        if let Some(db) = db {
            cmd.arg(db);
        }
        cmd.arg("-jsonlines")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to start {}: {e}", duckdb.display()))?;
        let stdin = child.stdin.take().ok_or("duckdb has no stdin")?;
        let stdout = child.stdout.take().ok_or("duckdb has no stdout")?;
        let stderr = child.stderr.take().ok_or("duckdb has no stderr")?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let nonce = format!(
            "{:x}{:x}{:x}",
            nanos,
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            stderr: rx,
            nonce,
            seq: 0,
            dead: false,
        })
    }

    /// Runs `sql` (any number of statements) and returns the JSON lines the
    /// statements printed, or DuckDB's error text. A timeout or a dead
    /// process kills the child and marks it `dead`.
    async fn exec(&mut self, sql: &str, timeout: Duration) -> Result<Vec<String>, String> {
        if self.dead {
            return Err("duckdb is not running".to_string());
        }
        match tokio::time::timeout(timeout, self.exec_inner(sql)).await {
            Ok(r) => r,
            Err(_) => {
                self.kill();
                Err(format!("query timed out after {}s", timeout.as_secs()))
            }
        }
    }

    async fn exec_inner(&mut self, sql: &str) -> Result<Vec<String>, String> {
        self.seq += 1;
        let end_row = format!("{{\"m\":\"__MFX_{}_END_{}__\"}}", self.nonce, self.seq);
        let err_tag = format!("__MFX_{}_ERR_{}__", self.nonce, self.seq);
        let script = format!(
            "{sql}\nSELECT '__MFX_{}_END_{}__' AS m;\nSELECT error('{err_tag}');\n",
            self.nonce, self.seq
        );
        if let Err(e) = self.stdin.write_all(script.as_bytes()).await {
            return Err(self.died(format!("write to duckdb failed: {e}")).await);
        }
        if let Err(e) = self.stdin.flush().await {
            return Err(self.died(format!("write to duckdb failed: {e}")).await);
        }
        let mut out = Vec::new();
        loop {
            match self.stdout.next_line().await {
                Ok(Some(line)) if line == end_row => break,
                Ok(Some(line)) => out.push(line),
                Ok(None) => return Err(self.died("duckdb exited unexpectedly".into()).await),
                Err(e) => return Err(self.died(format!("read from duckdb failed: {e}")).await),
            }
        }
        let mut errs = Vec::new();
        loop {
            match self.stderr.recv().await {
                Some(line) if line.contains(&err_tag) => break,
                Some(line) => errs.push(line),
                None => return Err(self.died("duckdb exited unexpectedly".into()).await),
            }
        }
        let msg = errs.join("\n").trim().to_string();
        if msg.is_empty() { Ok(out) } else { Err(msg) }
    }

    async fn died(&mut self, what: String) -> String {
        self.kill();
        let mut tail = Vec::new();
        while let Ok(line) = self.stderr.try_recv() {
            tail.push(line);
        }
        let tail = tail.join("\n");
        if tail.trim().is_empty() {
            what
        } else {
            format!("{what}: {}", tail.trim())
        }
    }

    fn kill(&mut self) {
        self.dead = true;
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// Locating duckdb
// ---------------------------------------------------------------------------

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Finds the `duckdb` executable: `MESHFOX_DUCKDB`, then `[tables]
/// duckdb_path` in config, then `PATH`, then the places the official
/// installers and package managers put it (the worker may have been started
/// by a launcher whose `PATH` lacks them). A path set explicitly that
/// doesn't exist is an error naming it, not a silent fall-through.
pub fn locate_duckdb(canvas_dir: &Path) -> Result<PathBuf, TableFailure> {
    let missing = || TableFailure::new("duckdb-missing", DUCKDB_MISSING_MESSAGE);
    let explicit = match std::env::var("MESHFOX_DUCKDB") {
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => meshfox_core::config::tables_duckdb_path(canvas_dir),
    };
    if let Some(p) = explicit {
        return if is_executable_file(&p) {
            Ok(p)
        } else {
            Err(TableFailure::new(
                "duckdb-missing",
                format!(
                    "The configured DuckDB executable {} does not exist or is not executable.",
                    p.display()
                ),
            ))
        };
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("duckdb");
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
    }
    let mut known = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        known.push(PathBuf::from(home).join(".duckdb/cli/latest/duckdb"));
    }
    known.extend(
        [
            "/opt/homebrew/bin/duckdb",
            "/usr/local/bin/duckdb",
            "/usr/bin/duckdb",
        ]
        .iter()
        .map(PathBuf::from),
    );
    known
        .into_iter()
        .find(|p| is_executable_file(p))
        .ok_or_else(missing)
}

// ---------------------------------------------------------------------------
// Cache directory
// ---------------------------------------------------------------------------

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

/// Removes `tables-<pid>` siblings of `own` whose process is gone (a worker
/// that was killed outright never got to clean up).
fn sweep_dead_cache_dirs(own: &Path) {
    let Some(parent) = own.parent() else { return };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == own {
            continue;
        }
        let pid = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix("tables-"))
            .and_then(|p| p.parse::<i32>().ok());
        if let Some(pid) = pid {
            if !pid_alive(pid) {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

fn touch(dir: &Path) {
    let _ = std::fs::write(dir.join(".used"), b"");
}

fn last_used(dir: &Path) -> SystemTime {
    std::fs::metadata(dir.join(".used"))
        .or_else(|_| std::fs::metadata(dir))
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

// ---------------------------------------------------------------------------
// Manager and sessions
// ---------------------------------------------------------------------------

enum SessionState {
    Importing,
    Ready {
        columns: Arc<Vec<Column>>,
        total: u64,
    },
    Failed(TableFailure),
}

struct Preview {
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
}

struct Slot {
    proc: Option<DuckProc>,
    /// The spec the temp table `v` currently holds, and its row count.
    view: Option<(ViewSpec, u64)>,
}

pub struct Session {
    key: String,
    source: PathBuf,
    kind: Source,
    dir: PathBuf,
    duckdb: PathBuf,
    max_bytes: u64,
    file_size: u64,
    mtime_ms: u64,
    state: StdMutex<SessionState>,
    slot: tokio::sync::Mutex<Slot>,
    preview: tokio::sync::OnceCell<Result<Arc<Preview>, String>>,
    last_touch: StdMutex<Instant>,
}

pub struct TableManager {
    canvas_dir: PathBuf,
    canvas_abs: PathBuf,
    read_only: bool,
    cache_dir: StdMutex<Option<PathBuf>>,
    sessions: tokio::sync::Mutex<HashMap<String, Arc<Session>>>,
}

impl TableManager {
    pub fn new(canvas_path: &Path, read_only: bool) -> Self {
        let canvas_abs = if canvas_path.is_absolute() {
            canvas_path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(canvas_path))
                .unwrap_or_else(|_| canvas_path.to_path_buf())
        };
        let canvas_dir = match canvas_abs.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("/"),
        };
        Self {
            canvas_dir,
            canvas_abs,
            read_only,
            cache_dir: StdMutex::new(None),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The cache directory, created on first use. An error here is final —
    /// there is no in-memory fallback.
    fn ensure_cache_dir(&self) -> Result<PathBuf, TableFailure> {
        let mut slot = self.cache_dir.lock().unwrap();
        if let Some(d) = slot.as_ref() {
            return Ok(d.clone());
        }
        let dir = if self.read_only {
            meshfox_core::builtin_interpreter::read_only_temp_root(&self.canvas_abs)
                .join(format!("tables-{}", std::process::id()))
        } else {
            self.canvas_dir.join(".meshfox").join("tables")
        };
        create_private_dir(&dir).map_err(|e| {
            TableFailure::new(
                "cache",
                format!(
                    "can't create the table cache directory {}: {e}",
                    dir.display()
                ),
            )
        })?;
        if self.read_only {
            sweep_dead_cache_dirs(&dir);
        }
        *slot = Some(dir.clone());
        Ok(dir)
    }

    fn max_bytes(&self) -> u64 {
        meshfox_core::config::tables_cache_max_bytes(&self.canvas_dir)
    }

    /// Evicts least-recently-used tables until the cache fits in
    /// `max_bytes`, never `keep` and never one still importing. Closing the
    /// session of an evicted table is fine: asking for it again re-imports.
    async fn make_room(
        &self,
        cache: &Path,
        keep: &str,
        sessions: &mut HashMap<String, Arc<Session>>,
    ) {
        let max = self.max_bytes();
        let Ok(entries) = std::fs::read_dir(cache) else {
            return;
        };
        let mut dirs: Vec<(PathBuf, u64, SystemTime)> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| {
                let p = e.path();
                let size = dir_size(&p);
                let used = last_used(&p);
                (p, size, used)
            })
            .collect();
        let mut total: u64 = dirs.iter().map(|d| d.1).sum();
        dirs.sort_by_key(|d| d.2);
        for (path, size, _) in dirs {
            if total <= max {
                break;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            if name.as_deref() == Some(keep) {
                continue;
            }
            let owner = sessions
                .iter()
                .find(|(_, s)| Some(s.key.as_str()) == name.as_deref())
                .map(|(id, s)| (id.clone(), Arc::clone(s)));
            if let Some((id, s)) = owner {
                if matches!(*s.state.lock().unwrap(), SessionState::Importing) {
                    continue;
                }
                sessions.remove(&id);
                s.close().await;
            }
            let _ = std::fs::remove_dir_all(&path);
            total = total.saturating_sub(size);
        }
    }

    /// The session for `node_id`'s target, starting (or replacing, when the
    /// file changed) one as needed. `source` must already be confined to the
    /// canvas directory. `Err(TableError::Unavailable(..))` is an expected
    /// condition the UI reports in place (DuckDB missing, no cache dir).
    pub async fn session(&self, node_id: &str, source: &Path) -> Result<Arc<Session>, TableError> {
        let meta = std::fs::metadata(source)
            .map_err(|e| TableError::Target(format!("{}: {e}", source.display())))?;
        if !meta.is_file() {
            return Err(TableError::Target(format!(
                "{} is not a regular file",
                source.display()
            )));
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
        let mtime_ms = mtime.map(|d| d.as_millis() as u64).unwrap_or(0);
        let mtime_ns = mtime.map(|d| d.as_nanos()).unwrap_or(0);
        let key = format!(
            "{:016x}",
            fnv1a64(format!("{}\0{}\0{}", source.display(), meta.len(), mtime_ns).as_bytes())
        );

        let mut sessions = self.sessions.lock().await;
        if let Some(existing) = sessions.get(node_id) {
            if existing.key == key {
                return Ok(Arc::clone(existing));
            }
        }
        // Changed (or first) — drop any stale session of this node.
        if let Some(old) = sessions.remove(node_id) {
            old.close().await;
            let _ = std::fs::remove_dir_all(&old.dir);
        }

        let duckdb = locate_duckdb(&self.canvas_dir).map_err(TableError::Unavailable)?;
        let cache = self.ensure_cache_dir().map_err(TableError::Unavailable)?;
        self.make_room(&cache, &key, &mut sessions).await;

        let dir = cache.join(&key);
        create_private_dir(&dir).map_err(|e| {
            TableError::Unavailable(TableFailure::new(
                "cache",
                format!("can't create {}: {e}", dir.display()),
            ))
        })?;
        touch(&dir);
        let (_, kind) = reader_sql(source, false).map_err(TableError::BadRequest)?;
        let session = Arc::new(Session {
            key,
            source: source.to_path_buf(),
            kind,
            dir,
            duckdb,
            max_bytes: self.max_bytes(),
            file_size: meta.len(),
            mtime_ms,
            state: StdMutex::new(SessionState::Importing),
            slot: tokio::sync::Mutex::new(Slot {
                proc: None,
                view: None,
            }),
            preview: tokio::sync::OnceCell::new(),
            last_touch: StdMutex::new(Instant::now()),
        });
        sessions.insert(node_id.to_string(), Arc::clone(&session));
        let background = Arc::clone(&session);
        tokio::spawn(async move { background.run_import().await });
        Ok(session)
    }

    /// Describes `node_id`'s table without ever failing on an *expected*
    /// condition — those come back as `state: failed` with an error the UI
    /// shows in place.
    pub async fn meta(&self, node_id: &str, source: &Path) -> Result<TableMeta, TableError> {
        match self.session(node_id, source).await {
            Ok(s) => Ok(s.meta().await),
            Err(TableError::Unavailable(failure)) => {
                let (file_size, mtime_ms) = std::fs::metadata(source)
                    .map(|m| {
                        let ms = m
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        (m.len(), ms)
                    })
                    .unwrap_or((0, 0));
                Ok(TableMeta {
                    state: TableState::Failed,
                    error: Some(failure),
                    columns: Vec::new(),
                    total_rows: None,
                    file_size,
                    mtime_ms,
                    version: String::new(),
                    preview: false,
                })
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for TableManager {
    fn drop(&mut self) {
        if self.read_only {
            if let Some(dir) = self.cache_dir.lock().unwrap().take() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
}

impl Session {
    #[cfg(test)]
    fn failure(&self) -> Option<TableFailure> {
        match &*self.state.lock().unwrap() {
            SessionState::Failed(f) => Some(f.clone()),
            _ => None,
        }
    }

    fn touch_if_due(&self) {
        let mut last = self.last_touch.lock().unwrap();
        if last.elapsed() >= TOUCH_EVERY {
            *last = Instant::now();
            touch(&self.dir);
        }
    }

    async fn close(&self) {
        let mut slot = self.slot.lock().await;
        if let Some(mut p) = slot.proc.take() {
            p.kill();
        }
    }

    fn db_path(&self) -> PathBuf {
        self.dir.join("t.duckdb")
    }

    /// Settings every process gets before touching data: no extension
    /// downloads, bounded resources, spill space inside the cache dir.
    fn init_sql(&self) -> Result<String, String> {
        let tmp = sql_path(&self.dir.join("tmp"))?;
        Ok(format!(
            "SET autoinstall_known_extensions=false;\n\
             SET autoload_known_extensions=false;\n\
             SET threads={DUCKDB_THREADS};\n\
             SET memory_limit='{DUCKDB_MEMORY_LIMIT}';\n\
             SET temp_directory={tmp};\n\
             SET max_temp_directory_size='{}B';",
            self.max_bytes.max(1 << 20)
        ))
    }

    /// Cuts the process off from everything but the cache dir and (for a
    /// Parquet view) the source file, and locks the settings.
    fn lockdown_sql(&self) -> Result<String, String> {
        let allowed_files = match self.kind {
            Source::View => format!("[{}]", sql_path(&self.source)?),
            Source::Table => "[]".to_string(),
        };
        Ok(format!(
            "SET allowed_paths={allowed_files};\n\
             SET allowed_directories=[{}];\n\
             SET enable_external_access=false;\n\
             SET lock_configuration=true;",
            sql_path(&self.dir)?
        ))
    }

    /// Spawns the cache-database process and applies [`Self::init_sql`].
    async fn open_proc(&self) -> Result<DuckProc, String> {
        let mut proc = DuckProc::spawn(&self.duckdb, Some(&self.db_path()))?;
        proc.exec(&self.init_sql()?, QUERY_TIMEOUT).await?;
        Ok(proc)
    }

    async fn run_import(self: Arc<Self>) {
        let outcome = self.import().await;
        let mut state = self.state.lock().unwrap();
        *state = match outcome {
            Ok((columns, total)) => SessionState::Ready {
                columns: Arc::new(columns),
                total,
            },
            Err(f) => SessionState::Failed(f),
        };
    }

    async fn import(&self) -> Result<(Vec<Column>, u64), TableFailure> {
        let fail = |m: String| TableFailure::new("import", m);
        let mut slot = self.slot.lock().await;

        // A cache database from another DuckDB version (or a damaged one)
        // fails to open: start over once.
        let mut proc = match self.open_proc().await {
            Ok(p) => p,
            Err(first) => {
                let _ = std::fs::remove_file(self.db_path());
                let _ = std::fs::remove_file(self.dir.join("t.duckdb.wal"));
                self.open_proc().await.map_err(|_| fail(first))?
            }
        };

        match self.kind {
            Source::View => {
                let (reader, _) = reader_sql(&self.source, false).map_err(fail)?;
                proc.exec(
                    &format!("CREATE OR REPLACE VIEW t AS SELECT * FROM {reader};"),
                    QUERY_TIMEOUT,
                )
                .await
                .map_err(fail)?;
            }
            Source::Table => {
                let exists = proc
                    .exec(
                        "SELECT count(*) AS n FROM information_schema.tables \
                         WHERE table_name = 't' AND table_type = 'BASE TABLE';",
                        QUERY_TIMEOUT,
                    )
                    .await
                    .map_err(fail)
                    .and_then(|l| parse_count(&l).map_err(fail))?
                    > 0;
                if !exists {
                    // No timeout worth the name: importing is as long as the file is.
                    let long = Duration::from_secs(60 * 60 * 24);
                    let (reader, _) = reader_sql(&self.source, false).map_err(fail)?;
                    let first = proc
                        .exec(&format!("CREATE TABLE t AS SELECT * FROM {reader};"), long)
                        .await;
                    if let Err(first_err) = first {
                        // Type inference runs over a sample; a later row that
                        // disagrees fails the import. Read everything as text.
                        if proc.dead {
                            return Err(fail(first_err));
                        }
                        let (reader, _) = reader_sql(&self.source, true).map_err(fail)?;
                        proc.exec(&format!("CREATE TABLE t AS SELECT * FROM {reader};"), long)
                            .await
                            .map_err(|e| fail(format!("{first_err}\n(retry as text: {e})")))?;
                    }
                }
            }
        }

        let columns = proc
            .exec("DESCRIBE t;", QUERY_TIMEOUT)
            .await
            .map_err(fail)
            .and_then(|l| parse_columns(&l).map_err(fail))?;
        let total = proc
            .exec("SELECT count(*) AS n FROM t;", QUERY_TIMEOUT)
            .await
            .map_err(fail)
            .and_then(|l| parse_count(&l).map_err(fail))?;

        proc.exec(&self.lockdown_sql().map_err(fail)?, QUERY_TIMEOUT)
            .await
            .map_err(fail)?;

        let used = dir_size(&self.dir);
        if used > self.max_bytes {
            proc.kill();
            let _ = std::fs::remove_dir_all(&self.dir);
            return Err(TableFailure::new(
                "limit",
                format!(
                    "this table needs {used} bytes of cache but `cache_max_bytes` under `[tables]` \
                     in .meshfox/config.toml is {}; raise it to preview this file",
                    self.max_bytes
                ),
            ));
        }
        slot.proc = Some(proc);
        Ok((columns, total))
    }

    /// First rows and the schema read straight from the file in a throwaway
    /// process — what answers while the import runs.
    async fn preview(&self) -> Result<Arc<Preview>, String> {
        self.preview
            .get_or_init(|| async {
                let (reader, _) = reader_sql(&self.source, false)?;
                let mut proc = DuckProc::spawn(&self.duckdb, None)?;
                let result = async {
                    let mut setup = String::from(
                        "SET autoinstall_known_extensions=false;\n\
                         SET autoload_known_extensions=false;\n\
                         SET threads=2;\n",
                    );
                    setup.push_str(&format!("SET memory_limit='{DUCKDB_MEMORY_LIMIT}';\n"));
                    proc.exec(&setup, PREVIEW_TIMEOUT).await?;
                    let columns = parse_columns(
                        &proc
                            .exec(
                                &format!("DESCRIBE SELECT * FROM {reader};"),
                                PREVIEW_TIMEOUT,
                            )
                            .await?,
                    )?;
                    let select = select_list(&columns).map_err(|e| e.message())?;
                    let lines = proc
                        .exec(
                            &format!("SELECT {select} FROM {reader} LIMIT {PREVIEW_ROWS};"),
                            PREVIEW_TIMEOUT,
                        )
                        .await?;
                    let rows = parse_rows(&lines, columns.len())?;
                    Ok::<_, String>(Arc::new(Preview { columns, rows }))
                }
                .await;
                proc.kill();
                result
            })
            .await
            .clone()
    }

    pub async fn meta(&self) -> TableMeta {
        self.touch_if_due();
        let base = |state, error, columns, total_rows, preview| TableMeta {
            state,
            error,
            columns,
            total_rows,
            file_size: self.file_size,
            mtime_ms: self.mtime_ms,
            version: self.key.clone(),
            preview,
        };
        enum Now {
            Importing,
            Ready(Arc<Vec<Column>>, u64),
            Failed(TableFailure),
        }
        let now = match &*self.state.lock().unwrap() {
            SessionState::Importing => Now::Importing,
            SessionState::Ready { columns, total } => Now::Ready(Arc::clone(columns), *total),
            SessionState::Failed(f) => Now::Failed(f.clone()),
        };
        match now {
            Now::Ready(columns, total) => base(
                TableState::Ready,
                None,
                columns.to_vec(),
                Some(total),
                false,
            ),
            Now::Failed(f) => base(TableState::Failed, Some(f), Vec::new(), None, false),
            Now::Importing => match self.preview().await {
                Ok(p) => base(TableState::Importing, None, p.columns.clone(), None, true),
                Err(e) => base(
                    TableState::Failed,
                    Some(TableFailure::new("import", e)),
                    Vec::new(),
                    None,
                    false,
                ),
            },
        }
    }

    /// One page of the view `spec` describes.
    pub async fn rows(
        &self,
        spec: ViewSpec,
        offset: usize,
        limit: usize,
    ) -> Result<RowsPage, TableError> {
        self.touch_if_due();
        let spec = spec.normalized();
        let limit = limit.clamp(1, MAX_PAGE_ROWS);

        enum Now {
            Importing,
            Ready(Arc<Vec<Column>>, u64),
        }
        let now = match &*self.state.lock().unwrap() {
            SessionState::Importing => Now::Importing,
            SessionState::Ready { columns, total } => Now::Ready(Arc::clone(columns), *total),
            SessionState::Failed(f) => return Err(TableError::Unavailable(f.clone())),
        };

        let (columns, total) = match now {
            Now::Importing => {
                // Only the unfiltered head is available until the import ends.
                let rows = if spec.is_empty() {
                    let p = self
                        .preview()
                        .await
                        .map_err(|e| TableError::Unavailable(TableFailure::new("import", e)))?;
                    p.rows.iter().skip(offset).take(limit).cloned().collect()
                } else {
                    Vec::new()
                };
                return Ok(RowsPage {
                    state: TableState::Importing,
                    version: self.key.clone(),
                    offset,
                    rows,
                    matched_rows: None,
                    total_rows: None,
                });
            }
            Now::Ready(c, t) => (c, t),
        };

        let mut slot = self.slot.lock().await;
        if slot.proc.as_ref().map_or(true, |p| p.dead) {
            // The process was killed (timeout) or never survived: reopen.
            slot.view = None;
            let mut proc = self.open_proc().await.map_err(TableError::Query)?;
            proc.exec(
                &self.lockdown_sql().map_err(TableError::Query)?,
                QUERY_TIMEOUT,
            )
            .await
            .map_err(TableError::Query)?;
            slot.proc = Some(proc);
        }

        let (source, matched) = if spec.is_empty() {
            ("t", total)
        } else {
            match &slot.view {
                Some((s, n)) if *s == spec => ("v", *n),
                _ => {
                    let sql = materialize_sql(&spec, &columns)?;
                    slot.view = None;
                    let proc = slot.proc.as_mut().expect("opened above");
                    proc.exec(&sql, QUERY_TIMEOUT)
                        .await
                        .map_err(TableError::Query)?;
                    let n = parse_count(
                        &proc
                            .exec("SELECT count(*) AS n FROM v;", QUERY_TIMEOUT)
                            .await
                            .map_err(TableError::Query)?,
                    )
                    .map_err(TableError::Query)?;
                    slot.view = Some((spec.clone(), n));
                    ("v", n)
                }
            }
        };

        let select = select_list(&columns)?;
        // Real tables keep rowids dense and in insertion order, so a window
        // is a range read; only the Parquet view (no rowid) pages by OFFSET.
        let sql = if source == "t" && self.kind == Source::View {
            format!("SELECT {select} FROM t OFFSET {offset} LIMIT {limit};")
        } else {
            format!(
                "SELECT {select} FROM {source} WHERE rowid >= {offset} AND rowid < {} ORDER BY rowid;",
                offset + limit
            )
        };
        let proc = slot.proc.as_mut().expect("opened above");
        let lines = proc
            .exec(&sql, QUERY_TIMEOUT)
            .await
            .map_err(TableError::Query)?;
        let rows = parse_rows(&lines, columns.len()).map_err(TableError::Query)?;
        Ok(RowsPage {
            state: TableState::Ready,
            version: self.key.clone(),
            offset,
            rows,
            matched_rows: Some(matched),
            total_rows: Some(total),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .enumerate()
            .map(|(index, n)| Column {
                index,
                name: n.to_string(),
                col_type: "VARCHAR".into(),
                kind: "text",
            })
            .collect()
    }

    #[test]
    fn string_literals_double_quotes_and_refuse_line_breaks() {
        assert_eq!(sql_str("it's").unwrap(), "'it''s'");
        assert_eq!(sql_str(r"a\b").unwrap(), r"'a\b'");
        assert!(sql_str("a\nb").is_err());
        assert!(sql_str("a\0b").is_err());
    }

    #[test]
    fn identifiers_are_quoted_and_line_breaks_refused() {
        assert_eq!(sql_ident(r#"we"ird"#).unwrap(), r#""we""ird""#);
        assert!(sql_ident("a\nb").is_err());
    }

    #[test]
    fn reader_follows_the_effective_extension() {
        let (r, k) = reader_sql(Path::new("/d/a.parquet"), false).unwrap();
        assert_eq!(
            (r.as_str(), k),
            ("read_parquet('/d/a.parquet')", Source::View)
        );
        let (r, k) = reader_sql(Path::new("/d/a.json.gz"), false).unwrap();
        assert_eq!(
            (r.as_str(), k),
            ("read_json_auto('/d/a.json.gz')", Source::Table)
        );
        let (r, k) = reader_sql(Path::new("/d/a.csv.gz"), true).unwrap();
        assert_eq!(k, Source::Table);
        assert!(r.starts_with("read_csv('/d/a.csv.gz'") && r.contains("all_varchar=true"));
        let (r, _) = reader_sql(Path::new("/d/it's.tsv"), false).unwrap();
        assert!(r.contains("'/d/it''s.tsv'"));
    }

    #[test]
    fn column_kinds() {
        assert_eq!(column_kind("DECIMAL(18,3)"), "number");
        assert_eq!(column_kind("BIGINT"), "number");
        assert_eq!(column_kind("VARCHAR"), "text");
        assert_eq!(column_kind("TIMESTAMP WITH TIME ZONE"), "temporal");
        assert_eq!(column_kind("BOOLEAN"), "bool");
        assert_eq!(column_kind("INTEGER[]"), "number");
        assert_eq!(column_kind("STRUCT(a INTEGER)"), "other");
    }

    #[test]
    fn view_sql_uses_validated_indexes_and_quoted_literals() {
        let columns = cols(&["id", "na'me"]);
        let spec = ViewSpec {
            sort: vec![SortKey {
                column: 1,
                desc: true,
            }],
            filters: vec![Filter {
                column: 0,
                op: FilterOp::Gt,
                value: Some("1'; DROP TABLE t; --".into()),
            }],
            search: Some("  x  ".into()),
        }
        .normalized();
        let sql = materialize_sql(&spec, &columns).unwrap();
        assert!(sql.contains(r#""id" > '1''; DROP TABLE t; --'"#), "{sql}");
        assert!(sql.contains(r#"ORDER BY "na'me" DESC NULLS LAST"#), "{sql}");
        assert!(sql.contains("lower('x')"), "{sql}");
    }

    #[test]
    fn out_of_range_columns_and_missing_values_are_bad_requests() {
        let columns = cols(&["a"]);
        let bad_sort = ViewSpec {
            sort: vec![SortKey {
                column: 3,
                desc: false,
            }],
            ..Default::default()
        };
        assert!(matches!(
            materialize_sql(&bad_sort, &columns),
            Err(TableError::BadRequest(_))
        ));
        let no_value = ViewSpec {
            filters: vec![Filter {
                column: 0,
                op: FilterOp::Eq,
                value: None,
            }],
            ..Default::default()
        };
        assert!(matches!(
            materialize_sql(&no_value, &columns),
            Err(TableError::BadRequest(_))
        ));
    }

    #[test]
    fn blank_search_normalizes_away() {
        let spec = ViewSpec {
            search: Some("   ".into()),
            ..Default::default()
        }
        .normalized();
        assert!(spec.is_empty());
    }

    #[test]
    fn rows_parse_nulls_and_strings() {
        let lines = vec![r#"{"c0":"1","c1":null}"#.to_string()];
        assert_eq!(
            parse_rows(&lines, 2).unwrap(),
            vec![vec![Some("1".to_string()), None]]
        );
    }

    #[test]
    fn missing_duckdb_message_is_platform_neutral() {
        for word in ["brew", "apt", "winget", "choco", "install"] {
            assert!(!DUCKDB_MISSING_MESSAGE.contains(word), "{word}");
        }
    }

    // ---- against a real duckdb (skipped when none is installed) ----

    fn duckdb_available() -> bool {
        locate_duckdb(Path::new(".")).is_ok()
    }

    fn scratch(name: &str) -> PathBuf {
        let n = NONCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tables-test-{name}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn ready(m: &TableManager, id: &str, file: &Path) -> Arc<Session> {
        let s = m.session(id, file).await.unwrap();
        for _ in 0..200 {
            if !matches!(*s.state.lock().unwrap(), SessionState::Importing) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("import did not finish");
    }

    #[tokio::test]
    async fn csv_import_paging_sort_filter_search() {
        if !duckdb_available() {
            eprintln!("skipping: no duckdb");
            return;
        }
        let dir = scratch("csv");
        let canvas = dir.join("c.canvas.md");
        let file = dir.join("d.csv");
        let mut csv = String::from("id,name,price\n");
        for i in 1..=500 {
            csv.push_str(&format!("{i},item{i},{}\n", i as f64 / 2.0));
        }
        csv.push_str("501,\"multi\nline\",\n");
        std::fs::write(&file, csv).unwrap();

        let m = TableManager::new(&canvas, false);
        let s = ready(&m, "n", &file).await;
        assert_eq!(s.failure(), None);

        let meta = s.meta().await;
        assert_eq!(meta.state, TableState::Ready);
        assert_eq!(meta.total_rows, Some(501));
        assert_eq!(
            meta.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "name", "price"]
        );
        assert_eq!(meta.columns[0].kind, "number");

        let page = s.rows(ViewSpec::default(), 10, 3).await.unwrap();
        assert_eq!(page.matched_rows, Some(501));
        assert_eq!(page.rows.len(), 3);
        assert_eq!(page.rows[0][1].as_deref(), Some("item11"));

        // Sorted descending by price: NULL last, so 500 comes first.
        let sorted = ViewSpec {
            sort: vec![SortKey {
                column: 2,
                desc: true,
            }],
            ..Default::default()
        };
        let page = s.rows(sorted.clone(), 0, 2).await.unwrap();
        assert_eq!(page.rows[0][0].as_deref(), Some("500"));
        let page = s.rows(sorted, 500, 5).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0][2], None);

        // Filter + search.
        let filtered = ViewSpec {
            filters: vec![Filter {
                column: 0,
                op: FilterOp::Ge,
                value: Some("490".into()),
            }],
            ..Default::default()
        };
        let page = s.rows(filtered, 0, 100).await.unwrap();
        assert_eq!(page.matched_rows, Some(12));
        let searched = ViewSpec {
            search: Some("MULTI".into()),
            ..Default::default()
        };
        let page = s.rows(searched, 0, 100).await.unwrap();
        assert_eq!(page.matched_rows, Some(1));
        assert_eq!(page.rows[0][1].as_deref(), Some("multi\nline"));

        // A value the column can't hold is the client's error, not a crash,
        // and the session keeps working afterwards.
        let bad = ViewSpec {
            filters: vec![Filter {
                column: 0,
                op: FilterOp::Eq,
                value: Some("abc".into()),
            }],
            ..Default::default()
        };
        assert!(matches!(
            s.rows(bad, 0, 10).await,
            Err(TableError::Query(_))
        ));
        assert!(s.rows(ViewSpec::default(), 0, 1).await.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn locked_down_process_cannot_read_other_files() {
        if !duckdb_available() {
            eprintln!("skipping: no duckdb");
            return;
        }
        let dir = scratch("lock");
        let file = dir.join("d.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        let m = TableManager::new(&dir.join("c.canvas.md"), false);
        let s = ready(&m, "n", &file).await;
        let mut slot = s.slot.lock().await;
        let proc = slot.proc.as_mut().unwrap();
        let err = proc
            .exec("SELECT * FROM read_csv('/etc/hosts');", QUERY_TIMEOUT)
            .await
            .unwrap_err();
        assert!(err.contains("disabled by configuration"), "{err}");
        let err = proc
            .exec("SET enable_external_access=true;", QUERY_TIMEOUT)
            .await
            .unwrap_err();
        assert!(err.contains("locked"), "{err}");
        drop(slot);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn parquet_is_a_view_and_importing_serves_a_preview() {
        if !duckdb_available() {
            eprintln!("skipping: no duckdb");
            return;
        }
        let dir = scratch("parquet");
        let file = dir.join("p.parquet");
        let mut writer = DuckProc::spawn(&locate_duckdb(Path::new(".")).unwrap(), None).unwrap();
        writer.exec(
            &format!(
                "COPY (SELECT range AS a, 'x' || range AS b FROM range(1000)) TO {} (FORMAT parquet);",
                sql_path(&file).unwrap()
            ),
            QUERY_TIMEOUT,
        )
        .await
        .unwrap();
        writer.kill();

        let m = TableManager::new(&dir.join("c.canvas.md"), false);
        let s = ready(&m, "n", &file).await;
        assert_eq!(s.kind, Source::View);
        let meta = s.meta().await;
        assert_eq!(meta.total_rows, Some(1000));
        // The view isn't copied: the cache holds only a tiny database.
        assert!(dir_size(&s.dir) < 1 << 20);
        let page = s.rows(ViewSpec::default(), 995, 10).await.unwrap();
        assert_eq!(page.rows.len(), 5);
        let sorted = ViewSpec {
            sort: vec![SortKey {
                column: 0,
                desc: true,
            }],
            ..Default::default()
        };
        let page = s.rows(sorted, 0, 1).await.unwrap();
        assert_eq!(page.rows[0][1].as_deref(), Some("x999"));

        // A preview is available from the file alone, before/without an import.
        let direct = s.preview().await.unwrap();
        assert_eq!(direct.rows.len(), PREVIEW_ROWS);
        assert_eq!(direct.columns.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn changed_file_gets_a_fresh_session() {
        if !duckdb_available() {
            eprintln!("skipping: no duckdb");
            return;
        }
        let dir = scratch("change");
        let file = dir.join("d.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        let m = TableManager::new(&dir.join("c.canvas.md"), false);
        let s1 = ready(&m, "n", &file).await;
        assert_eq!(s1.meta().await.total_rows, Some(1));
        std::fs::write(&file, "a\n1\n2\n3\n").unwrap();
        let s2 = ready(&m, "n", &file).await;
        assert_ne!(s1.key, s2.key);
        assert_eq!(s2.meta().await.total_rows, Some(3));
        assert!(!s1.dir.exists(), "stale cache entry is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn oversized_table_is_refused_naming_the_config_key() {
        if !duckdb_available() {
            eprintln!("skipping: no duckdb");
            return;
        }
        let dir = scratch("limit");
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        std::fs::write(
            dir.join(".meshfox/config.toml"),
            "[tables]\ncache_max_bytes = 1000\n",
        )
        .unwrap();
        let file = dir.join("d.csv");
        let mut csv = String::from("a,b\n");
        for i in 0..5000 {
            csv.push_str(&format!("{i},{}\n", i * 7));
        }
        std::fs::write(&file, csv).unwrap();
        let m = TableManager::new(&dir.join("c.canvas.md"), false);
        let s = ready(&m, "n", &file).await;
        let f = s.failure().expect("refused");
        assert_eq!(f.kind, "limit");
        assert!(f.message.contains("cache_max_bytes"), "{}", f.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_duckdb_is_an_expected_failure_in_meta() {
        let dir = scratch("nodb");
        let file = dir.join("d.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        // An explicit, nonexistent path is reported, not silently skipped.
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        std::fs::write(
            dir.join(".meshfox/config.toml"),
            "[tables]\nduckdb_path = \"/nonexistent/duckdb\"\n",
        )
        .unwrap();
        let prev = std::env::var_os("MESHFOX_DUCKDB");
        std::env::remove_var("MESHFOX_DUCKDB");
        let m = TableManager::new(&dir.join("c.canvas.md"), false);
        let meta = m.meta("n", &file).await.unwrap();
        if let Some(p) = prev {
            std::env::set_var("MESHFOX_DUCKDB", p);
        }
        assert_eq!(meta.state, TableState::Failed);
        assert_eq!(meta.error.unwrap().kind, "duckdb-missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_only_cache_lives_in_a_private_per_pid_temp_dir() {
        if !duckdb_available() {
            eprintln!("skipping: no duckdb");
            return;
        }
        let dir = scratch("ro");
        let file = dir.join("d.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        let canvas = dir.join("c.canvas.md");
        let m = TableManager::new(&canvas, true);
        let s = ready(&m, "n", &file).await;
        assert_eq!(s.meta().await.total_rows, Some(1));
        assert!(s.dir.starts_with(std::env::temp_dir()));
        assert!(
            s.dir
                .to_string_lossy()
                .contains(&format!("tables-{}", std::process::id()))
        );
        assert!(
            !dir.join(".meshfox").exists(),
            "nothing written next to the canvas"
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(s.dir.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "owner-only");
        let cache = s.dir.parent().unwrap().to_path_buf();
        drop(s);
        drop(m);
        assert!(!cache.exists(), "removed when the manager goes away");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
