//! Clipboard transports. OSC 52 addresses the user's terminal; native tools
//! address the machine running this process. Neither is silently substituted.
use std::{io::Write, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

pub use meshfox_core::config::TuiClipboard as Method;

pub fn terminal(text: &str) -> std::io::Result<()> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout().lock();
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()
}

pub async fn system(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(); Set-Clipboard -Value ([Console]::In.ReadToEnd())"])];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    copy_with_commands(text, candidates).await
}

async fn copy_with_commands(text: &str, candidates: &[(&str, &[&str])]) -> Result<(), String> {
    for (program, args) in candidates {
        let mut child = match tokio::process::Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{program}: {e}")),
        };
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut stdin = child
                .stdin
                .take()
                .ok_or("clipboard stdin unavailable".to_string())?;
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            drop(stdin);
            let status = child.wait().await.map_err(|e| e.to_string())?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("{program} exited with {status}"))
            }
        })
        .await;
        return result.unwrap_or_else(|_| Err(format!("{program} timed out")));
    }
    Err("no system clipboard tool found (macOS: pbcopy; Linux: wl-copy, xclip or xsel; Windows: powershell.exe)".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_transport_pipes_exact_utf8_without_using_the_real_clipboard() {
        let path =
            std::env::temp_dir().join(format!("meshfox-clipboard-{}.txt", uuid::Uuid::new_v4()));
        let path_text = path.to_str().unwrap();
        let args = ["-c", "cat > \"$1\"", "clipboard-test", path_text];
        let source = "Привет 界🙂\nsecond line\n";
        copy_with_commands(source, &[("/bin/sh", &args)])
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        std::fs::remove_file(path).unwrap();
    }
    #[tokio::test]
    async fn missing_tools_and_nonzero_exits_report_errors() {
        assert!(
            copy_with_commands("text", &[("/definitely-missing-meshfox-clipboard", &[])])
                .await
                .unwrap_err()
                .contains("no system clipboard tool")
        );
        let error = copy_with_commands("text", &[("/bin/sh", &["-c", "cat >/dev/null; exit 7"])])
            .await
            .unwrap_err();
        assert!(error.contains("exited"), "{error}");
    }
}
