//! Demand-driven preparation of an execution graph. Secret reads belong here,
//! before admission, rather than to display or individual execution steps.
use std::collections::{HashMap, HashSet};

use crate::{BlockAddr, Canvas, DepsError, ResolvedVars, RunError, SharedEnv, VarCache, VarDecl};

#[derive(Debug)]
pub struct PreparedRun {
    pub chain: Vec<BlockAddr>,
    pub declarations: Vec<VarDecl>,
    pub resolved: ResolvedVars,
}

/// Discover the chain with the same dependency walk used by execution. Before
/// following an edge, resolve only that application's variables (and the output
/// namespace when it consumes artifacts). Values and backend failures are kept
/// for this preparation; repeated graph visits never re-prompt the secret store.
/// Missing ordinary inputs are returned for the caller's pre-run form. Computed
/// `from=` values remain deferred to execution, as in the ordinary resolver.
pub fn prepare_run(
    canvas: &mut Canvas,
    path: &[&str],
    block: &str,
    with_deps: bool,
    overrides: &HashMap<String, String>,
    cache: &VarCache,
    computed: &HashMap<String, String>,
    shared: &SharedEnv,
) -> Result<PreparedRun, RunError> {
    let target = crate::resolve_target(canvas, path, block)?;
    let decls = crate::declared_vars(canvas).map_err(DepsError::from)?;
    let cache = cache.for_run();
    let mut needed = HashSet::new();
    let mut values = canvas.artifact_values.clone();
    loop {
        let chain = {
            let mut resolve = |names: HashSet<String>| {
                needed.extend(crate::close_over_var_refs(
                    &decls,
                    names.iter().map(String::as_str),
                ));
                let relevant: Vec<_> = decls
                    .iter()
                    .filter(|d| needed.contains(&d.name))
                    .cloned()
                    .collect();
                let resolved =
                    crate::resolve_with_shared(&relevant, overrides, &cache, computed, shared);
                values.extend(resolved.values);
                values.clone()
            };
            crate::deps::resolve_chain_preparing(
                canvas,
                target.clone(),
                with_deps,
                &mut |graph, addr| {
                    let node = graph.node(&addr.node_id).ok_or_else(|| {
                        DepsError::BlockNotFound(addr.node_id.clone(), addr.block_name.clone())
                    })?;
                    let app = crate::args::Application::parse(&addr.block_name)
                        .map_err(DepsError::Arguments)?;
                    let raw_block = node
                        .runnable_blocks()
                        .into_iter()
                        .find(|b| b.name.as_deref() == Some(&app.definition))
                        .ok_or_else(|| {
                            DepsError::BlockNotFound(addr.node_id.clone(), addr.block_name.clone())
                        })?;
                    let signatures = crate::args::scan_node_signatures(node)
                        .map_err(|e| DepsError::Arguments(e.to_string()))?;
                    let locals: HashSet<_> = signatures
                        .iter()
                        .filter(|s| s.block.span == raw_block.span)
                        .flat_map(|s| s.args.iter().map(|a| a.name.as_str()))
                        .collect();
                    let mut names: HashSet<_> = crate::direct_var_seed(&raw_block)
                        .into_iter()
                        .filter(|name| !locals.contains(name.as_str()))
                        .collect();
                    names.extend(app.bindings.values().filter_map(|b| match b {
                        crate::args::Binding::Reference(name) => Some(name.clone()),
                        _ => None,
                    }));
                    let mut prepared = graph.clone();
                    prepared.artifact_values = resolve(names);
                    let bound = crate::deps::find_block(&prepared, addr)?;
                    let bound_addr = BlockAddr::new(&addr.node_id, bound.name.as_deref().unwrap());
                    let names = crate::env_var_names_for_chain(&prepared, &[bound_addr]);
                    prepared.artifact_values = resolve(names);
                    // Runtime-produced values can select a different application or output
                    // after execution starts. Prepare the finite set of secret declarations
                    // reachable through those definitions, without executing their sources.
                    let potential = potential_secrets(
                        &prepared,
                        &bound,
                        &addr.node_id,
                        &decls,
                        shared,
                        with_deps,
                    )?;
                    prepared.artifact_values = resolve(potential);
                    Ok(Some(prepared))
                },
            )?
        };
        canvas.artifact_values = values.clone();
        // A newly resolved output namespace can reveal producers not visible to an
        // earlier ancestor. Continue the same walk with the accumulated values.
        let final_chain = crate::resolve_run_chain(canvas, path, block, with_deps)?;
        if final_chain != chain {
            continue;
        }
        let declarations: Vec<_> = decls
            .into_iter()
            .filter(|d| needed.contains(&d.name))
            .collect();
        let resolved =
            crate::resolve_with_shared(&declarations, overrides, &cache, computed, shared);
        return Ok(PreparedRun {
            chain: final_chain,
            declarations,
            resolved,
        });
    }
}

