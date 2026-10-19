//! Native canvas calls go through the same worker path as every frontend.
use std::{
    path::{Path, PathBuf},
    process::Command,
};
fn fixture(child: &str, parent_options: &str, parent_body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mfx-call-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("child.md"),
        format!("<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n{child}"),
    )
    .unwrap();
    let parent = dir.join("parent.md");
    std::fs::write(&parent, format!("<!-- meshfox:canvas -->\n# Parent\n<!-- meshfox:node id=\"root\" -->\n\n{parent_body}\n```meshfox name=invoke {parent_options}\ncanvas: child.md\nrun: work\n```\n")).unwrap();
    (dir, parent)
}
fn command(dir: &Path, parent: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    let bin = Path::new(env!("CARGO_BIN_EXE_meshfox")).parent().unwrap();
    cmd.env("HOME", dir)
        .env_remove("MESHFOX_SERVER_SOCKET")
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .arg(parent)
        .args(["run", "invoke", "--terminal-status", "off"]);
    cmd
}
#[test]
fn native_call_streams_child_and_propagates_failure() {
    for code in [0, 7] {
        let (dir, parent) = fixture(
            &format!("```bash name=work\necho child-output; exit {code}\n```"),
            "",
            "",
        );
        let out = command(&dir, &parent).output().unwrap();
        assert_eq!(
            out.status.success(),
            code == 0,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("child-output"),
            "{:?}",
            out
        );
    }
}
#[test]
fn native_missing_inputs_are_found_before_parent_dependencies_run() {
    let (dir, parent) = fixture("<!-- meshfox:var name=\"TOKEN\" type=\"string\" -->\n```bash name=work env=\"$TOKEN\"\nprintf '%s' \"$TOKEN\"\n```", "deps=before", "```bash name=before\ntouch side-effect\n```\n");
    let out = command(&dir, &parent).output().unwrap();
    assert!(!out.status.success());
    assert!(!dir.join("side-effect").exists());
    let out = command(&dir, &parent)
        .args(["--set", "@canvas[root:invoke]/TOKEN=provided"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("provided"));
}
#[test]
fn native_child_confirmation_is_not_implicitly_approved() {
    let (dir, parent) = fixture("```bash name=work confirm\ntouch approved\n```", "", "");
    let out = command(&dir, &parent).output().unwrap();
    assert!(!out.status.success());
    assert!(!dir.join("approved").exists());
    let out = command(&dir, &parent).arg("--confirm").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("approved").exists());
}
#[test]
fn native_cycle_fails_before_any_side_effects() {
    let (dir, parent) = fixture(
        "```meshfox name=work\ncanvas: parent.md\nrun: invoke\n```",
        "",
        "",
    );
    let out = command(&dir, &parent).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cycle"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn native_yaml_options_and_bound_arguments_are_applied() {
    let (dir, parent) = fixture("<!-- meshfox:arg name=\"MSG\" type=\"string\" -->\n```bash name=work env=\"$TOKEN\"\nprintf '%s:%s' \"$MSG\" \"$TOKEN\"\n```\n<!-- meshfox:var name=\"TOKEN\" type=\"string\" -->", "", "");
    let text = std::fs::read_to_string(&parent).unwrap().replace(
        "run: work",
        "run: 'work[MSG=hello]'\nset:\n  TOKEN: fixed\nfresh: true",
    );
    std::fs::write(&parent, text).unwrap();
    let out = command(&dir, &parent).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello:fixed"),
        "{:?}",
        out
    );
}

#[test]
fn native_included_block_resolves_canvas_relative_to_its_document() {
    let (dir, parent) = fixture("```bash name=work\necho correct-child\n```", "", "");
    let include = dir.join("included");
    std::fs::create_dir_all(&include).unwrap();
    std::fs::write(include.join("calls.md"), "<!-- meshfox:canvas -->\n# Calls\n<!-- meshfox:node id=\"calls\" -->\n```meshfox name=invoke\ncanvas: ../child.md\nrun: work\n```\n").unwrap();
    std::fs::write(&parent, "<!-- meshfox:canvas -->\n# Parent\n<!-- meshfox:node id=\"root\" -->\n## Included\n<!-- meshfox:node id=\"included\" type=\"include\" -->\n[calls](included/calls.md)\n").unwrap();
    // The include node owns its source's root body.
    let binary = env!("CARGO_BIN_EXE_meshfox");
    let out = Command::new(binary)
        .env("HOME", &dir)
        .env_remove("MESHFOX_SERVER_SOCKET")
        .arg(&parent)
        .args(["run", "included", "invoke"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("correct-child"));
}

#[test]
fn cancelling_parent_cancels_child_without_leaving_it_running() {
    let (dir, parent) = fixture(
        "```bash name=work\ntouch started; sleep 3; touch escaped\n```",
        "",
        "",
    );
    let mut child = command(&dir, &parent)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !dir.join("started").exists() && std::time::Instant::now() < deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "parent exited before child started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(dir.join("started").exists());
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    assert_eq!(child.wait().unwrap().code(), Some(130));
    std::thread::sleep(std::time::Duration::from_secs(4));
    assert!(
        !dir.join("escaped").exists(),
        "cancelled child survived its parent"
    );
}

#[test]
fn a_leased_service_in_an_embedded_child_is_cleaned_up_before_the_host_exits() {
    let (dir, parent) = fixture(
        "```yaml name=database interpreter=@service\nstart: sleep 60\nready: 'true'\nstop: touch stopped\nstop_timeout: 100ms\n```\n```bash name=work lease=database\necho using-database\n```", "", "");
    let out = command(&dir, &parent).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("using-database"));
    assert!(dir.join("stopped").exists());
}

#[test]
fn a_local_lease_returns_without_following_the_released_service() {
    for borrowed in [false, true] {
        let (dir, parent) = fixture("", "", "");
        if borrowed {
            std::fs::write(dir.join("external"), "running").unwrap();
        }
        std::fs::write(&parent, concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```yaml name=db interpreter=@service\noneshot: true\nreuse: test -f external\n",
            "start: touch external\nready: test -f external\nstop: rm external\nstop_timeout: 100ms\n```\n",
            "```bash name=invoke lease=db\necho finished\n```\n"
        )).unwrap();
        let out = command(&dir, &parent).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("finished"));
        assert!(!stdout.contains("service(s) running"), "{stdout}");
        assert_eq!(dir.join("external").exists(), borrowed);
    }
}
