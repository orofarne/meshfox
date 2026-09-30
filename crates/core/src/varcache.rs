//! On-disk cache for resolved `meshfox:var` answers — a small,
//! hand-editable, `.gitignore`-able file that remembers what a document's
//! variables were last resolved to, so a canvas only has to ask once
//! (analogous to CMake's `CMakeCache.txt`). See `crate::vars` for
//! declaration/resolution; a `secret` variable is never *automatically*
//! written here (only an explicit opt-in does, e.g. the web UI's "save
//! (plaintext)" checkbox), but is read back from here like any other
//! declaration once it's actually present — see `vars::resolve`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::secret_index::SecretIndex;
use crate::secret_store::{self, SecretBackend, SecretStoreKind};

/// Where the var cache for `canvas_path` lives: a sibling `.meshfox/`
/// directory right next to the canvas file itself, named after the
/// canvas file's own name — e.g. `examples/hello.canvas.md` ->
/// `examples/.meshfox/hello.canvas.md.env`. Colocated with the document,
/// not the current directory, so it stays correct regardless of where
/// meshfox happens to be invoked from.
pub fn cache_path(canvas_path: &Path) -> PathBuf {
    let dir = match canvas_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = canvas_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.join(".meshfox").join(format!("{file_name}.env"))
}

/// A resolved-variable cache, backed by a dotenv-style (`KEY=value` per
/// line) file. Written eagerly — every `set` rewrites the whole file
/// immediately, since the cache is small (a handful of variables) and
/// tool-managed rather than something worth patching surgically like the
/// canvas document itself.
#[derive(Debug, Clone, Default)]
pub struct VarCache {
    /// `None` for an in-memory-only cache (tests) — `set` then updates the
    /// in-memory map but never touches disk.
    path: Option<PathBuf>,
    entries: HashMap<String, String>,
    /// Where a `secret` answer is saved and looked up when
    /// `secret_store = "keychain"` is configured (see `crate::secret_store`);
    /// `None` for the default plaintext behaviour.
    secrets: Option<SecretBinding>,
}

#[derive(Debug, Clone)]
struct SecretBinding {
    backend: Arc<dyn SecretBackend>,
    canvas_path: PathBuf,
    /// Where a save is noted (see `crate::secret_index`); `None` for tests
    /// and when there's no `$HOME`.
    index: Option<SecretIndex>,
}

