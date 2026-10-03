//! `meshfox secret set|show|rm|list|prune` — manage values in the system
//! secret store (`secret_store = "keychain"`, see `meshfox_core::secret_store`)
//! and the local index of what's in it (`meshfox_core::secret_index`).
//! Values are only ever printed by `show --reveal`.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use meshfox_core::secret_index::{self, SecretIndex, SecretReport, SecretStatus};
use meshfox_core::secret_store::{self, SecretBackend};
use meshfox_core::shared_env;

#[derive(clap::Args, Debug)]
pub struct SecretScope {
    /// A variable's saved answer for this one canvas (the default scope; the
    /// canvas is auto-discovered when none of the scope flags is given).
    #[arg(long, value_name = "CANVAS", conflicts_with_all = ["global", "project"])]
    canvas: Option<PathBuf>,
    /// A `secrets = [...]` entry of a `[[env]]` section in
    /// `~/.meshfox/config.toml`.
    #[arg(long, conflicts_with = "project")]
    global: bool,
    /// With `--global`: the section's `path=` exactly as written there
    /// (omit for an unscoped section).
    #[arg(long, value_name = "PATH", requires = "global")]
    path: Option<String>,
    /// A `secrets = [...]` entry of a `[[env]]` section in
    /// `<DIR>/.meshfox/config.toml`.
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,
}

#[derive(clap::Subcommand, Debug)]
pub enum SecretOp {
    /// Store a value. Read from a hidden prompt, or from stdin when stdin
    /// isn't a terminal (trailing newline stripped).
    Set {
        name: String,
        #[command(flatten)]
        scope: SecretScope,
    },
    /// Say whether a value is stored; print it only with `--reveal`.
    Show {
        name: String,
        #[arg(long)]
        reveal: bool,
        #[command(flatten)]
        scope: SecretScope,
    },
    /// Delete a stored value (and its index entry).
    Rm {
        name: String,
        #[command(flatten)]
        scope: SecretScope,
    },
    /// List what meshfox has stored, from the local index
    /// (`~/.meshfox/secrets.sqlite3`) — names and status only, never
    /// values. Status: `ok`; `orphan-decl` (the file no longer declares
    /// it); `orphan-path` (the canvas/project directory is gone — or was
    /// moved or renamed); `unknown` (a file that should say couldn't be
    /// read).
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Delete every orphaned secret (`orphan-decl` and `orphan-path`) from
    /// the keychain and the index. Only shows what it would delete unless
    /// `--yes` is given. `unknown` entries are never touched.
    Prune {
        #[arg(long)]
        yes: bool,
    },
}

pub fn run(op: SecretOp, find_canvas: impl FnOnce() -> PathBuf) -> Result<(), String> {
    match op {
        SecretOp::Set { name, scope } => {
            let t = resolve(&scope, &name, find_canvas)?;
            let value = read_value(&name)?;
            t.backend
                .set(&t.account(), &value)
                .map_err(|e| e.to_string())?;
            index()?
                .record(&t.scope, &name)
                .map_err(|e| format!("stored, but couldn't note {name} in the index: {e}"))?;
            println!("stored {name}");
        }
        SecretOp::Show {
            name,
            reveal,
            scope,
        } => {
            let t = resolve(&scope, &name, find_canvas)?;
            match t.backend.get(&t.account()).map_err(|e| e.to_string())? {
                Some(v) if reveal => println!("{v}"),
                Some(_) => println!("{name}: set"),
                None => println!("{name}: unset"),
            }
        }
        SecretOp::Rm { name, scope } => {
            let t = resolve(&scope, &name, find_canvas)?;
            let removed = secret_index::remove(&index()?, t.backend.as_ref(), &t.scope, &name)
                .map_err(|e| e.to_string())?;
            if removed {
                println!("removed {name}");
            } else {
                println!("{name}: nothing stored");
            }
        }
        SecretOp::List { json } => {
            let reports = secret_index::report(&index()?).map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reports_json(&reports)).unwrap()
                );
            } else if reports.is_empty() {
                println!("no secrets recorded");
            } else {
                for r in &reports {
                    println!(
                        "{:<12} {:<20} {}  ({})",
                        r.status.as_str(),
                        r.entry.name,
                        r.entry.scope,
                        r.entry.created_at
                    );
                    if let SecretStatus::Unknown(why) = &r.status {
                        println!("             {why}");
                    }
                }
            }
        }
        SecretOp::Prune { yes } => prune(yes)?,
    }
    Ok(())
}

