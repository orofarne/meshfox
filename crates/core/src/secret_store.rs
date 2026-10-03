//! Optional system secret store for `secret` variables — the alternative
//! to writing a value to the plaintext `.meshfox/<file>.env` cache (or to a
//! `[[env]]` section's `vars`). Chosen explicitly, never automatically:
//!
//! ```toml
//! secret_store = "keychain"   # or "plaintext" (the default)
//! ```
//!
//! in `.meshfox/config.toml` (local or global, same local-wins load as any
//! other setting). `"keychain"` is the macOS Keychain, or on Linux the
//! freedesktop Secret Service (`secret_store_linux`); asking for it on any
//! other platform, or without a reachable Secret Service, is a loud error on
//! first use, never a silent fallback to plaintext. Values live under the service [`SERVICE`], keyed by
//! an *account* string built by [`doc_account`] (a document's own saved
//! answers) or [`env_account`] (a `[[env]]` section's `secrets = [...]`).
//!
//! A value written by hand — in the cache file or a `vars` table — always
//! beats what the store holds (see `crate::vars::resolve` and
//! `crate::shared_env`): choosing the keychain doesn't take that option
//! away from the user.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Keychain "service" name every meshfox item is filed under.
pub const SERVICE: &str = "meshfox";

/// Which backend `secret_store =` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreKind {
    Plaintext,
    Keychain,
}

impl SecretStoreKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretStoreKind::Plaintext => "plaintext",
            SecretStoreKind::Keychain => "keychain",
        }
    }
}

/// A place secrets can be put and fetched by account string.
pub trait SecretBackend: std::fmt::Debug + Send + Sync {
    /// `Ok(None)` for "no such item" — anything else (locked keychain,
    /// access denied, unsupported platform) is an `Err`.
    fn get(&self, account: &str) -> io::Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> io::Result<()>;
    /// `Ok(false)` when there was nothing to delete.
    fn delete(&self, account: &str) -> io::Result<bool>;
}

/// The `secret_store` setting for the project rooted at `canvas_root`.
/// Absent means `plaintext`; any other unrecognised value is an error, so a
/// typo can't silently leave secrets unprotected.
pub fn configured_kind(canvas_root: &Path) -> Result<SecretStoreKind, String> {
    kind_from_table(&crate::config::load(canvas_root))
}

fn kind_from_table(table: &toml::Table) -> Result<SecretStoreKind, String> {
    match table.get("secret_store") {
        None => Ok(SecretStoreKind::Plaintext),
        Some(toml::Value::String(s)) => match s.as_str() {
            "plaintext" => Ok(SecretStoreKind::Plaintext),
            "keychain" => Ok(SecretStoreKind::Keychain),
            other => Err(format!(
                "unknown secret_store {other:?} (expected \"plaintext\" or \"keychain\")"
            )),
        },
        Some(_) => Err("secret_store must be a string".to_string()),
    }
}

/// The backend for a project's configured store: `None` for `plaintext`.
pub fn backend_for(canvas_root: &Path) -> Result<Option<Arc<dyn SecretBackend>>, String> {
    Ok(match configured_kind(canvas_root)? {
        SecretStoreKind::Plaintext => None,
        SecretStoreKind::Keychain => Some(system_backend()),
    })
}

/// The OS keychain backend: the macOS Keychain, the Linux Secret Service,
/// or an always-failing stub elsewhere.
pub fn system_backend() -> Arc<dyn SecretBackend> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::SecretServiceBackend)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(SystemKeychain)
    }
}

#[cfg(target_os = "linux")]
#[path = "secret_store_linux.rs"]
mod linux;

/// Account for one variable's saved answer in one document. `canvas_path`
/// is canonicalized when possible, so different spellings of the same file
/// agree.
pub fn doc_account(canvas_path: &Path, name: &str) -> String {
    account_for(&doc_scope(canvas_path), name)
}

pub fn doc_scope(canvas_path: &Path) -> String {
    let canonical =
        std::fs::canonicalize(canvas_path).unwrap_or_else(|_| canvas_path.to_path_buf());
    format!("doc:{}", canonical.display())
}

