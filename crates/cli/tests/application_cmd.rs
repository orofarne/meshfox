//! Concrete applications use the same worker path as ordinary CLI runs.
use std::process::Command;

#[test]
fn typed_applications_run_with_local_args_and_resolved_output_paths() {
    let dir = std::env::temp_dir().join(format!("meshfox-app-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let canvas = dir.join("app.canvas.md");
    let source = format!("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:var name=\"WORK\" default=\"{}\" -->\n<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" -->\n<!-- meshfox:arg name=\"n\" type=\"int\" default=\"9\" required -->\n```bash name=\"extract\" env=\"WORK\" outputs=\"$WORK/out_${{lang}}_${{n}}.txt\"\nprintf '%s:%s' \"$lang\" \"$n\" > \"$WORK/out_${{lang}}_${{n}}.txt\"\nprintf 'args=%s:%s\\n' \"$lang\" \"$n\"\n```\n", dir.display());
    std::fs::write(&canvas, &source).unwrap();
    let run = |application: &str| {
        Command::new(env!("CARGO_BIN_EXE_meshfox"))
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "")
            .arg("run")
            .arg("--canvas")
            .arg(&canvas)
            .arg(application)
            .output()
            .unwrap()
    };
    let result = run("extract[n=01,lang=hy]");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("args=hy:1"));
    assert_eq!(
        std::fs::read_to_string(dir.join("out_hy_1.txt")).unwrap(),
        "hy:1"
    );
    let result = run("extract[lang=en,n=2]");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("out_en_2.txt")).unwrap(),
        "en:2"
    );
    for bad in [
        "extract[lang=hy]",
        "extract[lang=fr,n=1]",
        "extract[lang=hy,n=no]",
    ] {
        assert!(!run(bad).status.success(), "accepted {bad}");
    }
    assert_eq!(std::fs::read_to_string(&canvas).unwrap(), source);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn manual_launch_defaults_and_default_block_shortcuts_use_argument_preparation() {
    let dir = std::env::temp_dir().join(format!("meshfox-arg-preflight-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let canvas = dir.join("app.canvas.md");
    std::fs::write(
        &canvas,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Child\n<!-- meshfox:node id=\"child\" -->\n",
            "<!-- meshfox:arg name=\"n\" type=\"int\" default=\"02\" -->\n",
            "```bash name=\"work\" default\nprintf 'default=%s\\n' \"$n\"\n```\n",
            "<!-- meshfox:arg name=\"n\" type=\"int\" default=\"9\" required -->\n",
            "```bash name=\"confirm\"\nprintf 'confirmed=%s\\n' \"$n\"\n```\n",
        ),
    )
    .unwrap();
    let run = |target: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_meshfox"))
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "")
            .args(["run", "--canvas"])
            .arg(&canvas)
            .args(target)
            .output()
            .unwrap()
    };
    let result = run(&["child"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("default=2"));
    let result = run(&["child", "confirm"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing required argument(s): n"));
    let result = run(&["child", "confirm[n=09]"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("confirmed=9"));
    let result = run(&["child", "confirm"]);
    assert!(
        !result.status.success(),
        "arguments must not persist between runs"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn env_templates_select_declared_values_after_argument_binding() {
    let dir = std::env::temp_dir().join(format!("meshfox-env-template-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let canvas = dir.join("app.canvas.md");
    std::fs::write(
        &canvas,
        r#"<!-- meshfox:canvas -->
# Root
<!-- meshfox:node id="root" -->
<!-- meshfox:var name="URL_en" default="en-url" -->
<!-- meshfox:var name="URL_hy" default="hy-url$literal" -->
<!-- meshfox:arg name="lang" type="string" -->
```bash name="fetch" env="URL=URL_${lang}"
printf '%s\n' "$URL"
```
"#,
    )
    .unwrap();
    let run = |app: &str| {
        Command::new(env!("CARGO_BIN_EXE_meshfox"))
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "")
            .args(["run", "--canvas"])
            .arg(&canvas)
            .arg(app)
            .output()
            .unwrap()
    };
    let result = run("fetch[lang=hy]");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("hy-url$literal"));
    let result = run("fetch[lang=fr]");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("URL_fr"));
    let _ = std::fs::remove_dir_all(dir);
}

