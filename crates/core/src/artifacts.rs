//! File-value dependencies. Producer discovery never implies execution cascade.
use crate::{BlockAddr, Canvas, CodeBlock};
use globset::{GlobBuilder, GlobMatcher};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

fn error(message: impl Into<String>) -> crate::DepsError {
    crate::DepsError::Artifacts(message.into())
}

pub fn declarations(block: &CodeBlock, attr: &str) -> Vec<String> {
    block
        .attrs
        .get(attr)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// $NAME and ${NAME} may occur anywhere in a path; $$ escapes a literal dollar.
fn substitute(
    text: &str,
    values: &HashMap<String, String>,
    strict: bool,
) -> Result<String, crate::DepsError> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'$') {
            chars.next();
            out.push('$');
            continue;
        }
        let braced = chars.peek() == Some(&'{');
        if braced {
            chars.next();
        }
        let mut name = String::new();
        while let Some(&next) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                name.push(next);
                chars.next();
            } else {
                break;
            }
        }
        if braced && chars.next() != Some('}') {
            return Err(error("invalid braced variable in artifact path"));
        }
        if name.is_empty() {
            return Err(error(
                "invalid variable in artifact path; use $$ for a literal dollar",
            ));
        }
        match values.get(&name) {
            Some(value) => out.push_str(value),
            None if strict => return Err(error(format!("unresolved artifact variable {name}"))),
            None => {
                out.push_str("__meshfox_var_");
                out.push_str(&name);
                out.push_str("__");
            }
        }
    }
    Ok(out)
}

pub fn var_refs(block: &CodeBlock) -> Vec<String> {
    let mut names = Vec::new();
    for attr in ["inputs", "outputs"] {
        for path in declarations(block, attr) {
            let mut chars = path.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '$' {
                    continue;
                }
                if chars.peek() == Some(&'$') {
                    chars.next();
                    continue;
                }
                if chars.peek() == Some(&'{') {
                    chars.next();
                }
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir => {
                if result.file_name().is_some() {
                    result.pop();
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn paths(
    canvas: &Canvas,
    addr: &BlockAddr,
    block: &CodeBlock,
    attr: &str,
    values: &HashMap<String, String>,
    strict: bool,
) -> Result<Vec<PathBuf>, crate::DepsError> {
    if block.attrs.get(attr).is_some_and(|v| v.trim().is_empty()) {
        return Err(error(format!(
            "{}/{}: empty {attr}",
            addr.node_id, addr.block_name
        )));
    }
    let node = canvas
        .node(&addr.node_id)
        .ok_or_else(|| error("artifact node disappeared"))?;
    let root = if canvas.artifact_root.is_absolute() {
        canvas.artifact_root.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| error(e.to_string()))?
            .join(&canvas.artifact_root)
    };
    let cwd = node.cwd(&root);
    declarations(block, attr)
        .into_iter()
        .map(|raw| {
            let expanded = substitute(&raw, values, strict)?;
            let path = normalize(&cwd.join(expanded));
            if attr == "outputs" && has_glob(&path) {
                return Err(error(format!(
                    "output must be an exact file path: {}",
                    path.display()
                )));
            }
            Ok(path)
        })
        .collect()
}

fn has_glob(path: &Path) -> bool {
    path.to_string_lossy().contains(['*', '?', '['])
}
fn matcher(path: &Path) -> Result<GlobMatcher, crate::DepsError> {
    GlobBuilder::new(&path.to_string_lossy())
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| error(e.to_string()))
}

fn graph_values(canvas: &Canvas) -> HashMap<String, String> {
    let mut values: HashMap<_, _> = crate::declared_vars(canvas)
        .unwrap_or_default()
        .into_iter()
        .filter(|v| v.from.is_none())
        .filter_map(|v| v.default.map(|value| (v.name, value)))
        .collect();
    values.extend(canvas.artifact_values.clone());
    values
}

fn producers(
    canvas: &Canvas,
    values: &HashMap<String, String>,
) -> Result<BTreeMap<PathBuf, BlockAddr>, crate::DepsError> {
    let mut result = BTreeMap::new();
    for node in &canvas.nodes {
        // Included Markdown is not a producer namespace of this canvas.
        if node.plain_markdown_include {
            continue;
        }
        for block in crate::scan_runnable_blocks(&node.id, &node.text) {
            let addr = BlockAddr::new(&node.id, block.name.as_deref().unwrap());
            for path in paths(canvas, &addr, &block, "outputs", values, false)? {
                if let Some(other) = result.insert(path.clone(), addr.clone()) {
                    return Err(error(format!(
                        "ambiguous producer for {}: {}/{} and {}/{}",
                        path.display(),
                        other.node_id,
                        other.block_name,
                        addr.node_id,
                        addr.block_name
                    )));
                }
            }
        }
    }
    Ok(result)
}

pub fn producer_deps(
    canvas: &Canvas,
    addr: &BlockAddr,
    block: &CodeBlock,
) -> Result<Vec<BlockAddr>, crate::DepsError> {
    let values = graph_values(canvas);
    let producers = producers(canvas, &values)?;
    let mut deps = Vec::new();
    // Observe computed output paths before deciding which declarations match.
    // Even an unrelated variable name may resolve to this input's path.
    if block.attrs.contains_key("inputs") {
        let decls = crate::declared_vars(canvas)?;
        for node in &canvas.nodes {
            if node.plain_markdown_include {
                continue;
            }
            for mut producer in crate::scan_runnable_blocks(&node.id, &node.text) {
                producer.attrs.remove("inputs");
                let names = var_refs(&producer);
                for name in
                    crate::vars::close_over_var_refs(&decls, names.iter().map(String::as_str))
                {
                    if let Some(source) = decls
                        .iter()
                        .find(|d| d.name == name)
                        .and_then(|d| d.from.as_ref())
                    {
                        let source = crate::deps::resolve_ref(&node.id, source);
                        if !deps.contains(&source) {
                            deps.push(source);
                        }
                    }
                }
            }
        }
    }
    for input in paths(canvas, addr, block, "inputs", &values, false)? {
        let pattern = matcher(&input)?;
        for (output, producer) in &producers {
            if pattern.is_match(output) && !deps.contains(producer) {
                deps.push(producer.clone());
            }
        }
    }
    Ok(deps)
}

fn files(pattern: &Path) -> Result<Vec<PathBuf>, crate::DepsError> {
    if !has_glob(pattern) {
        return Ok(if pattern.is_file() {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        });
    }
    let matcher = matcher(pattern)?;
    let mut root = PathBuf::new();
    for part in pattern.components() {
        if part.as_os_str().to_string_lossy().contains(['*', '?', '[']) {
            break;
        }
        root.push(part.as_os_str());
    }
    let mut found = Vec::new();
    fn walk(
        dir: &Path,
        matcher: &GlobMatcher,
        found: &mut Vec<PathBuf>,
    ) -> Result<(), crate::DepsError> {
        for entry in std::fs::read_dir(dir).map_err(|e| error(format!("{}: {e}", dir.display())))? {
            let entry = entry.map_err(|e| error(e.to_string()))?;
            let kind = entry.file_type().map_err(|e| error(e.to_string()))?;
            if kind.is_dir() {
                walk(&entry.path(), matcher, found)?;
            } else if entry.path().is_file() && matcher.is_match(entry.path()) {
                found.push(entry.path());
            }
        }
        Ok(())
    }
    if root.is_dir() {
        walk(&root, &matcher, &mut found)?;
    }
    found.sort();
    found.dedup();
    Ok(found)
}

fn digest(path: &Path) -> Result<String, crate::DepsError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| error(format!("{}: {e}", path.display())))?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| error(e.to_string()))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().to_hex().to_string())
}

