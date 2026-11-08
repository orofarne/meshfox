//! The checks and merges behind `Canvas::resolve_extends`: one small step
//! each, so the walk itself stays a plain loop. Every error is the message a
//! `meshfox validate` run prints for the block whose `extends=` it is.

use crate::canvas::{Extended, Node};
use crate::fence::{BlockRef, CodeBlock};

/// An entry takes a block name only.
pub(crate) fn check_ref(entry: &BlockRef, owner: &str) -> Result<(), String> {
    if entry.sync || entry.condition.is_some() || entry.block_name.contains('[') {
        return Err(format!(
            "extends= entry {:?} of {owner:?} takes only a block name (no `!`, `?` condition or arguments)",
            entry.render()
        ));
    }
    Ok(())
}

/// `key` is already on the way down: following it again would never end.
pub(crate) fn check_cycle(path: &[(String, String)], key: &(String, String)) -> Result<(), String> {
    if !path.contains(key) {
        return Ok(());
    }
    let chain: Vec<String> = path
        .iter()
        .chain(std::iter::once(key))
        .map(|(n, b)| format!("{n}/{b}"))
        .collect();
    Err(format!("extends= cycle: {}", chain.join(" -> ")))
}

/// The runnable block `name` of `node`, if the node has inline code at all.
pub(crate) fn find_parent(node: &Node, name: &str, owner: &str) -> Result<CodeBlock, String> {
    if node.is_runnable_file() {
        return Err(format!(
            "extends= of {owner:?}: {:?} is a file node, which has no inline code to extend",
            node.id
        ));
    }
    crate::fence::scan_runnable_blocks(&node.id, &node.text)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(name))
        .ok_or_else(|| {
            format!(
                "extends= of {owner:?}: no runnable block {name:?} in node {:?}",
                node.id
            )
        })
}

/// A block that is extended is borrowed text: anything that would act on its
/// own (dependencies, files, a terminal, a service, a cache) would silently
/// never apply, and the language has to match for the text to join.
pub(crate) fn check_definitions_only(
    parent: &CodeBlock,
    root: &CodeBlock,
    owner: &str,
    label: &str,
) -> Result<(), String> {
    if parent.lang != root.lang {
        return Err(format!(
            "extends= of {owner:?}: {label} is {:?} but the block is {:?}",
            parent.lang, root.lang
        ));
    }
    if let Some(action) = ["deps", "inputs", "outputs", "lease", "defer"]
        .into_iter()
        .find(|a| parent.attrs.contains_key(*a))
    {
        return Err(format!(
            "extends= of {owner:?}: {label} has `{action}=`; a block that is extended holds definitions only, its dependencies would never run"
        ));
    }
    let flags = [
        ("tty", parent.tty),
        ("service", parent.service),
        ("cache", parent.cache),
    ];
    if let Some((flag, _)) = flags.into_iter().find(|(_, set)| *set) {
        return Err(format!(
            "extends= of {owner:?}: {label} is `{flag}`; a block that is extended holds definitions only"
        ));
    }
    Ok(())
}

/// The block's own interpreter must agree with its parents'; without one it
/// takes the one they agree on.
pub(crate) fn merge_interpreter(
    ext: &mut Extended,
    root: &CodeBlock,
    parent: &CodeBlock,
    owner: &str,
    label: &str,
) -> Result<(), String> {
    let Some(theirs) = &parent.interpreter else {
        return Ok(());
    };
    match (&root.interpreter, &ext.interpreter) {
        (Some(own), _) if own != theirs => Err(format!(
            "extends= of {owner:?}: {label} runs under {theirs:?} but the block under {own:?}"
        )),
        (None, Some(agreed)) if agreed != theirs => Err(format!(
            "extends= of {owner:?}: parents disagree on the interpreter ({agreed:?} / {theirs:?})"
        )),
        (None, _) => {
            ext.interpreter = Some(theirs.clone());
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The parents' `env=` entries, first come first kept; one local name may not
/// mean two variables.
pub(crate) fn merge_env(ext: &mut Extended, parent: &CodeBlock, owner: &str) -> Result<(), String> {
    for e in &parent.env {
        match ext.env.iter().find(|(local, _)| local == &e.local_name) {
            Some((_, var)) if var != &e.source_text() => {
                return Err(format!(
                    "extends= of {owner:?}: parents bind {:?} to different variables ({var} / {})",
                    e.local_name, e.var_name
                ));
            }
            Some(_) => {}
            None => ext.env.push((e.local_name.clone(), e.source_text())),
        }
    }
    Ok(())
}

/// The parent's code, ended by a newline so the next piece starts on a line.
pub(crate) fn append_code(ext: &mut Extended, code: &str) {
    ext.code.push_str(code);
    if !code.ends_with('\n') {
        ext.code.push('\n');
    }
}
