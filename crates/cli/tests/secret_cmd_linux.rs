//! `meshfox secret set|show|rm|list` on Linux, through the real CLI against a
//! real Secret Service. Ignored by default: it needs an unlocked keyring on
//! the session bus and `secret-tool`; `scripts/secret-service-it.sh` sets
//! both up and runs it.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn project() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("mfx-secret-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        "secret_store = \"keychain\"\n",
    )
    .unwrap();
    let canvas = dir.join("doc.canvas.md");
    std::fs::write(
        &canvas,
        "<!-- meshfox:canvas -->\n# Doc\n\
         <!-- meshfox:var name=\"API_TOKEN\" secret -->\n\
         <!-- meshfox:node id=\"doc\" -->\n\n\
         ```bash name=\"show\" env=\"API_TOKEN\"\necho \"token=$API_TOKEN\"\n```\n",
    )
    .unwrap();
    (dir.canonicalize().unwrap(), canvas.canonicalize().unwrap())
}

fn meshfox(home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Keep the secret index (and any config lookup) out of the real home.
    cmd.env("HOME", home).current_dir(home).args(args);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
#[ignore = "needs a Secret Service and secret-tool; run scripts/secret-service-it.sh"]
fn secret_set_show_run_and_rm_through_the_cli() {
    let (home, canvas) = project();
    let canvas_arg = canvas.to_str().unwrap();

    let o = meshfox(
        &home,
        &["secret", "show", "API_TOKEN", "--canvas", canvas_arg],
        None,
    );
    assert!(o.status.success(), "{o:?}");
    assert_eq!(out(&o), "API_TOKEN: unset\n");

    let o = meshfox(
        &home,
        &["secret", "set", "API_TOKEN", "--canvas", canvas_arg],
        Some("s3cret ключ\n"),
    );
    assert!(o.status.success(), "{o:?}");
    assert_eq!(out(&o), "stored API_TOKEN\n");

    let o = meshfox(
        &home,
        &["secret", "show", "API_TOKEN", "--canvas", canvas_arg],
        None,
    );
    assert_eq!(out(&o), "API_TOKEN: set\n", "no --reveal, no value");
    let o = meshfox(
        &home,
        &[
            "secret",
            "show",
            "API_TOKEN",
            "--reveal",
            "--canvas",
            canvas_arg,
        ],
        None,
    );
    assert_eq!(out(&o), "s3cret ключ\n");

    // Stored in the real Secret Service, where another client finds it.
    let account = format!("doc:{}/API_TOKEN", canvas.display());
    let lookup = Command::new("secret-tool")
        .args(["lookup", "service", "meshfox", "account", &account])
        .output()
        .expect("secret-tool (package libsecret-tools) is needed");
    assert_eq!(String::from_utf8_lossy(&lookup.stdout), "s3cret ключ");

    let o = meshfox(&home, &["secret", "list"], None);
    assert!(out(&o).contains("API_TOKEN"), "{o:?}");
    assert!(out(&o).starts_with("ok"), "{o:?}");

    // A run picks the value up from the keychain without asking.
    let o = meshfox(&home, &[canvas_arg, "run", "show"], None);
    assert!(o.status.success(), "{o:?}");
    assert!(out(&o).contains("token=s3cret ключ"), "{o:?}");

    let o = meshfox(
        &home,
        &["secret", "rm", "API_TOKEN", "--canvas", canvas_arg],
        None,
    );
    assert_eq!(out(&o), "removed API_TOKEN\n");
    let o = meshfox(
        &home,
        &["secret", "rm", "API_TOKEN", "--canvas", canvas_arg],
        None,
    );
    assert_eq!(out(&o), "API_TOKEN: nothing stored\n");
    let o = meshfox(
        &home,
        &["secret", "show", "API_TOKEN", "--canvas", canvas_arg],
        None,
    );
    assert_eq!(out(&o), "API_TOKEN: unset\n");
    let o = meshfox(&home, &["secret", "list"], None);
    assert_eq!(out(&o), "no secrets recorded\n");

    let _ = std::fs::remove_dir_all(&home);
}

/// With no Secret Service reachable the CLI says so and stores nothing; it
/// never falls back to the plaintext cache.
#[test]
#[ignore = "run by scripts/secret-service-it.sh with no session bus"]
fn secret_commands_fail_loudly_without_a_secret_service() {
    let (home, canvas) = project();
    let canvas_arg = canvas.to_str().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.env("HOME", &home)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .current_dir(&home)
        .args(["secret", "show", "API_TOKEN", "--canvas", canvas_arg]);
    let o = cmd.output().unwrap();
    assert!(!o.status.success(), "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("secret service"),
        "{o:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
