use std::time::Duration;

use crate::{fixtures, harness::TuiSession};

#[test]
#[ignore = "pty-based e2e — run via cargo test --test tui_e2e -- --ignored"]
fn tty_detaches_and_reconnects_without_stopping_or_restarting_the_process() {
    let (path, dir) = fixtures::write_fixture(
        r#"<!-- meshfox:canvas -->
# Root
<!-- meshfox:node id="root" -->
```python name=terminal interpreter=python3 tty
print("TTY-READY", flush=True)
for n in range(1, 4):
    text = input()
    print(f"REPLY-{n}={text}", flush=True)
```
"#,
    );
    let mut session = TuiSession::spawn(&path, dir, 30, 120);
    let timeout = Duration::from_secs(10);
    session.wait_for("Root", timeout).unwrap();
    session.send_keys("r");
    session.wait_for("Ctrl-] to return", timeout).unwrap();
    session.wait_for("TTY-READY", timeout).unwrap();
    for n in 1..=2 {
        session.send_keys(&format!("hello{n}\r"));
        session
            .wait_for(&format!("REPLY-{n}=hello{n}"), timeout)
            .unwrap();
        session.send_keys("\x1d");
        session.wait_for("Root", timeout).unwrap();
        assert!(session.in_alternate_screen());
        session.send_keys("t");
        session.wait_for("live terminals", timeout).unwrap();
        session.wait_for("terminal", timeout).unwrap();
        session.clear_raw();
        session.send_keys("\r");
        session.wait_for_raw("Ctrl-] to return", timeout);
        session.wait_for_raw(&format!("REPLY-{n}=hello{n}"), timeout);
        assert!(!session.in_alternate_screen());
    }
    session.send_keys("last\r");
    session.wait_for("REPLY-3=last", timeout).unwrap();
    session.wait_for("Completed · exit 0", timeout).unwrap();
    session.send_keys(" ");
    session.wait_for("Root", timeout).unwrap();
    assert!(session.in_alternate_screen());
}

#[test]
#[ignore = "pty-based e2e — run via cargo test --test tui_e2e -- --ignored"]
fn tty_tracks_terminal_size_while_output_is_continuous() {
    let (path, dir) = fixtures::write_fixture(
        r#"<!-- meshfox:canvas -->
# Root
<!-- meshfox:node id="root" -->
```python name=terminal interpreter=python3 tty autoclose
import os, time
for _ in range(500):
    size = os.get_terminal_size()
    print(f"PTY-SIZE={size.columns}x{size.lines}", flush=True)
    time.sleep(0.02)
```
"#,
    );
    let mut session = TuiSession::spawn(&path, dir, 30, 100);
    session.wait_for("Root", Duration::from_secs(10)).unwrap();
    session.send_keys("r");
    session
        .wait_for("PTY-SIZE=100x30", Duration::from_secs(10))
        .unwrap();
    assert!(!session.in_alternate_screen());
    session.resize_terminal(40, 140);
    session
        .wait_for("PTY-SIZE=140x40", Duration::from_secs(4))
        .unwrap();
}