pub fn reports_json(reports: &[SecretReport]) -> serde_json::Value {
    serde_json::Value::Array(
        reports
            .iter()
            .map(|r| {
                let mut v = serde_json::json!({
                    "scope": r.entry.scope,
                    "name": r.entry.name,
                    "createdAt": r.entry.created_at,
                    "status": r.status.as_str(),
                });
                if let SecretStatus::Unknown(why) = &r.status {
                    v["reason"] = serde_json::Value::String(why.clone());
                }
                v
            })
            .collect(),
    )
}

fn index() -> Result<SecretIndex, String> {
    SecretIndex::default_location()
        .ok_or_else(|| "HOME isn't set — nowhere to keep the index".to_string())
}

fn prune(yes: bool) -> Result<(), String> {
    let index = index()?;
    let reports = secret_index::report(&index).map_err(|e| e.to_string())?;
    let (by_path, by_decl): (Vec<_>, Vec<_>) = reports
        .iter()
        .filter(|r| r.status.is_orphan())
        .partition(|r| r.status == SecretStatus::OrphanPath);
    if by_path.is_empty() && by_decl.is_empty() {
        println!("nothing to prune");
        return Ok(());
    }
    let describe = |title: &str, group: &[&SecretReport]| {
        if !group.is_empty() {
            println!("{title}:");
            for r in group {
                println!("  {}  {}", r.entry.name, r.entry.scope);
            }
        }
    };
    describe("no longer declared", &by_decl);
    describe(
        "file or directory not found (deleted — or moved/renamed, which looks the same)",
        &by_path,
    );
    if !yes {
        println!("dry run — pass --yes to delete these from the keychain and the index");
        return Ok(());
    }
    let backend = secret_store::system_backend();
    let mut failed = 0;
    for r in by_decl.iter().chain(by_path.iter()) {
        match secret_index::remove(&index, backend.as_ref(), &r.entry.scope, &r.entry.name) {
            Ok(_) => println!("deleted {}  {}", r.entry.name, r.entry.scope),
            Err(e) => {
                failed += 1;
                eprintln!("couldn't delete {}  {}: {e}", r.entry.name, r.entry.scope);
            }
        }
    }
    if failed > 0 {
        return Err(format!("{failed} secret(s) could not be deleted"));
    }
    Ok(())
}

struct Target {
    backend: std::sync::Arc<dyn SecretBackend>,
    scope: String,
    name: String,
}

impl Target {
    fn account(&self) -> String {
        secret_store::account_for(&self.scope, &self.name)
    }
}

/// The configured backend plus the index scope for `name`. Errors (rather
/// than doing nothing) when `secret_store` isn't `keychain`.
fn resolve(
    scope: &SecretScope,
    name: &str,
    find_canvas: impl FnOnce() -> PathBuf,
) -> Result<Target, String> {
    let (config_root, index_scope): (PathBuf, String) = if scope.global {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        (
            root,
            shared_env::global_account_scope(scope.path.as_deref()),
        )
    } else if let Some(dir) = &scope.project {
        (dir.clone(), shared_env::project_account_scope(dir))
    } else {
        let canvas = scope.canvas.clone().unwrap_or_else(find_canvas);
        let root = canvas
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        (root, secret_store::doc_scope(&canvas))
    };
    let backend = secret_store::backend_for(&config_root)?.ok_or_else(|| {
        "secret_store is not set to \"keychain\" in .meshfox/config.toml — nothing to manage"
            .to_string()
    })?;
    Ok(Target {
        backend,
        scope: index_scope,
        name: name.to_string(),
    })
}

fn read_value(name: &str) -> Result<String, String> {
    if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("{name}: ")).map_err(|e| e.to_string())
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| e.to_string())?;
        Ok(buf
            .strip_suffix('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s))
            .unwrap_or(&buf)
            .to_string())
    }
}
