//! `secret_store = "keychain"` on Linux against a *real* Secret Service.
//! Ignored by default: it needs a running, unlocked keyring on the session
//! bus. `scripts/secret-service-it.sh` sets one up (gnome-keyring inside a
//! private `dbus-run-session`) and runs these.
#![cfg(target_os = "linux")]

use meshfox_core::secret_store::system_backend;

/// Unique per run, so a leftover item from a crashed run can't confuse one.
fn account(name: &str) -> String {
    format!("it:{}:{}/{name}", std::process::id(), name.len())
}

#[test]
#[ignore = "needs a Secret Service; run scripts/secret-service-it.sh"]
fn round_trip_replace_and_delete() {
    let backend = system_backend();
    let acc = account("DB_PASSWORD");

    assert_eq!(backend.get(&acc).unwrap(), None, "starts empty");
    backend.set(&acc, "hunter2").unwrap();
    assert_eq!(backend.get(&acc).unwrap().as_deref(), Some("hunter2"));

    // Setting again replaces rather than adding a second item.
    backend.set(&acc, "correct horse").unwrap();
    assert_eq!(backend.get(&acc).unwrap().as_deref(), Some("correct horse"));

    assert!(backend.delete(&acc).unwrap());
    assert_eq!(backend.get(&acc).unwrap(), None);
    assert!(!backend.delete(&acc).unwrap(), "nothing left to delete");
}

#[test]
#[ignore = "needs a Secret Service; run scripts/secret-service-it.sh"]
fn accounts_do_not_leak_into_each_other() {
    let backend = system_backend();
    let (a, b) = (account("A"), account("BB"));
    backend.set(&a, "one").unwrap();
    backend.set(&b, "two").unwrap();
    assert_eq!(backend.get(&a).unwrap().as_deref(), Some("one"));
    assert_eq!(backend.get(&b).unwrap().as_deref(), Some("two"));
    backend.delete(&a).unwrap();
    assert_eq!(backend.get(&b).unwrap().as_deref(), Some("two"));
    backend.delete(&b).unwrap();
}

#[test]
#[ignore = "needs a Secret Service; run scripts/secret-service-it.sh"]
fn awkward_values_survive() {
    let backend = system_backend();
    let acc = account("AWKWARD");
    let big = "x".repeat(100_000);
    for value in [
        "",
        "пароль ключ 🔑",
        "line one\nline two\n",
        "  padded  ",
        big.as_str(),
    ] {
        backend.set(&acc, value).unwrap();
        assert_eq!(
            backend.get(&acc).unwrap().as_deref(),
            Some(value),
            "value {:?}",
            &value[..value.len().min(20)]
        );
    }
    backend.delete(&acc).unwrap();
}

/// What we store is a normal Secret Service item any other client sees, not
/// something only this code can read back: `secret-tool` (libsecret) finds
/// it by the same attributes.
#[test]
#[ignore = "needs a Secret Service and secret-tool; run scripts/secret-service-it.sh"]
fn items_are_visible_to_other_clients() {
    let backend = system_backend();
    let acc = account("VISIBLE");
    backend.set(&acc, "shared-secret").unwrap();

    let out = std::process::Command::new("secret-tool")
        .args(["lookup", "service", "meshfox", "account", &acc])
        .output()
        .expect("secret-tool (package libsecret-tools) is needed");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "shared-secret");

    backend.delete(&acc).unwrap();
}

/// Run with `DBUS_SESSION_BUS_ADDRESS` pointing at nothing (the script
/// does): the failure must be an error naming the cause, never a silent
/// success or a hang.
#[test]
#[ignore = "run by scripts/secret-service-it.sh with no session bus"]
fn no_secret_service_is_a_loud_error() {
    let backend = system_backend();
    for err in [
        backend.get("it:none").unwrap_err(),
        backend.set("it:none", "v").unwrap_err(),
        backend.delete("it:none").unwrap_err(),
    ] {
        assert!(err.to_string().contains("secret service"), "{err}");
    }
}
