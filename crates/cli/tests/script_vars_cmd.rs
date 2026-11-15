//! End-to-end: `starlark constraint` and `starlark render` fences read
//! declared variables as `vars`, through `meshfox check` and `meshfox render`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-script-vars-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox(sub: &str, canvas: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.env("HOME", unique_dir());
    cmd.env("MFX_SV_FROM_ENV", "from the environment");
    cmd.arg(sub).arg("--canvas").arg(canvas);
    cmd
}

const DOC: &str = concat!(
    "<!-- meshfox:canvas -->\n# Vars\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"MFX_SV_ENV\" default=\"unused\" -->\n",
    "<!-- meshfox:var name=\"MFX_SV_FROM_ENV\" default=\"unused\" -->\n",
    "<!-- meshfox:var name=\"MFX_SV_COUNT\" type=\"int\" default=\"41\" -->\n",
    "<!-- meshfox:var name=\"MFX_SV_TOKEN\" secret default=\"hunter2\" -->\n",
    "<!-- meshfox:var name=\"MFX_SV_Q\" content=\"queries/q\" -->\n\n",
    "```starlark constraint\n",
    "if vars[\"MFX_SV_FROM_ENV\"] != \"from the environment\":\n    fail(\"env value not seen: \" + vars[\"MFX_SV_FROM_ENV\"])\n",
    "if vars[\"MFX_SV_COUNT\"] + 1 != 42:\n    fail(\"int is not an int\")\n",
    "if \"MFX_SV_TOKEN\" in vars:\n    fail(\"a secret leaked into vars\")\n",
    "```\n\n",
    "```starlark render name=\"show\"\n",
    "print(\"count=\" + str(vars[\"MFX_SV_COUNT\"] + 1), \"query=\" + vars[\"MFX_SV_Q\"])\n",
    "```\n\n",
    "## Queries\n<!-- meshfox:node id=\"queries\" -->\n\n",
    "```sql name=\"q\"\nSELECT 1\n```\n",
);

#[test]
fn check_runs_constraints_and_renders_with_vars() {
    let dir = unique_dir();
    let path = dir.join("c.canvas.md");
    std::fs::write(&path, DOC).unwrap();
    let out = meshfox("check", &path).output().unwrap();
    assert!(
        out.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_prints_values_computed_from_vars() {
    let dir = unique_dir();
    let path = dir.join("c.canvas.md");
    std::fs::write(&path, DOC).unwrap();
    let out = meshfox("render", &path).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("count=42 query=SELECT 1"),
        "stdout: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_fails_when_a_constraint_sees_an_unexpected_var_value() {
    let dir = unique_dir();
    let path = dir.join("c.canvas.md");
    std::fs::write(&path, DOC).unwrap();
    let out = meshfox("check", &path)
        .env("MFX_SV_FROM_ENV", "something else")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("env value not seen: something else"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}