impl VarCache {
    /// Loads the cache for `canvas_path`, or an empty one if it doesn't
    /// exist yet — nothing is created on disk until the first `set`.
    pub fn load(canvas_path: &Path) -> io::Result<VarCache> {
        let path = cache_path(canvas_path);
        let entries = match std::fs::read_to_string(&path) {
            Ok(contents) => crate::dotenv::parse(&contents),
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e),
        };
        let canvas_root = match canvas_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let secrets = secret_store::backend_for(canvas_root)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .map(|backend| SecretBinding {
                backend,
                canvas_path: canvas_path.to_path_buf(),
                index: SecretIndex::default_location(),
            });
        Ok(VarCache {
            path: Some(path),
            entries,
            secrets,
        })
    }

    /// A cache backed by nothing on disk — for tests.
    pub fn in_memory() -> VarCache {
        VarCache {
            path: None,
            entries: HashMap::new(),
            secrets: None,
        }
    }

    /// Routes `secret` answers through `backend`, as if `secret_store =
    /// "keychain"` were configured — for tests.
    pub fn with_secret_backend(
        mut self,
        backend: Arc<dyn SecretBackend>,
        canvas_path: &Path,
    ) -> VarCache {
        self.secrets = Some(SecretBinding {
            backend,
            canvas_path: canvas_path.to_path_buf(),
            index: None,
        });
        self
    }

    /// Records saves in `index` — for tests of the index wiring.
    pub fn with_secret_index(mut self, index: SecretIndex) -> VarCache {
        if let Some(binding) = &mut self.secrets {
            binding.index = Some(index);
        }
        self
    }

    /// Which store a saved `secret` answer goes to.
    pub fn secret_store_kind(&self) -> SecretStoreKind {
        match self.secrets {
            Some(_) => SecretStoreKind::Keychain,
            None => SecretStoreKind::Plaintext,
        }
    }

    /// A `secret` answer saved in the configured secret store: `Ok(None)`
    /// when there's none (or no store is configured), `Err` when the store
    /// itself couldn't be read (locked keychain, access denied) — callers
    /// treat that as "not there" but should tell the user why.
    pub fn try_get_secret(&self, name: &str) -> io::Result<Option<String>> {
        let Some(binding) = self.secrets.as_ref() else {
            return Ok(None);
        };
        let account = secret_store::doc_account(&binding.canvas_path, name);
        binding.backend.get(&account)
    }

    /// Saves a `secret` answer: to the secret store when one is configured,
    /// otherwise (plaintext) into the cache file like any other answer.
    pub fn save_secret(&mut self, name: &str, value: &str) -> io::Result<()> {
        match &self.secrets {
            Some(binding) => {
                let account = secret_store::doc_account(&binding.canvas_path, name);
                binding.backend.set(&account, value)?;
                if let Some(index) = &binding.index {
                    index
                        .record(&secret_store::doc_scope(&binding.canvas_path), name)
                        .map_err(|e| {
                            io::Error::other(format!("saved, but couldn't note it in the index: {e}"))
                        })?;
                }
                Ok(())
            }
            None => self.set(name, value),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(String::as_str)
    }

    /// Updates `name`, then immediately rewrites the whole file (creating
    /// `.meshfox/` if needed) — a no-op on disk for an in-memory cache.
    pub fn set(&mut self, name: &str, value: &str) -> io::Result<()> {
        self.entries.insert(name.to_string(), value.to_string());
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Sorted by key rather than HashMap's arbitrary order — purely
        // cosmetic (the file is gitignored, not diffed in review), but a
        // stable rewrite is friendlier to a human peeking at it by hand.
        let mut kv: Vec<(&String, &String)> = self.entries.iter().collect();
        kv.sort_by_key(|(k, _)| k.as_str());
        let mut out = String::new();
        for (k, v) in kv {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
        std::fs::write(path, out)
    }

    #[cfg(test)]
    pub(crate) fn values_mut_for_test(&mut self) -> &mut HashMap<String, String> {
        &mut self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_a_sibling_meshfox_dir_named_after_the_file() {
        let p = cache_path(Path::new("examples/hello.canvas.md"));
        assert_eq!(p, PathBuf::from("examples/.meshfox/hello.canvas.md.env"));
    }

    #[test]
    fn cache_path_uses_current_dir_for_a_bare_filename() {
        let p = cache_path(Path::new("README.md"));
        assert_eq!(p, PathBuf::from("./.meshfox/README.md.env"));
    }

    #[test]
    fn missing_cache_file_loads_as_empty() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-varcache-test-{}", std::process::id()));
        let canvas_path = dir.join("nonexistent.canvas.md");
        let cache = VarCache::load(&canvas_path).unwrap();
        assert_eq!(cache.get("X"), None);
    }

    #[test]
    fn set_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("meshfox-varcache-roundtrip-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let canvas_path = dir.join("doc.canvas.md");

        let mut cache = VarCache::load(&canvas_path).unwrap();
        cache.set("INSTALL_PATH", "/usr/local/bin").unwrap();
        cache.set("LOG_LEVEL", "info").unwrap();

        let reloaded = VarCache::load(&canvas_path).unwrap();
        assert_eq!(reloaded.get("INSTALL_PATH"), Some("/usr/local/bin"));
        assert_eq!(reloaded.get("LOG_LEVEL"), Some("info"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_secret_goes_to_the_store_and_not_the_cache_file() {
        let backend = Arc::new(secret_store::MemoryBackend::new());
        let canvas = Path::new("/nonexistent/doc.canvas.md");
        let mut cache = VarCache::in_memory().with_secret_backend(backend.clone(), canvas);
        cache.save_secret("TOKEN", "s3cret").unwrap();
        assert_eq!(cache.try_get_secret("TOKEN").unwrap().as_deref(), Some("s3cret"));
        assert_eq!(cache.get("TOKEN"), None);
        assert_eq!(cache.secret_store_kind(), SecretStoreKind::Keychain);
    }

    #[test]
    fn save_secret_notes_the_save_in_the_index() {
        let dir = std::env::temp_dir().join(format!("meshfox-varcache-index-{}", uid()));
        std::fs::create_dir_all(&dir).unwrap();
        let index = SecretIndex::at(dir.join("secrets.sqlite3"));
        let canvas = dir.join("doc.canvas.md");
        let mut cache = VarCache::in_memory()
            .with_secret_backend(Arc::new(secret_store::MemoryBackend::new()), &canvas)
            .with_secret_index(index.clone());
        cache.save_secret("TOKEN", "v").unwrap();
        let entries = index.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "TOKEN");
        assert_eq!(entries[0].scope, secret_store::doc_scope(&canvas));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_secret_without_a_store_is_a_plain_cache_write() {
        let mut cache = VarCache::in_memory();
        cache.save_secret("TOKEN", "s3cret").unwrap();
        assert_eq!(cache.get("TOKEN"), Some("s3cret"));
        assert_eq!(cache.try_get_secret("TOKEN").unwrap(), None);
        assert_eq!(cache.secret_store_kind(), SecretStoreKind::Plaintext);
    }

    #[test]
    fn an_invalid_secret_store_setting_fails_the_load() {
        let dir = std::env::temp_dir().join(format!("meshfox-varcache-badstore-{}", uid()));
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        std::fs::write(dir.join(".meshfox/config.toml"), "secret_store = \"auto\"\n").unwrap();
        let err = VarCache::load(&dir.join("doc.canvas.md")).unwrap_err();
        assert!(err.to_string().contains("secret_store"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn in_memory_cache_never_touches_disk() {
        let mut cache = VarCache::in_memory();
        cache.set("X", "1").unwrap();
        assert_eq!(cache.get("X"), Some("1"));
    }

    fn uid() -> String {
        format!("{:?}-{}", std::thread::current().id(), std::process::id())
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect()
    }
}
