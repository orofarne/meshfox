//! Running a `[tui.open]` command for a plain file (`o` on a `file` node).
//! Which command comes from `meshfox_core::open_rules`; the worker only
//! vouches for the path, so this is the one place the TUI starts a program
//! for a file node, always in the TUI user's own environment (`PATH` and all).

use std::io;
use std::path::Path;

/// A command that wants the terminal. The event loop (`mod.rs`) hands the
/// screen over, awaits [`run_tty`] and shows its message.
pub struct PendingTty {
    pub argv: Vec<String>,
}

/// Runs the command on the real terminal until it exits. The terminal must
/// already be handed over. Returns the message for the status line.
pub async fn run_tty(argv: Vec<String>) -> String {
    let program = &argv[0];
    match tokio::process::Command::new(program)
        .args(&argv[1..])
        .status()
        .await
    {
        Err(e) => format!("couldn't run {program}: {e}"),
        Ok(status) if status.success() => format!("closed {program}"),
        Ok(status) => format!(
            "{program} exited with {}",
            status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"))
        ),
    }
}

/// Starts the command in the background: no terminal, not waited for, and in
/// its own process group so a Ctrl-C meant for the TUI doesn't reach it.
pub fn spawn_detached(argv: &[String]) -> io::Result<()> {
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    // Reaped, so a long-lived TUI doesn't collect zombies.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The OS default application, on a blocking thread (`open::that` shells out).
pub async fn open_with_system(path: &Path) -> Result<(), String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || open::that(&path).map_err(|e| e.to_string()))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[tokio::test]
    async fn a_tty_command_reports_how_it_ended() {
        assert_eq!(run_tty(argv(&["true"])).await, "closed true");
        assert_eq!(
            run_tty(argv(&["sh", "-c", "exit 3"])).await,
            "sh exited with code 3"
        );
        assert!(run_tty(argv(&["meshfox-no-such-program"]))
            .await
            .starts_with("couldn't run meshfox-no-such-program"));
    }

    #[test]
    fn a_detached_command_starts_without_being_waited_for() {
        let dir = std::env::temp_dir().join(format!("meshfox-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ran");
        spawn_detached(&argv(&[
            "sh",
            "-c",
            "echo hi > \"$0\"",
            marker.to_str().unwrap(),
        ]))
        .unwrap();
        for _ in 0..100 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "hi\n");
        assert!(spawn_detached(&argv(&["meshfox-no-such-program"])).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