/// Conservative admission for graph edges whose selector is produced at runtime.
/// Only potential secret inputs are added; ordinary fields of an unselected
/// producer are not turned into mandatory pre-run questions.
fn potential_secrets(
    canvas: &Canvas,
    block: &crate::CodeBlock,
    node_id: &str,
    decls: &[VarDecl],
    shared: &SharedEnv,
    with_deps: bool,
) -> Result<HashSet<String>, DepsError> {
    fn computed_name(decls: &[VarDecl], name: &str) -> bool {
        crate::close_over_var_refs(decls, [name])
            .iter()
            .any(|name| decls.iter().any(|d| &d.name == name && d.from.is_some()))
    }
    let mut candidates = Vec::new();
    if with_deps {
        for dep in &block.dependencies() {
            let app =
                crate::args::Application::parse(&dep.block_name).map_err(DepsError::Arguments)?;
            if app.bindings.values().any(|v| matches!(v, crate::args::Binding::Reference(name) if computed_name(decls, name))) {
                candidates.push(BlockAddr::new(dep.node_id.as_deref().unwrap_or(node_id), app.definition));
            }
        }
    }
    if block.attrs.contains_key("inputs") {
        let mut output_names = crate::output_path_var_names(canvas);
        output_names.extend(crate::artifacts::var_refs(block));
        if output_names.iter().any(|name| computed_name(decls, name)) {
            for node in &canvas.nodes {
                if node.plain_markdown_include {
                    continue;
                }
                candidates.extend(
                    node.runnable_blocks()
                        .into_iter()
                        .filter(|b| b.attrs.contains_key("outputs"))
                        .map(|b| BlockAddr::new(&node.id, b.name.unwrap())),
                );
            }
        }
    }
    let mut seen = HashSet::new();
    let mut names = HashSet::new();
    while let Some(addr) = candidates.pop() {
        if !seen.insert((addr.node_id.clone(), addr.block_name.clone())) {
            continue;
        }
        let Some(node) = canvas.node(&addr.node_id) else {
            continue;
        };
        let app =
            crate::args::Application::parse(&addr.block_name).map_err(DepsError::Arguments)?;
        let Some(raw) = node
            .runnable_blocks()
            .into_iter()
            .find(|b| b.name.as_deref() == Some(&app.definition))
        else {
            continue;
        };
        let signatures = crate::args::scan_node_signatures(node)
            .map_err(|e| DepsError::Arguments(e.to_string()))?;
        let locals: HashSet<_> = signatures
            .iter()
            .filter(|s| s.block.span == raw.span)
            .flat_map(|s| s.args.iter().map(|a| a.name.as_str()))
            .collect();
        let mut seeds: HashSet<_> = crate::direct_var_seed(&raw)
            .into_iter()
            .filter(|name| !locals.contains(name.as_str()))
            .collect();
        for env in &raw.env {
            if env.var_name.contains("${") {
                let mut pattern = env.var_name.clone();
                while let Some(start) = pattern.find("${") {
                    let Some(end) = pattern[start..].find('}') else {
                        break;
                    };
                    pattern.replace_range(start..start + end + 1, "*");
                }
                if let Ok(glob) = globset::Glob::new(&pattern) {
                    let matcher = glob.compile_matcher();
                    seeds.extend(
                        decls
                            .iter()
                            .filter(|d| matcher.is_match(&d.name))
                            .map(|d| d.name.clone()),
                    );
                }
            }
        }
        let closure = crate::close_over_var_refs(decls, seeds.iter().map(String::as_str));
        for d in decls.iter().filter(|d| closure.contains(&d.name)) {
            if d.secret || shared.contains_secret(&d.name) {
                names.insert(d.name.clone());
            }
            if let Some(from) = &d.from {
                candidates.push(crate::deps::resolve_ref(&node.id, from));
            }
        }
        if with_deps {
            for dep in &raw
                .dependencies()
                .into_iter()
                .chain(raw.defers())
                .collect::<Vec<_>>()
            {
                let app = crate::args::Application::parse(&dep.block_name)
                    .map_err(DepsError::Arguments)?;
                candidates.push(BlockAddr::new(
                    dep.node_id.as_deref().unwrap_or(&node.id),
                    app.definition,
                ));
            }
        }
        if raw.attrs.contains_key("inputs") {
            for producer_node in &canvas.nodes {
                if producer_node.plain_markdown_include {
                    continue;
                }
                candidates.extend(
                    producer_node
                        .runnable_blocks()
                        .into_iter()
                        .filter(|b| b.attrs.contains_key("outputs"))
                        .map(|b| BlockAddr::new(&producer_node.id, b.name.unwrap())),
                );
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::{doc_account, SecretBackend};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct Store {
        entries: HashMap<String, String>,
        reads: Mutex<Vec<String>>,
        denied: bool,
    }
    impl SecretBackend for Store {
        fn get(&self, account: &str) -> std::io::Result<Option<String>> {
            self.reads.lock().unwrap().push(account.into());
            if self.denied {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "access denied",
                ));
            }
            Ok(self.entries.get(account).cloned())
        }
        fn set(&self, _: &str, _: &str) -> std::io::Result<()> {
            unreachable!()
        }
        fn delete(&self, _: &str) -> std::io::Result<bool> {
            unreachable!()
        }
    }
    fn canvas(body: &str) -> Canvas {
        Canvas::from_markdown(&format!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n{body}"
        ))
        .unwrap()
    }
    fn cache(entries: &[(&str, &str)], denied: bool) -> (VarCache, Arc<Store>) {
        let path = std::path::Path::new("/test/lazy.canvas.md");
        let store = Arc::new(Store {
            entries: entries
                .iter()
                .map(|(k, v)| (doc_account(path, k), v.to_string()))
                .collect(),
            reads: Mutex::new(Vec::new()),
            denied,
        });
        (
            VarCache::in_memory().with_secret_backend(store.clone(), path),
            store,
        )
    }
    fn prepare(canvas: &mut Canvas, block: &str, cache: &VarCache) -> PreparedRun {
        prepare_run(
            canvas,
            &[],
            block,
            true,
            &HashMap::new(),
            cache,
            &HashMap::new(),
            &SharedEnv::new(),
        )
        .unwrap()
    }

    #[test]
    fn preview_and_a_secret_free_chain_do_not_contact_the_store() {
        let mut c = canvas("<!-- meshfox:var name=\"LAZY_TOKEN\" secret required -->\n```sh name=\"plain\"\necho ok\n```\n```sh name=\"private\" env=\"LAZY_TOKEN\"\necho private\n```\n");
        let (cache, store) = cache(&[], true);
        let decls = crate::declared_vars(&c).unwrap();
        crate::preview_with_shared(
            &decls,
            &HashMap::new(),
            &cache,
            &HashMap::new(),
            &SharedEnv::new(),
        );
        let plan = prepare(&mut c, "plain", &cache);
        assert!(plan.declarations.is_empty());
        assert!(store.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn prepares_secrets_for_the_whole_chain_once_and_keeps_failures() {
        let mut c = canvas("<!-- meshfox:var name=\"LAZY_TOKEN\" secret required -->\n```sh name=\"first\" env=\"LAZY_TOKEN\"\ntrue\n```\n```sh name=\"last\" deps=\"first\" env=\"LAZY_TOKEN\"\ntrue\n```\n");
        let (cache, store) = cache(&[], true);
        let plan = prepare(&mut c, "last", &cache);
        assert_eq!(plan.chain.len(), 2);
        assert_eq!(plan.resolved.missing[0].name, "LAZY_TOKEN");
        assert!(plan.resolved.secret_errors["LAZY_TOKEN"].contains("access denied"));
        assert_eq!(store.reads.lock().unwrap().len(), 1);
    }

    #[test]
    fn secret_artifact_path_discovers_a_producer_and_prepares_its_secrets() {
        let mut c = canvas("<!-- meshfox:var name=\"LAZY_PATH\" secret required -->\n<!-- meshfox:var name=\"LAZY_TOKEN\" secret required -->\n<!-- meshfox:var name=\"UNRELATED_SECRET\" secret required -->\n```sh name=\"producer\" outputs=\"$LAZY_PATH\" env=\"LAZY_TOKEN\"\ntrue\n```\n```sh name=\"consumer\" inputs=\"result.txt\"\ntrue\n```\n```sh name=\"unrelated\" env=\"UNRELATED_SECRET\"\ntrue\n```\n");
        let (cache, store) = cache(
            &[("LAZY_PATH", "result.txt"), ("LAZY_TOKEN", "token")],
            false,
        );
        let plan = prepare(&mut c, "consumer", &cache);
        assert_eq!(
            plan.chain
                .iter()
                .map(|a| a.block_name.as_str())
                .collect::<Vec<_>>(),
            ["producer", "consumer"]
        );
        assert_eq!(plan.resolved.values["LAZY_TOKEN"], "token");
        let reads = store.reads.lock().unwrap();
        assert_eq!(reads.len(), 2, "{reads:?}");
        assert!(reads[0].ends_with("LAZY_PATH"), "{reads:?}");
        assert!(reads[1].ends_with("LAZY_TOKEN"), "{reads:?}");
    }

    #[test]
    fn follows_indirect_variable_references_and_honors_overrides() {
        let mut c = canvas("<!-- meshfox:var name=\"LAZY_TOKEN\" secret required -->\n<!-- meshfox:var name=\"LAZY_ALIAS\" default_var=\"LAZY_TOKEN\" -->\n```sh name=\"use\" env=\"LAZY_ALIAS\"\ntrue\n```\n");
        let (cache, store) = cache(&[], true);
        let plan = prepare_run(
            &mut c,
            &[],
            "use",
            true,
            &HashMap::from([("LAZY_TOKEN".into(), "supplied".into())]),
            &cache,
            &HashMap::new(),
            &SharedEnv::new(),
        )
        .unwrap();
        assert_eq!(plan.resolved.values["LAZY_ALIAS"], "supplied");
        assert!(store.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn a_computed_environment_value_does_not_make_artifact_selection_dynamic() {
        let mut c = canvas(concat!(
            "<!-- meshfox:var name=\"LAZY_VALUE\" from=\"observe\" -->\n",
            "<!-- meshfox:var name=\"UNRELATED_SECRET\" secret required -->\n",
            "```sh name=\"observe\"\ntrue\n```\n",
            "```sh name=\"consumer\" inputs=\"fixed.txt\" env=\"LAZY_VALUE\"\ntrue\n```\n",
            "```sh name=\"other\" outputs=\"other.txt\" env=\"UNRELATED_SECRET\"\ntrue\n```\n",
        ));
        let (cache, store) = cache(&[], true);
        let plan = prepare(&mut c, "consumer", &cache);
        assert!(plan.resolved.secret_errors.is_empty());
        assert!(store.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn prepares_secret_candidates_behind_runtime_argument_selection() {
        let mut c = canvas(concat!(
            "<!-- meshfox:var name=\"LAZY_LANG\" from=\"observe\" -->\n",
            "<!-- meshfox:var name=\"LAZY_API_en\" secret required -->\n",
            "<!-- meshfox:var name=\"LAZY_API_hy\" secret required -->\n",
            "```sh name=\"observe\"\ntrue\n```\n",
            "<!-- meshfox:arg name=\"lang\" -->\n",
            "```sh name=\"child\" env=\"TOKEN=LAZY_API_${lang}\"\ntrue\n```\n",
            "```sh name=\"parent\" deps=\"child[lang=$LAZY_LANG]\"\ntrue\n```\n",
        ));
        let (cache, store) = cache(
            &[("LAZY_API_en", "english"), ("LAZY_API_hy", "armenian")],
            false,
        );
        let plan = prepare(&mut c, "parent", &cache);
        assert_eq!(plan.resolved.values["LAZY_API_en"], "english");
        assert_eq!(plan.resolved.values["LAZY_API_hy"], "armenian");
        assert_eq!(store.reads.lock().unwrap().len(), 2);
        assert!(!plan.chain.iter().any(|a| a.block_name.starts_with("child")));
    }

    #[test]
    fn secret_snapshot_is_new_for_each_preparation_and_no_deps_stays_scoped() {
        let mut c = canvas("<!-- meshfox:var name=\"LAZY_TOKEN\" secret required -->\n```sh name=\"child\" env=\"LAZY_TOKEN\"\ntrue\n```\n```sh name=\"parent\" deps=\"child\"\ntrue\n```\n");
        let (cache, store) = cache(&[("LAZY_TOKEN", "token")], false);
        prepare_run(
            &mut c,
            &[],
            "parent",
            false,
            &HashMap::new(),
            &cache,
            &HashMap::new(),
            &SharedEnv::new(),
        )
        .unwrap();
        assert!(store.reads.lock().unwrap().is_empty());
        prepare(&mut c, "parent", &cache);
        prepare(&mut c, "parent", &cache);
        assert_eq!(store.reads.lock().unwrap().len(), 2);
    }

    #[test]
    fn prepares_references_used_to_bind_dependency_arguments() {
        let mut c = canvas("<!-- meshfox:var name=\"LAZY_ARG\" secret required -->\n<!-- meshfox:arg name=\"value\" -->\n```sh name=\"child\"\ntrue\n```\n```sh name=\"parent\" deps=\"child[value=$LAZY_ARG]\"\ntrue\n```\n");
        let (cache, store) = cache(&[("LAZY_ARG", "hello")], false);
        let plan = prepare(&mut c, "parent", &cache);
        assert_eq!(plan.chain.len(), 2);
        assert!(plan.chain[0].block_name.contains("hello"));
        assert_eq!(store.reads.lock().unwrap().len(), 1);
    }
}
