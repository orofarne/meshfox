//! `meshfox mv`: moves a canvas file or a directory of canvases together with
//! the state meshfox keeps for them (see `meshfox_core::relocate` for what
//! that is and what is deliberately left alone). Runs in this process like
//! `meshfox prune`: the canvases must have no worker, so there is none to ask.

use std::path::{Path, PathBuf};

use meshfox_core::relocate::{self, Kind, Mention, Plan};
use meshfox_core::secret_index::SecretIndex;
use meshfox_core::secret_store;

pub fn run(src: PathBuf, dst: PathBuf, dry_run: bool) -> Result<(), String> {
    let index = SecretIndex::default_location();
    let plan = relocate::plan(&src, &dst, index.as_ref()).map_err(|e| e.to_string())?;
    describe(&plan);

    if dry_run {
        if !plan.busy.is_empty() {
            return Err(busy_message(&plan));
        }
        println!("\ndry run — nothing was moved");
        return Ok(());
    }

    // The keychain is only touched when there is something to move in it.
    let backend = (!plan.secrets.is_empty()).then(secret_store::system_backend);
    let warnings =
        relocate::apply(&plan, index.as_ref(), backend.as_deref()).map_err(|e| e.to_string())?;
    println!("\nmoved");
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn busy_message(plan: &Plan) -> String {
    let list: Vec<String> = plan.busy.iter().map(|p| p.display().to_string()).collect();
    format!(
        "a worker is running for {} — stop it first (`meshfox cores kill`, or close the canvas)",
        list.join(", ")
    )
}

fn describe(plan: &Plan) {
    let what = match plan.kind {
        Kind::File => "canvas",
        Kind::Directory => "directory",
    };
    println!(
        "move {what} {} -> {}",
        plan.src.display(),
        plan.dst.display()
    );
    for (from, to) in &plan.state_moves {
        println!("  state   {} -> {}", name_of(from), name_of(to));
    }
    for dir in &plan.dropped {
        println!(
            "  delete  {} (rebuilt by the next run of its setup block)",
            shown(dir, &plan.src)
        );
    }
    if !plan.secrets.is_empty() {
        println!(
            "  secrets {} saved in the system store follow to the new path",
            plan.secrets.len()
        );
    }
    for busy in &plan.busy {
        println!("  BUSY    a worker is running for {}", busy.display());
    }
    if plan.kind == Kind::File && plan.src.parent() != plan.dst.parent() {
        println!(
            "  note    settings inherited from `.meshfox/config.toml` files above the new \
             directory apply from now on"
        );
    }
    mentions(
        "other canvases that seem to refer to it (not changed — fix them by hand)",
        &plan.incoming,
    );
    mentions(
        "relative links of the moved canvas, now resolved from the new directory",
        &plan.outgoing,
    );
}

fn mentions(title: &str, found: &[Mention]) {
    if found.is_empty() {
        return;
    }
    println!("  {title}:");
    for m in found {
        println!("    {}:{}: {}", m.canvas.display(), m.line, m.text);
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path` relative to `root` when it is inside it, otherwise as is.
fn shown(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}
