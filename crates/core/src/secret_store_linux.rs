//! `secret_store = "keychain"` on Linux: the freedesktop Secret Service
//! (`org.freedesktop.secrets` on the session bus — gnome-keyring, KWallet,
//! oo7-daemon, KeePassXC, ...) through the `secret-service` crate's
//! blocking API, in the default collection.
//!
//! Each call opens its own connection (a few milliseconds, one DH key
//! exchange) so there is no state to keep alive or reconnect, like the
//! one-shot `security-framework` calls on macOS. Every failure is a loud
//! `io::Error` naming the cause: no secret service on the bus, a locked
//! collection that needs an unlock prompt nobody can answer, a dismissed
//! prompt. Nothing falls back to plaintext.

use std::collections::HashMap;
use std::io;

use secret_service::blocking::{Collection, SecretService};
use secret_service::{EncryptionType, Error};

use super::{SecretBackend, SERVICE};

#[derive(Debug)]
pub(super) struct SecretServiceBackend;

/// The attributes an item is stored and looked up by.
fn attributes(account: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", SERVICE), ("account", account)])
}

fn label(account: &str) -> String {
    format!("{SERVICE}: {account}")
}

fn map_err(e: Error) -> io::Error {
    let hint = match &e {
        Error::Unavailable => {
            " (is a Secret Service running on the session bus — gnome-keyring, KWallet or \
             oo7-daemon — and is DBUS_SESSION_BUS_ADDRESS set?)"
        }
        Error::Locked | Error::Prompt | Error::PromptDisconnected => {
            " (unlock the default keyring first, e.g. log in graphically or unlock it from \
             a keyring manager)"
        }
        _ => "",
    };
    io::Error::other(format!("secret service: {e}{hint}"))
}

/// Connects, opens the default collection, unlocks it if needed (which may
/// raise a prompt), and runs `f` on it.
fn with_collection<T>(f: impl FnOnce(&Collection<'_>) -> Result<T, Error>) -> io::Result<T> {
    let service = SecretService::connect(EncryptionType::Dh).map_err(map_err)?;
    let collection = service.get_default_collection().map_err(map_err)?;
    collection.ensure_unlocked().map_err(map_err)?;
    f(&collection).map_err(map_err)
}

impl SecretBackend for SecretServiceBackend {
    fn get(&self, account: &str) -> io::Result<Option<String>> {
        with_collection(|collection| {
            let items = collection.search_items(attributes(account))?;
            let Some(item) = items.first() else {
                return Ok(None);
            };
            item.ensure_unlocked()?;
            item.get_secret().map(Some)
        })?
        .map(|bytes| {
            String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        })
        .transpose()
    }

    fn set(&self, account: &str, value: &str) -> io::Result<()> {
        with_collection(|collection| {
            collection
                .create_item(
                    &label(account),
                    attributes(account),
                    value.as_bytes(),
                    true,
                    "text/plain",
                )
                .map(|_| ())
        })
    }

    fn delete(&self, account: &str) -> io::Result<bool> {
        with_collection(|collection| {
            let items = collection.search_items(attributes(account))?;
            for item in &items {
                item.delete()?;
            }
            Ok(!items.is_empty())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_found_by_service_and_account() {
        let attrs = attributes("doc:/p/x.canvas.md/DB");
        assert_eq!(attrs.get("service"), Some(&"meshfox"));
        assert_eq!(attrs.get("account"), Some(&"doc:/p/x.canvas.md/DB"));
        assert_eq!(attrs.len(), 2);
    }

    #[test]
    fn labels_name_the_account() {
        assert_eq!(label("env:global/DB"), "meshfox: env:global/DB");
    }

    #[test]
    fn an_unavailable_service_is_a_loud_actionable_error() {
        let e = map_err(Error::Unavailable);
        let text = e.to_string();
        assert!(text.contains("secret service"), "{text}");
        assert!(text.contains("DBUS_SESSION_BUS_ADDRESS"), "{text}");
    }

    #[test]
    fn a_locked_collection_says_how_to_unlock() {
        let text = map_err(Error::Locked).to_string();
        assert!(text.contains("unlock"), "{text}");
    }
}
