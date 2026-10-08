//! Poison inherited context in a separate process, keeping parallel tests and
//! the developer's environment/config untouched.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

#[test]
fn inherited_run_context_is_isolated_across_executors() {
    use std::os::unix::fs::PermissionsExt;
    let dir =
        std::env::temp_dir().join(format!("meshfox-run-environment-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(dir.join(".meshfox/config.toml"),
        "[interpreters.agent]\nprovider = 'codex'\n[process_env]\nMESHFOX_VENV_DIR = 'config-poison'\n").unwrap();
    let fake = dir.join("codex");
    std::fs::write(
        &fake,
        "#!/bin/sh\nfor arg do :; done\nprintf '%s\\n' \"$arg\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "run_environment_tests::poisoned_context_child",
            "--nocapture",
        ])
        .env("HOME", &dir)
        .env("MESHFOX_TEST_RUN_ENV_DIR", &dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("MESHFOX_ENV_NAMES", "SECRET")
        .env("MESHFOX_VARS_OUT", "parent-output")
        .env("MESHFOX_BLOCK_LANG", "parent-lang")
        .env("MESHFOX_VENV_DIR", "parent-venv")
        .env("MESHFOX_CONFIG_OLD_KEY", "parent-config")
        .env("MESHFOX_KEEP_CONTROL", "preserved")
        .env("SECRET", "ambient-secret")
        .output()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn captured(mut proc: crate::stream_exec::SpawnedProcess) -> String {
    let mut lines = Vec::new();
    while let Some((_, line)) = proc.output_rx.recv().await {
        lines.push(line);
    }
    assert!(proc.child.wait().await.unwrap().success(), "{lines:?}");
    lines.join("\n")
}

#[tokio::test]
async fn poisoned_context_child() {
    let Some(dir) = std::env::var_os("MESHFOX_TEST_RUN_ENV_DIR") else {
        return;
    };
    let cwd = Path::new(&dir);
    let check = r#"
test -z "${MESHFOX_VARS_OUT+x}"
test -z "${MESHFOX_BLOCK_LANG+x}"
test -z "${MESHFOX_VENV_DIR+x}"
test -z "${MESHFOX_CONFIG_OLD_KEY+x}"
test "$MESHFOX_KEEP_CONTROL" = preserved
test "$SECRET" = ambient-secret
test "$MESHFOX_ENV_NAMES" = TOPIC
test "$TOPIC" = declared
echo OK
"#;
    let locals = [("TOPIC", "declared")];
    assert_eq!(
        captured(crate::stream_exec::spawn_bash(check, locals, Some(cwd)).unwrap()).await,
        "OK"
    );
    assert_eq!(
        captured(
            crate::stream_exec::spawn_interpreter("bash -e", check, None, locals, Some(cwd), None)
                .unwrap()
        )
        .await,
        "OK"
    );
    let no_locals_check = check
        .replace(
            "test \"$MESHFOX_ENV_NAMES\" = TOPIC",
            "test \"$MESHFOX_ENV_NAMES\" = ''",
        )
        .replace("test \"$TOPIC\" = declared", "");
    assert_eq!(
        captured(
            crate::stream_exec::spawn_process("bash", ["-e", "-c", &no_locals_check], Some(cwd))
                .unwrap()
        )
        .await,
        "OK"
    );

    for interpreter in [None, Some("bash -e")] {
        let mut pty = crate::pty_exec::spawn(
            &format!("set -e\n{check}"),
            interpreter,
            None,
            locals,
            Some(cwd),
            None,
            (80, 24),
        )
        .unwrap();
        let mut bytes = Vec::new();
        while let Some(chunk) = pty.output_rx.recv().await {
            bytes.extend(chunk);
        }
        assert_eq!(pty.wait().await, 0, "{}", String::from_utf8_lossy(&bytes));
        assert!(String::from_utf8_lossy(&bytes).contains("OK"));
    }
    let mut debug = crate::debug_session::DebugSession::spawn(
        cwd,
        HashMap::from([("TOPIC".into(), "declared".into())]),
    )
    .unwrap();
    let result = debug
        .send(&format!("set -e\n{check}"), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("OK"));
    debug.stop().await;

    let vars_out = crate::stream_exec::spawn_bash(
        "test \"$MESHFOX_VARS_OUT\" = fresh; test \"$MESHFOX_ENV_NAMES\" = TOPIC; echo OK",
        [("TOPIC", "declared"), (meshfox_core::VARS_OUT_ENV, "fresh")],
        Some(cwd),
    )
    .unwrap();
    assert_eq!(captured(vars_out).await, "OK");
    let prompt = "$SECRET / $TOPIC";
    assert_eq!(
        captured(
            crate::stream_exec::spawn_interpreter("@agent", prompt, None, locals, Some(cwd), None)
                .unwrap()
        )
        .await,
        "$SECRET / declared"
    );
    assert_eq!(
        captured(
            crate::stream_exec::spawn_interpreter(
                "@agent",
                "$SECRET",
                None,
                [] as [(&str, &str); 0],
                Some(cwd),
                None
            )
            .unwrap()
        )
        .await,
        "$SECRET"
    );
    let mut pty = crate::pty_exec::spawn(
        prompt,
        Some("@agent"),
        None,
        locals,
        Some(cwd),
        None,
        (80, 24),
    )
    .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = pty.output_rx.recv().await {
        bytes.extend(chunk);
    }
    assert_eq!(pty.wait().await, 0);
    assert!(String::from_utf8_lossy(&bytes).contains("$SECRET / declared"));

    // Cache identities use the actual block cwd, including included content,
    // and propagate provider changes through explicit dependency closures.
    let raw = "# Root\n<!-- meshfox:node id=\"root\" -->\n```text name=\"ask\" interpreter=\"@agent\"\nHello\n```\n```bash name=\"after\" deps=\"ask\"\necho after\n```\n";
    let mut canvas = meshfox_core::mdcanvas::parse(raw).unwrap();
    canvas.artifact_root = cwd.to_path_buf();
    let target = meshfox_core::BlockAddr::new("root", "after");
    let values = HashMap::new();
    let before = meshfox_core::closure_fingerprint(&canvas, &target, &values).unwrap();
    std::fs::write(
        cwd.join(".meshfox/config.toml"),
        "[interpreters.agent]\nprovider = 'claude'\n",
    )
    .unwrap();
    let changed = meshfox_core::closure_fingerprint(&canvas, &target, &values).unwrap();
    assert_ne!(before, changed);
    std::fs::write(
        cwd.join(".meshfox/config.toml"),
        "[interpreters.agent]\nprovider = 'claude'\n[tables]\ncache_max_bytes = 321\n",
    )
    .unwrap();
    assert_eq!(
        changed,
        meshfox_core::closure_fingerprint(&canvas, &target, &values).unwrap()
    );
    let include_dir = cwd.join("included");
    std::fs::create_dir_all(include_dir.join(".meshfox")).unwrap();
    std::fs::write(
        include_dir.join(".meshfox/config.toml"),
        "[interpreters.agent]\nprovider = 'codex'\n",
    )
    .unwrap();
    canvas.node_mut("root").unwrap().asset_base = Some(include_dir.display().to_string());
    assert_eq!(
        before,
        meshfox_core::closure_fingerprint(&canvas, &target, &values).unwrap()
    );
}
