//! `meshfox secret set|show|rm` — manage values in the system secret store
//! (`secret_store = "keychain"`, see `meshfox_core::secret_store`). Values
//! are only ever printed by `show --reveal`.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

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
    /// Delete a stored value.
    Rm {
        name: String,
        #[command(flatten)]
        scope: SecretScope,
    },
}

pub fn run(op: SecretOp, find_canvas: impl FnOnce() -> PathBuf) -> Result<(), String> {
    match op {
        SecretOp::Set { name, scope } => {
            let (backend, account) = resolve(&scope, &name, find_canvas)?;
            let value = read_value(&name)?;
            backend.set(&account, &value).map_err(|e| e.to_string())?;
            println!("stored {name}");
        }
        SecretOp::Show {
            name,
            reveal,
            scope,
        } => {
            let (backend, account) = resolve(&scope, &name, find_canvas)?;
            match backend.get(&account).map_err(|e| e.to_string())? {
                Some(v) if reveal => println!("{v}"),
                Some(_) => println!("{name}: set"),
                None => println!("{name}: unset"),
            }
        }
        SecretOp::Rm { name, scope } => {
            let (backend, account) = resolve(&scope, &name, find_canvas)?;
            if backend.delete(&account).map_err(|e| e.to_string())? {
                println!("removed {name}");
            } else {
                println!("{name}: nothing stored");
            }
        }
    }
    Ok(())
}

/// The configured backend plus the account for `name` in `scope`. Errors
/// (rather than doing nothing) when `secret_store` isn't `keychain`.
fn resolve(
    scope: &SecretScope,
    name: &str,
    find_canvas: impl FnOnce() -> PathBuf,
) -> Result<(std::sync::Arc<dyn SecretBackend>, String), String> {
    let (config_root, account): (PathBuf, String) = if scope.global {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let s = shared_env::global_account_scope(scope.path.as_deref());
        (root, secret_store::env_account(&s, name))
    } else if let Some(dir) = &scope.project {
        let s = shared_env::project_account_scope(dir);
        (dir.clone(), secret_store::env_account(&s, name))
    } else {
        let canvas = scope.canvas.clone().unwrap_or_else(find_canvas);
        let root = canvas
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let account = secret_store::doc_account(&canvas, name);
        (root, account)
    };
    let backend = secret_store::backend_for(&config_root)?.ok_or_else(|| {
        "secret_store is not set to \"keychain\" in .meshfox/config.toml — nothing to manage"
            .to_string()
    })?;
    Ok((backend, account))
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
