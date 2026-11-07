//! `meshfox configure` against a real worker, driven through a pty: the
//! questions come from the worker's own configure listing (the one the web
//! UI and the TUI use), so a `select` fed by a `choices_var=` source gets its
//! real, computed choices, can be filtered as you type, and one whose source
//! failed is skipped with the reason instead of being saved empty.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"FONT_LIST\" from=\"root/list-fonts\" -->\n",
    "<!-- meshfox:var name=\"FONT\" type=\"select\" choices_var=\"FONT_LIST\" prompt=\"Font\" -->\n",
    "<!-- meshfox:var name=\"BROKEN_LIST\" from=\"root/list-broken\" -->\n",
    "<!-- meshfox:var name=\"PICK\" type=\"select\" choices_var=\"BROKEN_LIST\" prompt=\"Pick\" -->\n",
    "<!-- meshfox:var name=\"NAME\" prompt=\"Name\" default=\"x\" -->\n\n",
    "```bash name=\"list-fonts\"\n",
    "echo \"FONT_LIST=Arial,Arial Black,Courier New,DejaVu Sans,DejaVu Sans Mono,Fira Code,Georgia,\\",
    "Helvetica,Helvetica Neue,Menlo,Monaco,Noto Mono,Noto Sans,Noto Serif,Times New Roman,Verdana,\\",
    "Zapfino,Zilla Slab,Ubuntu,Optima\" >> \"$MESHFOX_VARS_OUT\"\n```\n\n",
    "```bash name=\"list-broken\"\necho nope >&2\nexit 3\n```\n",
);

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-configure-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::write(dir.join("doc.canvas.md"), CANVAS).unwrap();
    dir.canonicalize().unwrap()
}

/// Reads the pty until `needle` shows up in what has been read since the
/// last call (so each wait looks only at new output).
fn read_until(reader: &mut dyn Read, seen: &mut String, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline && !seen.contains(needle) {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
    }
    assert!(
        seen.contains(needle),
        "never saw {needle:?}; output so far:\n{seen}"
    );
    // Keep only what follows, so the next wait cannot match this text again.
    let end = seen.find(needle).unwrap() + needle.len();
    *seen = seen[end..].to_string();
}

#[test]
fn configure_filters_a_long_computed_select_and_skips_a_pending_one() {
    #[cfg(target_os = "macos")]
    {
        static SIGN_ONCE: std::sync::Once = std::sync::Once::new();
        SIGN_ONCE.call_once(|| {
            let status = std::process::Command::new("codesign")
                .args(["--force", "-s", "-", env!("CARGO_BIN_EXE_meshfox")])
                .status()
                .expect("run codesign");
            assert!(
                status.success(),
                "codesign failed to re-sign the meshfox binary"
            );
        });
    }

    let dir = unique_dir();
    let canvas = dir.join("doc.canvas.md");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.args(["configure", "--canvas"]);
    cmd.arg(&canvas);
    cmd.env("HOME", dir.join("home"));
    cmd.env("MESHFOX_SERVER_SOCKET", "");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn meshfox configure");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let mut seen = String::new();
    let mut send = |text: &str| {
        writer.write_all(text.as_bytes()).unwrap();
        writer.flush().unwrap();
    };

    // 20 computed choices: more than a list shows, so the prompt asks for a filter.
    read_until(
        &mut reader,
        &mut seen,
        "and 5 more: type part of a name to filter",
    );
    read_until(&mut reader, &mut seen, "filter, name or number > ");
    // "hel" narrows to two; the list is shown again and numbers refer to it.
    send("hel\r");
    read_until(&mut reader, &mut seen, "(2 of 20 match)");
    read_until(&mut reader, &mut seen, "filter, name or number > ");
    send("2\r");

    // The select whose source failed is skipped, with the reason.
    read_until(
        &mut reader,
        &mut seen,
        "Pick: skipped, its choices come from BROKEN_LIST",
    );
    read_until(&mut reader, &mut seen, "exited with status 3: nope");

    read_until(&mut reader, &mut seen, "Name [x]: ");
    send("\r");
    read_until(&mut reader, &mut seen, "saved 2 variable(s)");
    let status = child.wait().expect("configure exits");
    assert!(status.success(), "configure failed: {status:?}");

    let cache = std::fs::read_to_string(meshfox_core::varcache::cache_path(&canvas)).unwrap();
    assert!(cache.contains("FONT=Helvetica Neue"), "cache was:\n{cache}");
    assert!(cache.contains("NAME=x"), "cache was:\n{cache}");
    assert!(
        !cache.contains("PICK"),
        "a pending select must not be saved:\n{cache}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
