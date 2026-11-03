use crate::{fixtures, harness::TuiSession};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(8);

fn svg(color: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="32"><rect width="80" height="32" fill="{color}"/></svg>"#
    )
}

fn wait_for_color(session: &TuiSession, color: vt100::Color) {
    let deadline = Instant::now() + WAIT;
    loop {
        if (0..28)
            .flat_map(|r| (33..108).map(move |c| (r, c)))
            .any(|(r, c)| session.bgcolor_at(r, c) == Some(color))
        {
            return;
        }
        assert!(Instant::now() < deadline, "SVG did not render {color:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "pty-based e2e"]
fn svg_change_repaints_without_navigation() {
    let (path, dir) = fixtures::write_fixture(
        "<!-- meshfox:canvas -->\n# Image\n<!-- meshfox:node id=\"root\" -->\n\n![plot](image.svg)\n\nafter image\n",
    );
    let image = dir.join("image.svg");
    std::fs::write(&image, svg("#ff0000")).unwrap();
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        "[tui]\nimage_protocol = \"halfblocks\"\n",
    )
    .unwrap();
    let mut session = TuiSession::spawn(&path, dir, 30, 110);
    session.wait_for("after image", WAIT).unwrap();
    wait_for_color(&session, vt100::Color::Rgb(255, 0, 0));
    std::fs::write(&image, svg("#0000ff")).unwrap();
    wait_for_color(&session, vt100::Color::Rgb(0, 0, 255));
    let replacement = image.with_extension("tmp");
    std::fs::write(&replacement, svg("#00ff00")).unwrap();
    std::fs::rename(&replacement, &image).unwrap();
    wait_for_color(&session, vt100::Color::Rgb(0, 255, 0));
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}

fn wait_for_iterm_color(session: &TuiSession, expected: [u8; 4]) {
    use base64::Engine;
    let deadline = Instant::now() + WAIT;
    loop {
        let raw = session.raw_output();
        let raw = String::from_utf8_lossy(&raw);
        for image in raw.split("doNotMoveCursor=1:").skip(1) {
            let Some((payload, _)) = image.split_once('\u{7}') else {
                continue;
            };
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) else {
                continue;
            };
            let Ok(pixels) = image::load_from_memory(&bytes) else {
                continue;
            };
            if pixels.to_rgba8().pixels().any(|pixel| pixel.0 == expected) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "iTerm2 did not receive SVG pixels {expected:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "pty-based e2e"]
fn svg_change_sends_new_iterm_pixels_without_navigation() {
    let (path, dir) = fixtures::write_fixture(
        "<!-- meshfox:canvas -->\n# Image\n<!-- meshfox:node id=\"root\" -->\n\n![plot](docs/power.svg){width=100%}\n\nafter image\n",
    );
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    let image = dir.join("docs/power.svg");
    std::fs::write(&image, svg("#ff0000")).unwrap();
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        "[tui]\nimage_protocol = \"iterm2\"\n",
    )
    .unwrap();
    let mut session = TuiSession::spawn(&path, dir, 30, 110);
    session.wait_for("after image", WAIT).unwrap();
    wait_for_iterm_color(&session, [255, 0, 0, 255]);
    std::fs::write(&image, svg("#0000ff")).unwrap();
    wait_for_iterm_color(&session, [0, 0, 255, 255]);
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}
