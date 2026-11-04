use std::time::Duration;

use crate::{fixtures, harness::TuiSession};

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
