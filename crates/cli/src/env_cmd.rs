//! `meshfox env list|rm` — the Python and Node environments meshfox builds
//! for a canvas (see `meshfox_core::env_state`). Runs in this process like
//! `meshfox prune`: it only looks at, or deletes, directories under
//! `.meshfox/`, and refuses a canvas that has a running worker.

use std::path::{Path, PathBuf};

use meshfox_core::env_state::{self, Environment, Kind};

#[derive(clap::Subcommand, Debug)]
pub enum EnvOp {
    /// List the environments (`.meshfox/<canvas>.venv` for `@python_venv`,
    /// `.meshfox/<canvas>.node` for `@node_env`, and the named ones inside
    /// them) in a directory, or of one canvas file. Changes nothing.
    List {
        /// A directory (default: the current one) or one canvas file.
        #[arg(value_name = "CANVAS|DIR", default_value = ".")]
        path: PathBuf,
        /// Also look in every subdirectory (skipping `.git`,
        /// `node_modules`, `target`).
        #[arg(long, short)]
        recursive: bool,
    },
    /// Delete a canvas's environments, so the next run of its setup block
    /// (`meshfox run --fresh <canvas> <block>`) builds them again — e.g.
    /// after the system Python was upgraded. Without `--name`, removes the
    /// canvas's usual environment together with every named one. Refuses
    /// while a worker is running for the canvas. Run history
    /// (`.session.sqlite3`) and variable caches (`.env`) are not touched.
    Rm {
        canvas: PathBuf,
        /// Only the environment selected by this `name=`.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Only Python environments (`@python_venv`).
        #[arg(long, conflicts_with = "node")]
        python: bool,
        /// Only Node environments (`@node_env`).
        #[arg(long)]
        node: bool,
    },
}

pub fn run(op: EnvOp) -> Result<(), String> {
    match op {
        EnvOp::List { path, recursive } => list(&path, recursive),
        EnvOp::Rm {
            canvas,
            name,
            python,
            node,
        } => rm(&canvas, name.as_deref(), python, node),
    }
}

fn list(path: &Path, recursive: bool) -> Result<(), String> {
    let found = if path.is_dir() {
        env_state::list(path, recursive)
    } else if path.is_file() {
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let name = path.file_name();
        env_state::list(parent, false)
            .into_iter()
            .filter(|e| e.canvas.file_name() == name)
            .collect()
    } else {
        return Err(format!("{} isn't a file or directory", path.display()));
    };
    if found.is_empty() {
        println!("no environments");
        return Ok(());
    }
    let root = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    for env in &found {
        println!("{}", line(env, root));
    }
    Ok(())
}

fn line(env: &Environment, root: &Path) -> String {
    let canvas = env
        .canvas
        .strip_prefix(root)
        .unwrap_or(&env.canvas)
        .display();
    let name = env
        .name
        .as_deref()
        .map(|n| format!(" name={n}"))
        .unwrap_or_default();
    let gone = if env.canvas_exists {
        ""
    } else {
        "  (canvas is gone — `meshfox prune` removes it)"
    };
    format!(
        "{canvas}  {}{name}  {}{gone}",
        env.kind.label(),
        size(env.bytes)
    )
}

fn rm(canvas: &Path, name: Option<&str>, python: bool, node: bool) -> Result<(), String> {
    let kinds: &[Kind] = match (python, node) {
        (true, _) => &[Kind::Python],
        (_, true) => &[Kind::Node],
        _ => &[Kind::Python, Kind::Node],
    };
    let removed = env_state::remove(canvas, name, kinds).map_err(|e| e.to_string())?;
    for r in &removed {
        println!("removed {} ({})", r.path.display(), size(r.bytes));
    }
    println!(
        "run the canvas's setup block again to rebuild: meshfox run --fresh {} <block>",
        canvas.display()
    );
    Ok(())
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