/// Missing files have a stable sentinel while planning. Execution validates
/// inputs before spawning and outputs after success; no sentinel is certified.
pub fn fingerprint(
    canvas: &Canvas,
    addr: &BlockAddr,
    block: &CodeBlock,
    values: &HashMap<String, String>,
    attr: &str,
    strict: bool,
) -> Result<String, crate::DepsError> {
    let mut hash = blake3::Hasher::new();
    for path in paths(canvas, addr, block, attr, values, strict)? {
        let matches = files(&path)?;
        hash.update(path.to_string_lossy().as_bytes());
        hash.update(&[0]);
        if matches.is_empty() {
            if strict {
                return Err(error(format!(
                    "{attr}: missing file or empty glob {}",
                    path.display()
                )));
            }
            hash.update(b"missing");
        }
        for file in matches {
            hash.update(file.to_string_lossy().as_bytes());
            hash.update(&[0]);
            hash.update(digest(&file)?.as_bytes());
            hash.update(&[0]);
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub fn validate(canvas: &Canvas) -> Result<(), crate::DepsError> {
    producers(canvas, &graph_values(canvas))?;
    for node in &canvas.nodes {
        for block in crate::scan_runnable_blocks(&node.id, &node.text) {
            let addr = BlockAddr::new(&node.id, block.name.as_deref().unwrap());
            for attr in ["inputs", "outputs"] {
                if block.attrs.contains_key(attr) && declarations(&block, attr).is_empty() {
                    return Err(error(format!("{attr} requires at least one file path")));
                }
            }
            if ["inputs", "outputs"]
                .iter()
                .any(|a| block.attrs.contains_key(*a))
                && (block.service
                    || crate::exec::is_form(&block.lang)
                    || crate::exec::is_button(&block.lang))
            {
                return Err(error("inputs/outputs require a finite executable block"));
            }
            for input in paths(
                canvas,
                &addr,
                &block,
                "inputs",
                &graph_values(canvas),
                false,
            )? {
                matcher(&input)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn canvas(body: &str) -> Canvas {
        crate::mdcanvas::parse(&format!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n{body}"
        ))
        .unwrap()
    }
    #[test]
    fn discovers_missing_outputs_and_rejects_ambiguous_producers_and_cycles() {
        let c = canvas("```sh name=build outputs=out.bin\ntrue\n```\n```sh name=install inputs=./out.bin\ntrue\n```");
        assert_eq!(
            crate::deps::resolve_chain(&c, BlockAddr::new("root", "install")).unwrap(),
            vec![
                BlockAddr::new("root", "build"),
                BlockAddr::new("root", "install")
            ]
        );
        let ambiguous = canvas(
            "```sh name=a outputs=out.bin\ntrue\n```\n```sh name=b outputs=./out.bin\ntrue\n```",
        );
        assert!(validate(&ambiguous)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
        let cycle = canvas("```sh name=a inputs=b outputs=a\ntrue\n```\n```sh name=b inputs=a outputs=b\ntrue\n```");
        assert!(matches!(
            crate::deps::resolve_chain(&cycle, BlockAddr::new("root", "a")),
            Err(crate::DepsError::Cycle(_))
        ));
    }
    #[test]
    fn glob_inputs_find_all_declared_producers_before_files_exist() {
        let c = canvas("```sh name=a outputs=data/a.csv\ntrue\n```\n```sh name=b outputs=data/b.csv\ntrue\n```\n```sh name=load inputs=data/*.csv\ntrue\n```");
        assert_eq!(
            crate::deps::resolve_chain(&c, BlockAddr::new("root", "load"))
                .unwrap()
                .len(),
            3
        );
        assert!(validate(&canvas("```sh outputs=data/*.csv\ntrue\n```")).is_err());
    }
    #[test]
    fn variables_are_resolved_in_paths_and_create_from_edges() {
        let mut c = canvas("<!-- meshfox:var name=DIR from=directory -->\n```sh name=directory\ntrue\n```\n```sh name=build outputs=$DIR/out\ntrue\n```\n```sh name=install inputs=${DIR}/out\ntrue\n```");
        let chain = crate::deps::resolve_chain(&c, BlockAddr::new("root", "install")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "directory"),
                BlockAddr::new("root", "build"),
                BlockAddr::new("root", "install")
            ]
        );
        c.artifact_values.insert("DIR".into(), "/tmp/value".into());
        assert_eq!(
            crate::deps::resolve_chain(&c, BlockAddr::new("root", "install")).unwrap(),
            chain
        );
        let block = crate::scan_runnable_blocks(
            "root",
            "```sh inputs=$A${B}/file outputs=$$literal\ntrue\n```",
        )
        .remove(0);
        assert_eq!(var_refs(&block), vec!["A", "B"]);
    }
    #[test]
    fn content_and_glob_membership_change_fingerprints() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-artifacts-{}",
            crate::timestamp::now_utc_rfc3339().replace(':', "")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = canvas("```sh name=load inputs=*.csv outputs=result\ntrue\n```");
        c.artifact_root = dir.clone();
        let addr = BlockAddr::new("root", "load");
        let block = crate::scan_runnable_blocks("root", &c.nodes[0].text).remove(0);
        let values = HashMap::new();
        assert!(fingerprint(&c, &addr, &block, &values, "inputs", true).is_err());
        std::fs::write(dir.join("a.csv"), "one").unwrap();
        let a = fingerprint(&c, &addr, &block, &values, "inputs", true).unwrap();
        std::fs::write(dir.join("a.csv"), "two").unwrap();
        let b = fingerprint(&c, &addr, &block, &values, "inputs", true).unwrap();
        assert_ne!(a, b);
        std::fs::write(dir.join("b.csv"), "two").unwrap();
        assert_ne!(
            b,
            fingerprint(&c, &addr, &block, &values, "inputs", true).unwrap()
        );
        assert!(fingerprint(&c, &addr, &block, &values, "outputs", true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn producer_code_does_not_change_consumer_value_fingerprint() {
        let a = canvas("```sh name=observe always\necho old\n```\n<!-- meshfox:var name=REV from=observe -->\n```sh name=consume env=REV\ntrue\n```");
        let b = canvas("```sh name=observe always\necho new\n```\n<!-- meshfox:var name=REV from=observe -->\n```sh name=consume env=REV\ntrue\n```");
        let values = HashMap::from([("REV".into(), "same".into())]);
        let addr = BlockAddr::new("root", "consume");
        assert_eq!(
            crate::deps::closure_fingerprint(&a, &addr, &values).unwrap(),
            crate::deps::closure_fingerprint(&b, &addr, &values).unwrap()
        );
    }
}
