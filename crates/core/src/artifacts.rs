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
    names.retain(|name| !block.arguments.contains_key(name));
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
    let mut local_values = values.clone();
    local_values.extend(
        block
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    declarations(block, attr)
        .into_iter()
        .map(|raw| {
            let expanded = substitute(&raw, &local_values, strict)?;
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

#[derive(Clone)]
struct Producer {
    definition: BlockAddr,
    args: Vec<crate::args::ArgDecl>,
    templates: Vec<Vec<TemplatePart>>,
}

#[derive(Clone, Debug)]
enum TemplatePart {
    Literal(String),
    Argument(String),
}

fn producer_templates(
    canvas: &Canvas,
    values: &HashMap<String, String>,
) -> Result<Vec<Producer>, crate::DepsError> {
    let mut producers = Vec::new();
    for node in &canvas.nodes {
        if node.plain_markdown_include {
            continue;
        }
        let signatures = crate::args::scan_signatures(&node.id, &node.text)
            .map_err(|e| crate::DepsError::Arguments(e.to_string()))?;
        for block in crate::scan_runnable_blocks(&node.id, &node.text) {
            if !block.attrs.contains_key("outputs") {
                continue;
            }
            let args = signatures
                .iter()
                .find(|sig| sig.block.span == block.span)
                .map(|sig| sig.args.clone())
                .unwrap_or_default();
            let definition = BlockAddr::new(&node.id, block.name.as_deref().unwrap());
            let root = if canvas.artifact_root.is_absolute() {
                canvas.artifact_root.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| error(e.to_string()))?
                    .join(&canvas.artifact_root)
            };
            let cwd = node.cwd(&root);
            // Pick markers absent from every literal source. Substituted global
            // values and argument captures remain literal, never re-interpolated.
            let mut prefix = "__meshfox_argument_".to_string();
            while cwd.to_string_lossy().contains(&prefix)
                || block.attrs["outputs"].contains(&prefix)
                || values.values().any(|v| v.contains(&prefix))
            {
                prefix.push('_');
            }
            let markers: Vec<_> = args
                .iter()
                .enumerate()
                .map(|(i, arg)| (format!("{prefix}{i}__"), arg.name.clone()))
                .collect();
            let mut scope = values.clone();
            scope.extend(
                markers
                    .iter()
                    .map(|(marker, name)| (name.clone(), marker.clone())),
            );
            let mut templates = Vec::new();
            for raw in declarations(&block, "outputs") {
                let expanded = normalize(&cwd.join(substitute(&raw, &scope, false)?));
                if has_glob(&expanded) {
                    return Err(error(format!(
                        "output must be an exact file path: {}",
                        expanded.display()
                    )));
                }
                let text = expanded.to_string_lossy();
                let mut rest = text.as_ref();
                let mut parts = Vec::new();
                while let Some((at, marker, name)) = markers
                    .iter()
                    .filter_map(|(marker, name)| rest.find(marker).map(|at| (at, marker, name)))
                    .min_by_key(|(at, _, _)| *at)
                {
                    if at > 0 {
                        parts.push(TemplatePart::Literal(rest[..at].to_owned()));
                    }
                    parts.push(TemplatePart::Argument(name.clone()));
                    rest = &rest[at + marker.len()..];
                }
                if !rest.is_empty() {
                    parts.push(TemplatePart::Literal(rest.to_owned()));
                }
                templates.push(parts);
            }
            producers.push(Producer {
                definition,
                args,
                templates,
            });
        }
    }
    Ok(producers)
}

/// Enumerate structural captures, including adjacent and repeated placeholders.
/// No greedy tie-breaking: two valid canonical applications are an ambiguity.
fn capture_template(
    parts: &[TemplatePart],
    text: &str,
    bindings: &mut BTreeMap<String, String>,
    budget: &mut usize,
    accept: &mut impl FnMut(&BTreeMap<String, String>) -> Result<(), crate::DepsError>,
) -> Result<(), crate::DepsError> {
    if parts.len() > 128 {
        return Err(error("output template exceeds 128 literal/capture parts"));
    }
    if *budget == 0 {
        return Err(error("output inference exceeded 20000 matching steps"));
    }
    *budget -= 1;
    match parts.split_first() {
        None if text.is_empty() => accept(bindings),
        None => Ok(()),
        Some((TemplatePart::Literal(literal), rest)) => {
            if let Some(text) = text.strip_prefix(literal) {
                capture_template(rest, text, bindings, budget, accept)?;
            }
            Ok(())
        }
        Some((TemplatePart::Argument(name), rest)) => {
            if let Some(value) = bindings.get(name) {
                if let Some(text) = text.strip_prefix(value) {
                    capture_template(rest, text, bindings, budget, accept)?;
                }
                return Ok(());
            }
            // Boundaries are UTF-8 boundaries, and may include an empty string or
            // path separators; the output path is verified again after binding.
            for end in text
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
            {
                if let Some(TemplatePart::Literal(next)) = rest.first() {
                    if !text[end..].starts_with(next) {
                        continue;
                    }
                }
                bindings.insert(name.clone(), text[..end].to_owned());
                capture_template(rest, &text[end..], bindings, budget, accept)?;
                bindings.remove(name);
            }
            Ok(())
        }
    }
}

fn infer_producer(
    canvas: &Canvas,
    templates: &[Producer],
    path: &Path,
    values: &HashMap<String, String>,
) -> Result<Option<BlockAddr>, crate::DepsError> {
    let mut candidates = BTreeMap::new();
    let mut rejected = None;
    let mut budget = 20000;
    for producer in templates {
        let node = canvas.node(&producer.definition.node_id).unwrap();
        for template in &producer.templates {
            capture_template(
                template,
                &path.to_string_lossy(),
                &mut BTreeMap::new(),
                &mut budget,
                &mut |bindings| {
                    if let Some(arg) = producer
                        .args
                        .iter()
                        .find(|arg| arg.is_required() && !bindings.contains_key(&arg.name))
                    {
                        rejected.get_or_insert_with(|| {
                            format!(
                                "cannot infer required argument {:?} for {}/{} from {}",
                                arg.name,
                                producer.definition.node_id,
                                producer.definition.block_name,
                                path.display()
                            )
                        });
                        return Ok(());
                    }
                    // The same binder owns defaults, required rules, type checks
                    // and canonicalization for explicit and inferred applications.
                    let address =
                        crate::args::canonical_name(&producer.definition.block_name, bindings);
                    let bound =
                        match crate::args::bind_block(&node.id, &node.text, &address, values) {
                            Ok(bound) => bound,
                            Err(message) => {
                                rejected.get_or_insert_with(|| {
                                    format!(
                                        "invalid inferred argument for {}/{} from {}: {message}",
                                        producer.definition.node_id,
                                        producer.definition.block_name,
                                        path.display()
                                    )
                                });
                                return Ok(());
                            }
                        };
                    let addr = BlockAddr::new(&node.id, bound.name.as_deref().unwrap());
                    // Canonical int values, cwd normalization and all other outputs
                    // must describe the actual requested file, not a transformed alias.
                    if paths(canvas, &addr, &bound, "outputs", values, false)?
                        .contains(&path.to_path_buf())
                    {
                        candidates.insert((addr.node_id.clone(), addr.block_name.clone()), addr);
                    }
                    Ok(())
                },
            )?;
        }
    }
    if candidates.len() > 1 {
        return Err(error(format!(
            "ambiguous producer for {}: {}",
            path.display(),
            candidates
                .values()
                .map(|a| format!("{}/{}", a.node_id, a.block_name))
                .collect::<Vec<_>>()
                .join(" and ")
        )));
    }
    if let Some(addr) = candidates.into_values().next() {
        return Ok(Some(addr));
    }
    if let Some(message) = rejected {
        return Err(error(message));
    }
    Ok(None)
}

// Finite default applications still participate in glob inputs before files
// exist. A glob never invents an unbounded set of argument values.
fn producers(
    canvas: &Canvas,
    values: &HashMap<String, String>,
) -> Result<BTreeMap<PathBuf, BlockAddr>, crate::DepsError> {
    let mut result = BTreeMap::new();
    for producer in producer_templates(canvas, values)? {
        if producer.args.iter().any(|arg| arg.is_required()) {
            continue;
        }
        let node = canvas.node(&producer.definition.node_id).unwrap();
        let block = crate::args::bind_block(
            &node.id,
            &node.text,
            &producer.definition.block_name,
            values,
        )
        .map_err(crate::DepsError::Arguments)?;
        let addr = BlockAddr::new(&node.id, block.name.as_deref().unwrap());
        for path in paths(canvas, &addr, &block, "outputs", values, false)? {
            if let Some(other) = result.insert(path.clone(), addr.clone()) {
                if other != addr {
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
    let templates = producer_templates(canvas, &values)?;
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
                let local: std::collections::HashSet<_> =
                    crate::args::scan_signatures(&node.id, &node.text)
                        .map_err(|e| crate::DepsError::Arguments(e.to_string()))?
                        .into_iter()
                        .filter(|sig| sig.block.span == producer.span)
                        .flat_map(|sig| sig.args.into_iter().map(|arg| arg.name))
                        .collect();
                let names: Vec<_> = var_refs(&producer)
                    .into_iter()
                    .filter(|name| !local.contains(name))
                    .collect();
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
    let inputs = paths(canvas, addr, block, "inputs", &values, false)?;
    for (input, raw) in inputs.into_iter().zip(declarations(block, "inputs")) {
        let mut declaration = block.clone();
        declaration.attrs.remove("outputs");
        declaration.attrs.insert("inputs".into(), raw);
        if var_refs(&declaration)
            .iter()
            .any(|name| !values.contains_key(name))
        {
            // Planning sentinels are not argument values. Preserve finite default
            // producers (the old graph behavior), and infer only after observation.
            let pattern = matcher(&input)?;
            for (output, producer) in &producers {
                if pattern.is_match(output) && !deps.contains(producer) {
                    deps.push(producer.clone());
                }
            }
            continue;
        }
        let mut requested = if has_glob(&input) {
            let pattern = matcher(&input)?;
            let mut paths: Vec<_> = producers
                .keys()
                .filter(|output| pattern.is_match(output))
                .cloned()
                .collect();
            paths.extend(files(&input)?);
            paths.sort();
            paths.dedup();
            paths
        } else {
            vec![input]
        };
        requested.sort();
        requested.dedup();
        for output in requested {
            if let Some(producer) = infer_producer(canvas, &templates, &output, &values)? {
                if !deps.contains(&producer) {
                    deps.push(producer);
                }
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
    fn application_paths_substitute_values_after_list_parsing() {
        let c = canvas("<!-- meshfox:arg name=\"file\" -->\n```bash name=\"build\" outputs=\"${file}\"\ntrue\n```\n");
        let block = crate::args::bind_block(
            "root",
            &c.node("root").unwrap().text,
            r#"build[file="name,with$dollar.txt"]"#,
            &Default::default(),
        )
        .unwrap();
        let resolved = paths(
            &c,
            &BlockAddr::new("root", block.name.as_deref().unwrap()),
            &block,
            "outputs",
            &Default::default(),
            true,
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].file_name().unwrap(), "name,with$dollar.txt");
        assert!(var_refs(&block).is_empty());
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

#[cfg(test)]
mod inference_tests {
    use super::*;

    fn canvas(body: &str) -> Canvas {
        Canvas::from_markdown(&format!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n{body}"
        ))
        .unwrap()
    }
    fn inferred(canvas: &Canvas, file: &str) -> Result<Option<BlockAddr>, crate::DepsError> {
        let values = graph_values(canvas);
        let requested = normalize(
            &std::env::current_dir()
                .unwrap()
                .join(&canvas.artifact_root)
                .join(file),
        );
        infer_producer(
            canvas,
            &producer_templates(canvas, &values)?,
            &requested,
            &values,
        )
    }

    #[test]
    fn required_defaults_are_ui_only_but_path_captures_supply_arguments() {
        let c = canvas(
            r#"<!-- meshfox:arg name="lang" type="select" choices="en,hy" default="en" required -->
<!-- meshfox:arg name="pages" type="int" default="1" -->
```sh name="extract" outputs="${lang}.csv"
true
```
```sh name="merge" inputs="hy.csv"
true
```"#,
        );
        crate::deps::validate(&c).unwrap();
        let chain = crate::deps::resolve_chain(&c, BlockAddr::new("root", "merge")).unwrap();
        assert_eq!(
            chain,
            [
                BlockAddr::new("root", "extract[lang=hy,pages=1]"),
                BlockAddr::new("root", "merge")
            ]
        );
        assert_eq!(
            crate::deps::resolve_chain(&c, BlockAddr::new("root", "extract[pages=01,lang=hy]"))
                .unwrap()[0],
            chain[0]
        );
    }

    #[test]
    fn file_inputs_recursively_infer_only_requested_languages() {
        let c = canvas(
            r#"<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```sh name="download" outputs="pdf/${lang}.pdf"
true
```
<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```sh name="convert" inputs="pdf/${lang}.pdf" outputs="text/${lang}.txt"
true
```
<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```sh name="extract" inputs="text/${lang}.txt" outputs="csv/${lang}.csv,csv/${lang}.csv"
true
```
```sh name="merge" inputs="csv/hy.csv,csv/en.csv"
true
```"#,
        );
        let chain = crate::deps::resolve_chain(&c, BlockAddr::new("root", "merge")).unwrap();
        assert_eq!(
            chain
                .iter()
                .map(|a| a.block_name.as_str())
                .collect::<Vec<_>>(),
            [
                "download[lang=hy]",
                "convert[lang=hy]",
                "extract[lang=hy]",
                "download[lang=en]",
                "convert[lang=en]",
                "extract[lang=en]",
                "merge"
            ]
        );
    }

    #[test]
    fn repeated_captures_agree_and_adjacent_captures_cannot_choose_greedily() {
        let repeated = canvas(
            r#"<!-- meshfox:arg name="lang" -->
```sh name="x" outputs="${lang}_${lang}.csv"
true
```"#,
        );
        assert_eq!(
            inferred(&repeated, "hy_hy.csv").unwrap(),
            Some(BlockAddr::new("root", "x[lang=hy]"))
        );
        assert_eq!(inferred(&repeated, "hy_en.csv").unwrap(), None);
        let adjacent = canvas(
            r#"<!-- meshfox:arg name="a" -->
<!-- meshfox:arg name="b" -->
```sh name="x" outputs="${a}${b}.csv"
true
```"#,
        );
        assert!(inferred(&adjacent, "hy.csv")
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
    }

    #[test]
    fn inferred_values_are_typed_and_all_required_arguments_must_be_known() {
        let typed = canvas(
            r#"<!-- meshfox:arg name="n" type="int" -->
<!-- meshfox:arg name="flag" type="bool" -->
```sh name="x" outputs="${n}_${flag}.csv"
true
```"#,
        );
        assert_eq!(
            inferred(&typed, "1_true.csv").unwrap(),
            Some(BlockAddr::new("root", "x[flag=true,n=1]"))
        );
        for file in ["no_true.csv", "1_yes.csv"] {
            assert!(inferred(&typed, file)
                .unwrap_err()
                .to_string()
                .contains("invalid inferred argument"));
        }
        assert_eq!(
            inferred(&typed, "01_true.csv").unwrap(),
            None,
            "canonical n=1 does not produce 01_true.csv"
        );
        let missing = canvas(
            r#"<!-- meshfox:arg name="lang" -->
<!-- meshfox:arg name="token" default="ui-only" required -->
```sh name="x" outputs="${lang}.csv"
true
```"#,
        );
        assert!(inferred(&missing, "hy.csv")
            .unwrap_err()
            .to_string()
            .contains("cannot infer required argument"));
    }

    #[test]
    fn overlap_between_different_producers_is_an_error_even_for_existing_files() {
        let c = canvas(
            r#"<!-- meshfox:arg name="lang" -->
```sh name="x" outputs="${lang}.csv"
true
```
```sh name="y" outputs="hy.csv"
true
```"#,
        );
        assert!(inferred(&c, "hy.csv")
            .unwrap_err()
            .to_string()
            .contains("ambiguous producer"));
    }

    #[test]
    fn globals_are_literal_and_locals_shadow_them_in_normalized_paths() {
        let mut c = canvas(
            r#"<!-- meshfox:var name="lang" default="wrong" -->
<!-- meshfox:arg name="lang" -->
```sh name="x" outputs="$WORK/./${lang}.csv"
true
```"#,
        );
        c.artifact_values
            .insert("WORK".into(), "__meshfox_argument_/folder".into());
        let addr = inferred(&c, "__meshfox_argument_/folder/հայ,with$dollar.csv")
            .unwrap()
            .unwrap();
        assert_eq!(addr.block_name, r#"x[lang="հայ,with$dollar"]"#);
        let block = crate::deps::find_block(&c, &addr).unwrap();
        assert_eq!(block.arguments["lang"], "հայ,with$dollar");
    }

    #[test]
    fn inferred_file_edges_detect_cycles_between_concrete_applications() {
        let c = canvas(
            r#"<!-- meshfox:arg name="lang" -->
```sh name="a" inputs="b/${lang}" outputs="a/${lang}"
true
```
<!-- meshfox:arg name="lang" -->
```sh name="b" inputs="a/${lang}" outputs="b/${lang}"
true
```"#,
        );
        assert!(matches!(
            crate::deps::resolve_chain(&c, BlockAddr::new("root", "a[lang=hy]")),
            Err(crate::DepsError::Cycle(_))
        ));
    }

    #[test]
    fn matching_has_a_finite_budget() {
        let c = canvas(
            r#"<!-- meshfox:arg name="n" type="int" -->
<!-- meshfox:arg name="a" -->
<!-- meshfox:arg name="b" -->
```sh name="x" outputs="${n}${a}${b}.csv"
true
```"#,
        );
        assert!(inferred(&c, &format!("{}.csv", "x".repeat(128)))
            .unwrap_err()
            .to_string()
            .contains("matching steps"));
    }

    #[test]
    fn producer_cwd_is_used_before_normalizing_and_matching() {
        let mut c = canvas(
            r#"```sh name="merge" inputs="csv/hy.csv"
true
```
## Producer
<!-- meshfox:node id="p" -->
<!-- meshfox:arg name="lang" -->
```sh name="extract" outputs="../csv/${lang}.csv"
true
```"#,
        );
        c.nodes.iter_mut().find(|n| n.id == "p").unwrap().asset_base = Some(
            std::env::current_dir()
                .unwrap()
                .join("producer")
                .display()
                .to_string(),
        );
        let chain = crate::deps::resolve_chain(&c, BlockAddr::new("root", "merge")).unwrap();
        assert_eq!(chain[0], BlockAddr::new("p", "extract[lang=hy]"));
    }

    #[test]
    fn computed_global_paths_are_observed_before_template_selection() {
        let mut c = canvas(
            r#"<!-- meshfox:var name="DIR" from="observe" -->
```sh name="observe"
true
```
<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```sh name="extract" outputs="$DIR/${lang}.csv"
true
```
```sh name="merge" inputs="csv/hy.csv"
true
```"#,
        );
        let target = BlockAddr::new("root", "merge");
        assert_eq!(
            crate::deps::resolve_chain(&c, target.clone()).unwrap(),
            [BlockAddr::new("root", "observe"), target.clone()]
        );
        c.artifact_values.insert("DIR".into(), "csv".into());
        assert_eq!(
            crate::deps::resolve_chain(&c, target.clone()).unwrap(),
            [
                BlockAddr::new("root", "observe"),
                BlockAddr::new("root", "extract[lang=hy]"),
                target
            ]
        );
    }

    #[test]
    fn an_unresolved_input_sentinel_is_never_captured_as_a_typed_argument() {
        let mut c = canvas(
            r#"<!-- meshfox:var name="LANG" from="observe" -->
```sh name="observe"
true
```
<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```sh name="extract" outputs="${lang}.csv"
true
```
```sh name="merge" inputs="${LANG}.csv"
true
```"#,
        );
        let target = BlockAddr::new("root", "merge");
        assert_eq!(
            crate::deps::resolve_chain(&c, target.clone()).unwrap(),
            [BlockAddr::new("root", "observe"), target.clone()]
        );
        c.artifact_values.insert("LANG".into(), "hy".into());
        assert_eq!(
            crate::deps::resolve_chain(&c, target.clone()).unwrap(),
            [
                BlockAddr::new("root", "observe"),
                BlockAddr::new("root", "extract[lang=hy]"),
                target
            ]
        );
    }

    #[test]
    fn globs_infer_existing_files_without_enumerating_argument_values() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-inference-glob-{}",
            crate::timestamp::now_utc_rfc3339().replace(':', "")
        ));
        std::fs::create_dir_all(dir.join("csv")).unwrap();
        let mut c = canvas(
            r#"<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```sh name="extract" outputs="csv/${lang}.csv"
true
```
```sh name="merge" inputs="csv/*.csv"
true
```"#,
        );
        c.artifact_root = dir.clone();
        let target = BlockAddr::new("root", "merge");
        let chain = crate::deps::resolve_chain(&c, target.clone()).unwrap();
        assert_eq!(chain.as_slice(), std::slice::from_ref(&target));
        std::fs::write(dir.join("csv/hy.csv"), "hy").unwrap();
        assert_eq!(
            crate::deps::resolve_chain(&c, target.clone()).unwrap(),
            [BlockAddr::new("root", "extract[lang=hy]"), target]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