/// Account for `name` under a scope as the index stores it: `doc:<path>`
/// scopes are a document's saved answers, anything else an `[[env]]`
/// section's `secrets`.
pub fn account_for(scope: &str, name: &str) -> String {
    if scope.starts_with("doc:") {
        format!("{scope}/{name}")
    } else {
        env_account(scope, name)
    }
}

/// Account for a `secrets = [...]` entry of an `[[env]]` section. `scope`
/// is `global`, `global:<raw path=>` or `project:<canvas_root>` — see
/// `crate::shared_env`.
pub fn env_account(scope: &str, name: &str) -> String {
    format!("env:{scope}/{name}")
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct SystemKeychain;

#[cfg(target_os = "macos")]
impl SecretBackend for SystemKeychain {
    fn get(&self, account: &str) -> io::Result<Option<String>> {
        // errSecItemNotFound
        const NOT_FOUND: i32 = -25300;
        match security_framework::passwords::get_generic_password(SERVICE, account) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(io::Error::other(format!("keychain: {e}"))),
        }
    }

    fn set(&self, account: &str, value: &str) -> io::Result<()> {
        security_framework::passwords::set_generic_password(SERVICE, account, value.as_bytes())
            .map_err(|e| io::Error::other(format!("keychain: {e}")))
    }

    fn delete(&self, account: &str) -> io::Result<bool> {
        const NOT_FOUND: i32 = -25300;
        match security_framework::passwords::delete_generic_password(SERVICE, account) {
            Ok(()) => Ok(true),
            Err(e) if e.code() == NOT_FOUND => Ok(false),
            Err(e) => Err(io::Error::other(format!("keychain: {e}"))),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl SecretBackend for SystemKeychain {
    fn get(&self, _: &str) -> io::Result<Option<String>> {
        Err(unsupported())
    }
    fn set(&self, _: &str, _: &str) -> io::Result<()> {
        Err(unsupported())
    }
    fn delete(&self, _: &str) -> io::Result<bool> {
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "secret_store = \"keychain\" is only implemented on macOS and Linux so far",
    )
}

/// In-process backend, for tests (in this crate and in dependents).
#[derive(Debug, Default)]
pub struct MemoryBackend {
    items: Mutex<HashMap<String, String>>,
}

impl MemoryBackend {
    pub fn new() -> MemoryBackend {
        MemoryBackend::default()
    }
}

impl SecretBackend for MemoryBackend {
    fn get(&self, account: &str) -> io::Result<Option<String>> {
        Ok(self.items.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> io::Result<()> {
        self.items
            .lock()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, account: &str) -> io::Result<bool> {
        Ok(self.items.lock().unwrap().remove(account).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn absent_setting_means_plaintext() {
        assert_eq!(kind_from_table(&table("")), Ok(SecretStoreKind::Plaintext));
    }

    #[test]
    fn both_known_values_parse() {
        assert_eq!(
            kind_from_table(&table("secret_store = \"plaintext\"")),
            Ok(SecretStoreKind::Plaintext)
        );
        assert_eq!(
            kind_from_table(&table("secret_store = \"keychain\"")),
            Ok(SecretStoreKind::Keychain)
        );
    }

    #[test]
    fn an_unknown_or_mistyped_value_is_an_error_not_a_fallback() {
        assert!(kind_from_table(&table("secret_store = \"auto\"")).is_err());
        assert!(kind_from_table(&table("secret_store = 3")).is_err());
    }

    #[test]
    fn memory_backend_round_trips() {
        let b = MemoryBackend::new();
        assert_eq!(b.get("a").unwrap(), None);
        b.set("a", "1").unwrap();
        assert_eq!(b.get("a").unwrap().as_deref(), Some("1"));
        assert!(b.delete("a").unwrap());
        assert!(!b.delete("a").unwrap());
    }

    #[test]
    fn accounts_are_scoped_by_kind_and_name() {
        assert_eq!(env_account("global", "DB"), "env:global/DB");
        assert!(doc_account(Path::new("/nonexistent/x.canvas.md"), "DB")
            .starts_with("doc:/nonexistent/x.canvas.md/DB"));
    }
}
