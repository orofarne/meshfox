//! Document-scoped configuration variables (`<!-- meshfox:var ... -->`) —
//! the `INSTALL_PATH`-style values a canvas wants from whoever runs it,
//! asked for once and then remembered. See SPEC.md's "Variables" section
//! for the full writeup; `crate::varcache` is where an answer actually gets
//! persisted between runs.
//!
//! Declaration and resolution are deliberately split: this module only
//! ever reads (never prompts) — `meshfox configure`/`run`'s interactive
//! terminal prompt, and the server's HTTP form round-trip, are both callers
//! layered on top, not something this module knows about.

use crate::attrs::parse_attrs;
use crate::canvas::Canvas;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    String,
    Int,
    Bool,
    Select,
}

impl VarType {
    fn parse(s: &str) -> Option<VarType> {
        match s {
            "string" => Some(VarType::String),
            "int" => Some(VarType::Int),
            "bool" => Some(VarType::Bool),
            "select" => Some(VarType::Select),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            VarType::String => "string",
            VarType::Int => "int",
            VarType::Bool => "bool",
            VarType::Select => "select",
        }
    }
}

/// One `meshfox:var` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct VarDecl {
    pub name: String,
    pub var_type: VarType,
    /// Question text shown when prompting — defaults to `name` itself.
    pub prompt: String,
    pub default: Option<String>,
    /// Only non-empty for `Select`; `declared_vars`/`scan_var_decls` reject
    /// a `select` with none.
    pub choices: Vec<String>,
    /// Never persisted to the on-disk cache and never pre-filled from it —
    /// only `--set`/the process environment can supply one without an
    /// interactive prompt. See `crate::varcache`.
    pub secret: bool,
    /// Forces an interactive confirmation the first time this variable is
    /// needed, even when `default` would otherwise resolve it silently —
    /// `resolve`/`resolve_block_env` skip the `default` fallback for a
    /// `required` declaration, so it lands in `missing` (with `default`
    /// still attached, for the caller to offer as the prompt's pre-filled
    /// suggestion) until an override/the environment/the cache actually
    /// supplies a value. Once answered, the non-secret answer is cached
    /// same as any other, so later runs resolve it from there without
    /// asking again — this only affects the *first* confirmation, not
    /// every run.
    pub required: bool,
    /// `from="node-id/block-name"` — makes this a *computed* variable:
    /// instead of being prompted/defaulted/cached, its value comes from
    /// running the named block and reading back what it wrote to its own
    /// `crate::varout::VARS_OUT_ENV` file (see `crate::varout`). Mutually
    /// exclusive with `default`/`required`/`secret` (`build_var_decl`
    /// rejects combining them) — none of those mean anything for a value
    /// that's never cached, defaulted, or prompted for. A bare
    /// `from="block-name"` (no `node-id/`) means a block in the root
    /// node — `declared_vars` normalizes it to the root's own id, so by
    /// the time a `VarDecl` reaches any other code, `from`'s `node_id` is
    /// always `Some`.
    pub from: Option<crate::fence::BlockRef>,
    /// Never read from or written to the on-disk cache -- unlike
    /// `secret`, input isn't masked; this is about lifetime (never
    /// remembered past this one `meshfox run` invocation), not
    /// confidentiality. Combined with `required`, this guarantees a real
    /// prompt on every single run (a plain `session` alone still silently
    /// takes an `--set`/env/`default` value if one resolves it, exactly
    /// like any other declaration -- it just never remembers the answer).
    /// Mutually exclusive with `from=` (a computed value is already never
    /// cached, so `session` on it would be a no-op).
    pub session: bool,
    /// `default_var="OTHER_NAME"` -- this declaration's own `default`
    /// comes from another declared variable's resolved value instead of a
    /// literal string. Mutually exclusive with a literal `default=` and
    /// with `from=`. See `resolve`'s doc comment for how substitution
    /// works.
    pub default_var: Option<String>,
    /// `choices_var="OTHER_NAME"` -- same idea as `default_var`, but for
    /// `choices` (the referenced variable's resolved value is split the
    /// same comma-separated way a literal `choices=` already is).
    /// Mutually exclusive with a literal `choices=` and with `from=`;
    /// requires `type="select"`, same as a literal `choices=` does.
    pub choices_var: Option<String>,
}

#[derive(Debug, Error, PartialEq)]
pub enum VarsError {
    #[error("a meshfox:var comment is missing its required name= attribute")]
    MissingName,
    #[error("meshfox:var {0:?} has unknown type={1:?} (expected string, int, bool, or select)")]
    UnknownType(String, String),
    #[error("meshfox:var {0:?} has type=select but no choices= attribute")]
    SelectMissingChoices(String),
    #[error("duplicate meshfox:var name {0:?}")]
    DuplicateName(String),
    #[error("meshfox:var {0:?} is declared in node {1:?} — variables may only be declared in the root node")]
    NotInRoot(String, String),
    #[error("node {0:?} block {1:?} references undeclared variable {2:?} via env=")]
    UndeclaredEnvVar(String, String, String),
    #[error("meshfox:var {0:?} has an empty from= target")]
    EmptyFrom(String),
    #[error("meshfox:var {0:?} combines from= with {1}=, which isn't allowed for a computed variable")]
    FromConflict(String, &'static str),
    #[error("meshfox:var {0:?} combines default= with default_var=, only one may be given")]
    DefaultConflict(String),
    #[error("meshfox:var {0:?} combines choices= with choices_var=, only one may be given")]
    ChoicesConflict(String),
    #[error("meshfox:var {0:?} has choices_var= but type={1:?} (expected select)")]
    ChoicesVarRequiresSelect(String, String),
    #[error("meshfox:var {0:?} references undeclared variable {1:?} via {2}=")]
    UndeclaredVarRef(String, String, &'static str),
    #[error("cycle in default_var=/choices_var= references: {}", .0.join(" -> "))]
    VarRefCycle(Vec<String>),
}

/// Splits a comma-separated `choices=`-style string into trimmed,
/// non-empty tokens -- shared by `build_var_decl` (a literal `choices=`)
/// and `resolve` (a `choices_var=`-referenced variable's resolved value),
/// so both end up with lists shaped the same way.
fn split_choices(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn parse_var_comment(line: &str) -> Option<HashMap<String, String>> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let rest = inner.strip_prefix("meshfox:var")?;
    Some(parse_attrs(rest.trim()))
}

fn build_var_decl(attrs: HashMap<String, String>) -> Result<VarDecl, VarsError> {
    let name = attrs.get("name").cloned().ok_or(VarsError::MissingName)?;
    let var_type = match attrs.get("type") {
        None => VarType::String,
        Some(t) => {
            VarType::parse(t).ok_or_else(|| VarsError::UnknownType(name.clone(), t.clone()))?
        }
    };
    let choices: Vec<String> = attrs
        .get("choices")
        .map(|v| split_choices(v))
        .unwrap_or_default();
    let choices_var = attrs.get("choices_var").cloned();
    if !choices.is_empty() && choices_var.is_some() {
        return Err(VarsError::ChoicesConflict(name));
    }
    if choices_var.is_some() && var_type != VarType::Select {
        return Err(VarsError::ChoicesVarRequiresSelect(
            name,
            var_type.as_str().to_string(),
        ));
    }
    // A select needs *some* way to know its own choices -- either
    // literal, or deferred to whatever `choices_var` resolves to.
    if var_type == VarType::Select && choices.is_empty() && choices_var.is_none() {
        return Err(VarsError::SelectMissingChoices(name));
    }
    let secret = attrs.get("secret").map(|v| v != "false").unwrap_or(false);
    let required = attrs.get("required").map(|v| v != "false").unwrap_or(false);
    let session = attrs.get("session").map(|v| v != "false").unwrap_or(false);
    let prompt = attrs.get("prompt").cloned().unwrap_or_else(|| name.clone());
    let default = attrs.get("default").cloned();
    let default_var = attrs.get("default_var").cloned();
    if default.is_some() && default_var.is_some() {
        return Err(VarsError::DefaultConflict(name));
    }
    let from = match attrs.get("from") {
        None => None,
        Some(s) if s.trim().is_empty() => return Err(VarsError::EmptyFrom(name)),
        Some(s) => Some(crate::fence::parse_block_ref(s.trim())),
    };
    if from.is_some() {
        if default.is_some() {
            return Err(VarsError::FromConflict(name, "default"));
        }
        if required {
            return Err(VarsError::FromConflict(name, "required"));
        }
        if secret {
            return Err(VarsError::FromConflict(name, "secret"));
        }
        if session {
            return Err(VarsError::FromConflict(name, "session"));
        }
        if default_var.is_some() {
            return Err(VarsError::FromConflict(name, "default_var"));
        }
        if choices_var.is_some() {
            return Err(VarsError::FromConflict(name, "choices_var"));
        }
    }
    Ok(VarDecl {
        name,
        var_type,
        prompt,
        default,
        choices,
        secret,
        required,
        from,
        session,
        default_var,
        choices_var,
    })
}

/// Whether `value` is an acceptable *fresh* answer for `decl`'s own
/// `type` — `string` accepts anything; `int` must parse as a (signed)
/// integer; `bool` must be exactly `"true"` or `"false"`, the canonical
/// form every control that actually constrains a `bool` field already
/// produces (the TUI's left/right toggle, the web `VarsForm`'s checkbox,
/// the CLI prompt's own y/n handling); `select` must be exactly one of
/// `choices`.
///
/// Deliberately *not* called anywhere in `resolve`/`resolve_block_env`
/// themselves (see their own doc comments) — an already-cached, `--set`,
/// or process-environment value can predate a stricter `type=`/
/// `choices=` edit to the declaration, and resolution silently breaking
/// every run over that would be worse than just using the stale value.
/// This is for whoever is about to *accept a new answer* — an
/// interactive prompt, `--set`, a submitted form/API request — to check
/// before saving it, catching a bypass of a UI's own control (a `select`
/// answered via a raw `--set REGION=mars` or a direct API call, say)
/// that the control itself would never have produced.
pub fn validate_value(decl: &VarDecl, value: &str) -> Result<(), String> {
    match decl.var_type {
        VarType::String => Ok(()),
        VarType::Int => value
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| format!("{:?} expects an integer, got {value:?}", decl.name)),
        VarType::Bool => {
            if value == "true" || value == "false" {
                Ok(())
            } else {
                Err(format!(
                    "{:?} expects true or false, got {value:?}",
                    decl.name
                ))
            }
        }
        VarType::Select => {
            if decl.choices.is_empty() && decl.choices_var.is_some() {
                // A `choices_var`-declared select's real choices only
                // exist once `resolve` has substituted them in -- a
                // `decl` handed to this function straight from
                // `declared_vars` (e.g. eager `--set` validation, before
                // any run has actually resolved anything) has nothing
                // real to check membership against yet, so there's
                // nothing to reject here.
                return Ok(());
            }
            if decl.choices.iter().any(|c| c == value) {
                Ok(())
            } else {
                Err(format!(
                    "{:?} expects one of [{}], got {value:?}",
                    decl.name,
                    decl.choices.join(", ")
                ))
            }
        }
    }
}

/// Scans `markdown` (a node's own body text) for `meshfox:var` comments,
/// fence-aware — a line inside a code fence (e.g. a worked example showing
/// off the syntax itself) is never mistaken for a real declaration, same
/// convention `mdcanvas::scan`/`fence::candidate_fences` already use.
pub fn scan_var_decls(markdown: &str) -> Result<Vec<VarDecl>, VarsError> {
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let mut fi = 0;
    let mut decls = Vec::new();
    let mut offset = 0;
    for line in markdown.split('\n') {
        let start = offset;
        offset += line.len() + 1;
        while fi < fence_ranges.len() && fence_ranges[fi].end <= start {
            fi += 1;
        }
        if fi < fence_ranges.len() && fence_ranges[fi].start <= start {
            continue;
        }
        if let Some(attrs) = parse_var_comment(line) {
            decls.push(build_var_decl(attrs)?);
        }
    }
    Ok(decls)
}

/// Every variable `canvas` declares, in document order — always from the
/// root node only (see `VarsError::NotInRoot`: a `meshfox:var` found in any
/// other node is a `meshfox validate` error, not silently ignored, so a
/// misplaced declaration doesn't quietly do nothing). Also rejects a
/// repeated `name` across the document.
pub fn declared_vars(canvas: &Canvas) -> Result<Vec<VarDecl>, VarsError> {
    let mut root_decls = Vec::new();
    let mut root_id = None;
    for node in &canvas.nodes {
        let decls = scan_var_decls(&node.text)?;
        if node.parent.is_none() {
            root_id = Some(node.id.clone());
            root_decls = decls;
        } else if let Some(first) = decls.into_iter().next() {
            return Err(VarsError::NotInRoot(first.name, node.id.clone()));
        }
    }

    // A bare `from="block-name"` (no `node-id/`) means a block in the root
    // node — same convention `deps=`'s bare form uses for "same node as the
    // one declaring it", except a variable is always declared in root, so
    // there's no other node it could mean. Normalizing here means every
    // other consumer of a `VarDecl` (deps chain resolution, the runners)
    // can treat `from`'s `node_id` as always populated.
    if let Some(root_id) = &root_id {
        for decl in &mut root_decls {
            if let Some(from) = &mut decl.from {
                if from.node_id.is_none() {
                    from.node_id = Some(root_id.clone());
                }
            }
        }
    }

    let mut seen = HashSet::new();
    for decl in &root_decls {
        if !seen.insert(decl.name.clone()) {
            return Err(VarsError::DuplicateName(decl.name.clone()));
        }
    }
    Ok(root_decls)
}

/// Validates that every runnable fence's `env=` (see `crate::fence::EnvRef`)
/// only ever references a variable this document actually declares —
/// `meshfox validate`'s counterpart to `deps::validate` catching a `deps=`
/// reference to a block that doesn't exist. A block's `env=` is what scopes
/// variable resolution to just what it needs (see `resolve_block_env`), so
/// a typo here would otherwise just silently resolve to nothing instead of
/// failing loudly.
pub fn validate_env_refs(canvas: &Canvas) -> Result<(), VarsError> {
    let decls = declared_vars(canvas)?;
    let declared: HashSet<&str> = decls.iter().map(|d| d.name.as_str()).collect();
    for node in &canvas.nodes {
        for block in crate::fence::scan_runnable_blocks(&node.id, &node.text) {
            for env_ref in &block.env {
                if !declared.contains(env_ref.var_name.as_str()) {
                    return Err(VarsError::UndeclaredEnvVar(
                        node.id.clone(),
                        block.name.clone().unwrap_or_default(),
                        env_ref.var_name.clone(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validates that every `default_var`/`choices_var` in `canvas` names an
/// actually-declared variable, and that the reference graph has no
/// cycles -- `meshfox validate`'s counterpart to `validate_env_refs`,
/// catching a typo'd or circular reference before it silently degrades
/// into `resolve`'s own graceful "just don't substitute this round"
/// fallback.
pub fn validate_var_refs(canvas: &Canvas) -> Result<(), VarsError> {
    let decls = declared_vars(canvas)?;
    let declared: HashSet<&str> = decls.iter().map(|d| d.name.as_str()).collect();
    for decl in &decls {
        for (reference, attr) in [
            (&decl.default_var, "default_var"),
            (&decl.choices_var, "choices_var"),
        ] {
            if let Some(name) = reference {
                if !declared.contains(name.as_str()) {
                    return Err(VarsError::UndeclaredVarRef(
                        decl.name.clone(),
                        name.clone(),
                        attr,
                    ));
                }
            }
        }
    }

    let by_name: HashMap<&str, &VarDecl> = decls.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut visited = HashSet::new();
    for decl in &decls {
        let mut stack = Vec::new();
        if let Some(cycle) = find_var_ref_cycle(decl, &by_name, &mut visited, &mut stack) {
            return Err(VarsError::VarRefCycle(cycle));
        }
    }
    Ok(())
}

fn find_var_ref_cycle(
    decl: &VarDecl,
    by_name: &HashMap<&str, &VarDecl>,
    visited: &mut HashSet<String>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    if visited.contains(&decl.name) {
        return None;
    }
    if let Some(pos) = stack.iter().position(|n| n == &decl.name) {
        let mut cycle = stack[pos..].to_vec();
        cycle.push(decl.name.clone());
        return Some(cycle);
    }
    stack.push(decl.name.clone());
    for reference in [&decl.default_var, &decl.choices_var].into_iter().flatten() {
        if let Some(referenced) = by_name.get(reference.as_str()) {
            if let Some(cycle) = find_var_ref_cycle(referenced, by_name, visited, stack) {
                return Some(cycle);
            }
        }
    }
    stack.pop();
    visited.insert(decl.name.clone());
    None
}

/// Every variable name transitively reachable from `seed` by following
/// `default_var`/`choices_var` references (each seed name is always
/// included, whether or not it's actually declared) -- i.e. every var
/// whose own resolved value might be needed to materialize some var in
/// `seed`'s own `default`/`choices`. Used everywhere "which vars does X
/// actually need" is computed for a *scoped* subset of a document's
/// declarations (`resolve_block_env`'s own relevant-decls filter,
/// `deps::visit`'s implicit `from=` edges, `env_var_names_for_chain`) so a
/// var referenced only indirectly -- never in anyone's literal `env=` --
/// is still resolved (and, if it's itself `from=`-computed, still run)
/// wherever it matters.
pub fn close_over_var_refs<'a>(
    decls: &[VarDecl],
    seed: impl IntoIterator<Item = &'a str>,
) -> HashSet<String> {
    let by_name: HashMap<&str, &VarDecl> = decls.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut closure: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = seed.into_iter().map(String::from).collect();
    while let Some(name) = frontier.pop() {
        if !closure.insert(name.clone()) {
            continue;
        }
        if let Some(decl) = by_name.get(name.as_str()) {
            for reference in [&decl.default_var, &decl.choices_var].into_iter().flatten() {
                if !closure.contains(reference) {
                    frontier.push(reference.clone());
                }
            }
        }
    }
    closure
}

/// Result of resolving a set of declared variables — see `resolve`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedVars {
    /// name -> value, ready to inject into a spawned block's environment.
    pub values: HashMap<String, String>,
    /// Declarations that still need interactive input, in declaration
    /// order — however the caller prompts (terminal, HTTP form, ...).
    /// Never contains a `from`-declared one — see `unresolved_from`.
    pub missing: Vec<VarDecl>,
    /// `from`-declared declarations whose source block hasn't (yet, or
    /// ever, if it failed) produced a value in `computed` — see `resolve`.
    /// A caller must never prompt for one of these; it's a hard error
    /// (the run's chain resolution should have run the source block first,
    /// so a non-empty list here means either it failed, or it didn't write
    /// this name to its own output — either way, nothing to ask a human).
    pub unresolved_from: Vec<VarDecl>,
}

/// Resolves `decls` against, in priority order: `overrides` (explicit
/// `--set`/a submitted web form), the current process environment, the
/// on-disk cache (skipped entirely for a `secret` declaration — it's never
/// read from or written to `cache`, see `crate::varcache`), and finally
/// each declaration's own `default` — except for a `required` declaration,
/// which skips that last step: with nothing else supplying a value, it
/// lands in `missing` (still carrying `default`, for the caller to offer
/// as the prompt's pre-filled suggestion) rather than silently taking the
/// default. This is only a one-time confirmation, not a standing "always
/// ask" — once answered, a non-secret answer is written to `cache` same as
/// any other resolution, so the very next lookup finds it there and
/// resolves without prompting again. Whatever isn't resolved ends up in
/// `missing`, for the caller to prompt for and resolve again — a
/// variable's own type/`choices` are informational for that prompt only;
/// nothing here validates an incoming value against them (an incoming
/// value is just a plain string, the same as any other env var).
///
/// A `from`-declared decl is never touched by any of the four steps
/// above — not even `overrides` — since letting `--set`/a submitted form
/// answer a computed variable would let it silently impersonate a value
/// the source block never actually produced. It's resolved exclusively
/// from `computed`: the name -> value map a runner has already populated
/// by running that decl's `from=` block and reading back what it wrote to
/// its own `crate::varout::VARS_OUT_ENV` file. Absent from `computed`, it
/// lands in `unresolved_from`, never `missing` — see `ResolvedVars`.
///
/// `decls` is processed in *topological* order over `default_var`/
/// `choices_var` edges (see `topo_order`) rather than the given order, so
/// that by the time a decl referencing another one is reached, the
/// referenced decl (if resolvable at all) has already landed in `values`.
/// A decl whose `default_var`/`choices_var` reference isn't resolvable
/// this round (the referenced decl is itself `missing`/`unresolved_from`,
/// or -- defensively -- isn't present in this particular `decls` slice at
/// all) is skipped entirely: it lands in neither `values` nor `missing`
/// this call, deferred to a later one (e.g. once the referenced var has
/// been answered elsewhere in the same run). This never happens for the
/// expected pattern of referencing a `from=` variable, since chain
/// resolution (`deps::visit`) already guarantees that one's source block
/// has run -- and so it's already in `computed` -- before anything that
/// needs it via `default_var`/`choices_var` is resolved; see
/// `close_over_var_refs`. A genuinely unresolvable reference (a cycle, or
/// a name nothing declares) is what `validate_var_refs` catches as a real
/// `meshfox validate` error -- `resolve` itself stays infallible and just
/// degrades gracefully.
pub fn resolve(
    decls: &[VarDecl],
    overrides: &HashMap<String, String>,
    cache: &crate::varcache::VarCache,
    computed: &HashMap<String, String>,
) -> ResolvedVars {
    let mut values = HashMap::new();
    let mut missing = Vec::new();
    let mut unresolved_from = Vec::new();
    for decl in topo_order(decls) {
        if decl.from.is_some() {
            match computed.get(&decl.name) {
                Some(v) => {
                    values.insert(decl.name.clone(), v.clone());
                }
                None => unresolved_from.push(decl.clone()),
            }
            continue;
        }

        // override -> env -> cache first, same three steps as any other
        // decl -- none of these need `default_var`/`choices_var`
        // substitution at all, since they already supply a final answer
        // outright. Checking them before touching `default_var`/
        // `choices_var` (rather than substituting unconditionally, up
        // front) is what lets an already-known answer resolve even when
        // the referenced variable genuinely can't be (e.g. a client
        // supplying `REGION` directly before `REGIONS_LIST`'s own
        // `from=` source has ever run) -- there's nothing to defer for if
        // nothing downstream needs the substituted value in the first
        // place.
        let already_resolved = overrides
            .get(&decl.name)
            .cloned()
            .or_else(|| std::env::var(&decl.name).ok())
            .or_else(|| {
                // `session` skips the cache the same way `secret` does --
                // never remembered past this one resolution pass. See
                // `VarDecl::session`'s own doc comment for how a caller
                // avoids re-prompting on every block within one run.
                if decl.secret || decl.session {
                    None
                } else {
                    cache.get(&decl.name).map(str::to_string)
                }
            });
        if let Some(v) = already_resolved {
            values.insert(decl.name.clone(), v);
            continue;
        }

        // Not resolved yet -- only *now* do we need the effective
        // default/choices (substituted from `default_var`/`choices_var`
        // when set), either to silently take the default (a non-required
        // decl) or to build the prompt's own fields (`missing`).
        let effective_default = match &decl.default_var {
            Some(ref_name) => match values.get(ref_name) {
                Some(v) => Some(v.clone()),
                None => continue, // referenced var not resolvable yet -- defer
            },
            None => decl.default.clone(),
        };
        let effective_choices = match &decl.choices_var {
            Some(ref_name) => match values.get(ref_name) {
                Some(v) => split_choices(v),
                None => continue, // referenced var not resolvable yet -- defer
            },
            None => decl.choices.clone(),
        };

        if !decl.required {
            if let Some(v) = effective_default.clone() {
                values.insert(decl.name.clone(), v);
                continue;
            }
        }

        // The `missing` entry carries the *substituted* default/choices,
        // not the decl's own (possibly absent) literal ones -- this is
        // the only place that ever needs to know `default_var`/
        // `choices_var` exist; every consumer of `missing` (a terminal
        // prompt, a form) just reads `default`/`choices` off whatever
        // `VarDecl` it's handed.
        let mut missing_decl = decl.clone();
        missing_decl.default = effective_default;
        missing_decl.choices = effective_choices;
        missing.push(missing_decl);
    }
    ResolvedVars {
        values,
        missing,
        unresolved_from,
    }
}

/// Orders `decls` so that any decl named by another's `default_var`/
/// `choices_var` comes first -- a small DFS-based topological sort,
/// deliberately infallible (see `resolve`'s own doc comment for why): a
/// cycle just stops recursing at the point it's detected (that decl still
/// gets visited later, from the top-level loop, in whatever order it
/// naturally comes up), and a reference to a name not present in `decls`
/// simply has nothing to visit.
fn topo_order(decls: &[VarDecl]) -> Vec<&VarDecl> {
    let by_name: HashMap<&str, &VarDecl> = decls.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut order = Vec::with_capacity(decls.len());
    let mut visited = HashSet::new();
    let mut stack = HashSet::new();
    for decl in decls {
        visit_for_topo_order(decl, &by_name, &mut order, &mut visited, &mut stack);
    }
    order
}

fn visit_for_topo_order<'a>(
    decl: &'a VarDecl,
    by_name: &HashMap<&str, &'a VarDecl>,
    order: &mut Vec<&'a VarDecl>,
    visited: &mut HashSet<String>,
    stack: &mut HashSet<String>,
) {
    if visited.contains(&decl.name) || stack.contains(&decl.name) {
        return;
    }
    stack.insert(decl.name.clone());
    for reference in [&decl.default_var, &decl.choices_var].into_iter().flatten() {
        if let Some(referenced) = by_name.get(reference.as_str()) {
            visit_for_topo_order(referenced, by_name, order, visited, stack);
        }
    }
    stack.remove(&decl.name);
    visited.insert(decl.name.clone());
    order.push(decl);
}

/// One block's own resolved environment — see `resolve_block_env`.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEnvResolution {
    /// local env var name -> value, ready to inject into this block's
    /// spawned process — exactly (and only) what its own `env=` asked
    /// for, under whatever local names it asked for them as.
    pub env: HashMap<String, String>,
    /// Declared variables (by their *own* name, not any block-local
    /// rename) still needing interactive input, in `env_refs` order.
    /// Never contains a `from`-declared one — see `unresolved_from`.
    pub missing: Vec<VarDecl>,
    /// `from`-declared variables this block's `env=` references whose
    /// source block hasn't produced a value yet — see
    /// `ResolvedVars::unresolved_from`. A caller must treat this as a hard
    /// error, never a prompt.
    pub unresolved_from: Vec<VarDecl>,
}

/// Resolves *only* the declared variables a single block's own `env=`
/// references (`env_refs`) — a subset of `decls`, not every variable the
/// document declares — against `overrides`/the process environment/the
/// on-disk `cache`/each declaration's own `default`/`computed` (for a
/// `from`-declared one), same precedence as `resolve`. This is what keeps
/// running a block that doesn't use `env=` at all from ever resolving (or
/// prompting for) *any* declared variable — see SPEC.md's "Variables".
pub fn resolve_block_env(
    env_refs: &[crate::fence::EnvRef],
    decls: &[VarDecl],
    overrides: &HashMap<String, String>,
    cache: &crate::varcache::VarCache,
    computed: &HashMap<String, String>,
) -> BlockEnvResolution {
    // The closure, not just the literal names in `env_refs`: a var
    // referenced only via another (directly-needed) var's `default_var`/
    // `choices_var` still has to be resolved here too, or that other
    // var's dynamic default/choices could never be substituted. See
    // `close_over_var_refs`.
    let needed = close_over_var_refs(decls, env_refs.iter().map(|e| e.var_name.as_str()));
    let relevant: Vec<VarDecl> = decls
        .iter()
        .filter(|d| needed.contains(d.name.as_str()))
        .cloned()
        .collect();
    let resolved = resolve(&relevant, overrides, cache, computed);
    BlockEnvResolution {
        env: map_block_env(env_refs, &resolved.values),
        missing: resolved.missing,
        unresolved_from: resolved.unresolved_from,
    }
}

/// Projects already-resolved declared-variable values (keyed by their own
/// name, e.g. from `resolve`'s `values`) down to one block's own `env=`
/// list — local name -> value, only for whichever references actually
/// resolved. Split out from `resolve_block_env` so a caller that already
/// resolved a whole run chain's worth of variables at once (e.g. the
/// server, before streaming any output) doesn't need to re-run `resolve`
/// per block just to relabel them.
pub fn map_block_env(
    env_refs: &[crate::fence::EnvRef],
    resolved_values: &HashMap<String, String>,
) -> HashMap<String, String> {
    env_refs
        .iter()
        .filter_map(|er| {
            resolved_values
                .get(&er.var_name)
                .map(|v| (er.local_name.clone(), v.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::varcache::VarCache;

    fn canvas(md: &str) -> Canvas {
        Canvas::from_markdown(md).unwrap()
    }

    #[test]
    fn scans_a_simple_string_var() {
        let md = "<!-- meshfox:var name=\"INSTALL_PATH\" default=\"/usr/local/bin\" -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "INSTALL_PATH");
        assert_eq!(decls[0].var_type, VarType::String);
        assert_eq!(decls[0].prompt, "INSTALL_PATH");
        assert_eq!(decls[0].default.as_deref(), Some("/usr/local/bin"));
        assert!(!decls[0].secret);
        assert!(!decls[0].required);
    }

    #[test]
    fn scans_required_flag() {
        let md = "<!-- meshfox:var name=\"INSTALL_PATH\" default=\"/usr/local/bin\" required -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert!(decls[0].required);
    }

    #[test]
    fn scans_prompt_type_choices_and_secret() {
        let md = concat!(
            "<!-- meshfox:var name=\"LOG_LEVEL\" type=\"select\" choices=\"debug,info,warn\" ",
            "prompt=\"Log level?\" default=\"info\" -->\n",
            "<!-- meshfox:var name=\"API_TOKEN\" secret -->\n",
        );
        let decls = scan_var_decls(md).unwrap();
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].var_type, VarType::Select);
        assert_eq!(decls[0].choices, vec!["debug", "info", "warn"]);
        assert_eq!(decls[0].prompt, "Log level?");
        assert!(decls[1].secret);
        assert_eq!(decls[1].prompt, "API_TOKEN"); // falls back to name
    }

    #[test]
    fn ignores_var_comments_inside_a_fence() {
        let md = "```text\n<!-- meshfox:var name=\"NOT_REAL\" -->\n```\n";
        assert!(scan_var_decls(md).unwrap().is_empty());
    }

    #[test]
    fn missing_name_is_an_error() {
        let md = "<!-- meshfox:var type=\"string\" -->\n";
        assert_eq!(scan_var_decls(md).unwrap_err(), VarsError::MissingName);
    }

    #[test]
    fn unknown_type_is_an_error() {
        let md = "<!-- meshfox:var name=\"X\" type=\"float\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::UnknownType("X".to_string(), "float".to_string())
        );
    }

    #[test]
    fn select_without_choices_is_an_error() {
        let md = "<!-- meshfox:var name=\"X\" type=\"select\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::SelectMissingChoices("X".to_string())
        );
    }

    #[test]
    fn declared_vars_reads_only_the_root_node() {
        let doc = concat!(
            "# Root\n\n",
            "<!-- meshfox:var name=\"INSTALL_PATH\" default=\"/usr/local/bin\" -->\n\n",
            "## Install\n<!-- meshfox:node id=\"install\" -->\n\n",
            "```bash name=\"install\"\necho hi\n```\n",
        );
        let c = canvas(doc);
        let decls = declared_vars(&c).unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "INSTALL_PATH");
    }

    #[test]
    fn declared_vars_rejects_a_declaration_outside_the_root() {
        let doc = concat!(
            "# Root\n\n",
            "## Install\n<!-- meshfox:node id=\"install\" -->\n\n",
            "<!-- meshfox:var name=\"INSTALL_PATH\" -->\n",
        );
        let c = canvas(doc);
        assert_eq!(
            declared_vars(&c).unwrap_err(),
            VarsError::NotInRoot("INSTALL_PATH".to_string(), "install".to_string())
        );
    }

    #[test]
    fn declared_vars_rejects_duplicate_names() {
        let doc = concat!(
            "# Root\n\n",
            "<!-- meshfox:var name=\"X\" default=\"1\" -->\n",
            "<!-- meshfox:var name=\"X\" default=\"2\" -->\n",
        );
        let c = canvas(doc);
        assert_eq!(
            declared_vars(&c).unwrap_err(),
            VarsError::DuplicateName("X".to_string())
        );
    }

    fn decl(name: &str, default: Option<&str>, secret: bool) -> VarDecl {
        required_decl(name, default, secret, false)
    }

    fn required_decl(name: &str, default: Option<&str>, secret: bool, required: bool) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type: VarType::String,
            prompt: name.to_string(),
            default: default.map(String::from),
            choices: Vec::new(),
            secret,
            required,
            from: None,
            session: false,
            default_var: None,
            choices_var: None,
        }
    }

    fn typed_decl(name: &str, var_type: VarType, choices: &[&str]) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type,
            prompt: name.to_string(),
            default: None,
            choices: choices.iter().map(|s| s.to_string()).collect(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: None,
            choices_var: None,
        }
    }

    fn from_decl(name: &str, from: crate::fence::BlockRef) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type: VarType::String,
            prompt: name.to_string(),
            default: None,
            choices: Vec::new(),
            secret: false,
            required: false,
            from: Some(from),
            session: false,
            default_var: None,
            choices_var: None,
        }
    }

    fn block_ref(node_id: &str, block_name: &str) -> crate::fence::BlockRef {
        crate::fence::BlockRef {
            node_id: Some(node_id.to_string()),
            block_name: block_name.to_string(),
        }
    }

    #[test]
    fn validate_value_string_accepts_anything() {
        let d = typed_decl("X", VarType::String, &[]);
        assert!(validate_value(&d, "").is_ok());
        assert!(validate_value(&d, "anything at all, really").is_ok());
    }

    #[test]
    fn validate_value_int_requires_a_parseable_integer() {
        let d = typed_decl("X", VarType::Int, &[]);
        assert!(validate_value(&d, "42").is_ok());
        assert!(validate_value(&d, "-7").is_ok());
        assert!(validate_value(&d, "not a number").is_err());
        assert!(validate_value(&d, "3.14").is_err());
        assert!(validate_value(&d, "").is_err());
    }

    #[test]
    fn validate_value_bool_requires_the_canonical_true_or_false() {
        let d = typed_decl("X", VarType::Bool, &[]);
        assert!(validate_value(&d, "true").is_ok());
        assert!(validate_value(&d, "false").is_ok());
        assert!(validate_value(&d, "yes").is_err());
        assert!(validate_value(&d, "1").is_err());
        assert!(validate_value(&d, "").is_err());
    }

    #[test]
    fn validate_value_select_requires_membership_in_choices() {
        let d = typed_decl("X", VarType::Select, &["debug", "info", "warn"]);
        assert!(validate_value(&d, "info").is_ok());
        assert!(validate_value(&d, "trace").is_err());
        assert!(validate_value(&d, "").is_err());
    }

    #[test]
    fn validate_value_select_with_unsubstituted_choices_var_accepts_anything() {
        // A raw declaration (straight from `declared_vars`, not yet run
        // through `resolve`) has no real choices to check against --
        // eager `--set`/override validation must not reject a
        // choices_var-declared select just because it hasn't been
        // resolved yet.
        let d = choices_var_decl("X", "LIST");
        assert!(validate_value(&d, "anything at all").is_ok());
    }

    #[test]
    fn resolve_prefers_override_over_env_and_default() {
        let decls = vec![decl("X", Some("default-val"), false)];
        let mut overrides = HashMap::new();
        overrides.insert("X".to_string(), "override-val".to_string());
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &overrides, &cache, &HashMap::new());
        assert_eq!(
            resolved.values.get("X").map(String::as_str),
            Some("override-val")
        );
        assert!(resolved.missing.is_empty());
    }

    #[test]
    fn resolve_falls_back_to_cache_then_default() {
        let decls = vec![
            decl("X", Some("default-val"), false),
            decl("Y", Some("default-val"), false),
        ];
        let mut cache = VarCache::in_memory();
        cache.set("X", "cached-val").unwrap();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert_eq!(
            resolved.values.get("X").map(String::as_str),
            Some("cached-val")
        );
        assert_eq!(
            resolved.values.get("Y").map(String::as_str),
            Some("default-val")
        );
    }

    #[test]
    fn resolve_reports_missing_when_nothing_resolves_it() {
        let decls = vec![decl("X", None, false)];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolved.values.is_empty());
        assert_eq!(resolved.missing, decls);
    }

    #[test]
    fn resolve_never_reads_a_secret_from_cache() {
        let decls = vec![decl("TOKEN", None, true)];
        let mut cache = VarCache::in_memory();
        cache
            .values_mut_for_test()
            .insert("TOKEN".to_string(), "leaked".to_string());
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolved.values.is_empty());
        assert_eq!(resolved.missing.len(), 1);
    }

    #[test]
    fn resolve_required_with_a_default_is_still_missing_until_confirmed() {
        // Unlike a plain declaration, a `required` one must not silently
        // take its `default` — it needs an explicit first confirmation.
        let decls = vec![required_decl("X", Some("default-val"), false, true)];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolved.values.is_empty());
        assert_eq!(resolved.missing, decls);
        // The declaration in `missing` still carries `default`, so the
        // caller can offer it as the prompt's pre-filled suggestion.
        assert_eq!(resolved.missing[0].default.as_deref(), Some("default-val"));
    }

    #[test]
    fn resolve_required_reads_from_cache_once_confirmed() {
        // Once an answer (even the default, confirmed as-is) has been
        // cached, a `required` variable resolves like any other — it's a
        // one-time confirmation, not a standing "always ask".
        let decls = vec![required_decl("X", Some("default-val"), false, true)];
        let mut cache = VarCache::in_memory();
        cache.set("X", "default-val").unwrap();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert_eq!(
            resolved.values.get("X").map(String::as_str),
            Some("default-val")
        );
        assert!(resolved.missing.is_empty());
    }

    #[test]
    fn resolve_required_still_prefers_override_and_env_over_prompting() {
        let decls = vec![required_decl("X", Some("default-val"), false, true)];
        let mut overrides = HashMap::new();
        overrides.insert("X".to_string(), "override-val".to_string());
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &overrides, &cache, &HashMap::new());
        assert_eq!(
            resolved.values.get("X").map(String::as_str),
            Some("override-val")
        );
        assert!(resolved.missing.is_empty());
    }

    #[test]
    fn validate_env_refs_ok_when_every_reference_is_declared() {
        let doc = concat!(
            "# Root\n\n",
            "<!-- meshfox:var name=\"INSTALL_PATH\" default=\"/usr/local/bin\" -->\n\n",
            "## Install\n<!-- meshfox:node id=\"install\" -->\n\n",
            "```bash name=\"install\" env=\"$INSTALL_PATH\"\necho hi\n```\n",
        );
        assert!(validate_env_refs(&canvas(doc)).is_ok());
    }

    #[test]
    fn validate_env_refs_catches_an_undeclared_reference() {
        let doc = concat!(
            "# Root\n\n",
            "## Install\n<!-- meshfox:node id=\"install\" -->\n\n",
            "```bash name=\"install\" env=\"$INSTALL_PATH\"\necho hi\n```\n",
        );
        assert_eq!(
            validate_env_refs(&canvas(doc)).unwrap_err(),
            VarsError::UndeclaredEnvVar(
                "install".to_string(),
                "install".to_string(),
                "INSTALL_PATH".to_string()
            )
        );
    }

    #[test]
    fn resolve_block_env_only_touches_referenced_vars() {
        let decls = vec![
            decl("INSTALL_PATH", Some("/usr/local/bin"), false),
            decl("UNRELATED", None, false),
        ];
        let env_refs = vec![crate::fence::EnvRef {
            local_name: "INSTALL_PATH".to_string(),
            var_name: "INSTALL_PATH".to_string(),
        }];
        let cache = VarCache::in_memory();
        let resolution = resolve_block_env(&env_refs, &decls, &HashMap::new(), &cache, &HashMap::new());
        assert_eq!(
            resolution.env.get("INSTALL_PATH").map(String::as_str),
            Some("/usr/local/bin")
        );
        // UNRELATED is missing (no default) but was never asked for by this
        // block's env=, so it must not show up as something to prompt for.
        assert!(resolution.missing.is_empty());
    }

    #[test]
    fn resolve_block_env_renames_to_the_local_name() {
        let decls = vec![decl("INSTALL_PATH", Some("/opt"), false)];
        let env_refs = vec![crate::fence::EnvRef {
            local_name: "PREFIX".to_string(),
            var_name: "INSTALL_PATH".to_string(),
        }];
        let cache = VarCache::in_memory();
        let resolution = resolve_block_env(&env_refs, &decls, &HashMap::new(), &cache, &HashMap::new());
        assert_eq!(
            resolution.env.get("PREFIX").map(String::as_str),
            Some("/opt")
        );
        assert!(!resolution.env.contains_key("INSTALL_PATH"));
    }

    #[test]
    fn resolve_block_env_reports_missing_by_declared_name() {
        let decls = vec![decl("INSTALL_PATH", None, false)];
        let env_refs = vec![crate::fence::EnvRef {
            local_name: "PREFIX".to_string(),
            var_name: "INSTALL_PATH".to_string(),
        }];
        let cache = VarCache::in_memory();
        let resolution = resolve_block_env(&env_refs, &decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolution.env.is_empty());
        assert_eq!(resolution.missing, vec![decls[0].clone()]);
    }

    #[test]
    fn resolve_block_env_treats_a_required_default_as_missing() {
        let decls = vec![required_decl(
            "INSTALL_PATH",
            Some("/usr/local/bin"),
            false,
            true,
        )];
        let env_refs = vec![crate::fence::EnvRef {
            local_name: "INSTALL_PATH".to_string(),
            var_name: "INSTALL_PATH".to_string(),
        }];
        let cache = VarCache::in_memory();
        let resolution = resolve_block_env(&env_refs, &decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolution.env.is_empty());
        assert_eq!(resolution.missing, vec![decls[0].clone()]);
        assert_eq!(
            resolution.missing[0].default.as_deref(),
            Some("/usr/local/bin")
        );
    }

    #[test]
    fn scans_a_qualified_from_reference() {
        let md = "<!-- meshfox:var name=\"RESOURCE_ID\" from=\"provision/create\" -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert_eq!(
            decls[0].from,
            Some(crate::fence::BlockRef {
                node_id: Some("provision".to_string()),
                block_name: "create".to_string(),
            })
        );
    }

    #[test]
    fn scans_a_bare_from_reference_with_no_node_id_yet() {
        // `scan_var_decls` operates on raw text with no notion of "which
        // node is root" — that normalization only happens in
        // `declared_vars` (see below).
        let md = "<!-- meshfox:var name=\"X\" from=\"create\" -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert_eq!(
            decls[0].from,
            Some(crate::fence::BlockRef {
                node_id: None,
                block_name: "create".to_string(),
            })
        );
    }

    #[test]
    fn empty_from_is_an_error() {
        let md = "<!-- meshfox:var name=\"X\" from=\"\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::EmptyFrom("X".to_string())
        );
    }

    #[test]
    fn from_conflicts_with_default_required_and_secret() {
        let cases = [
            ("<!-- meshfox:var name=\"X\" from=\"a\" default=\"1\" -->\n", "default"),
            ("<!-- meshfox:var name=\"X\" from=\"a\" required -->\n", "required"),
            ("<!-- meshfox:var name=\"X\" from=\"a\" secret -->\n", "secret"),
        ];
        for (md, attr) in cases {
            assert_eq!(
                scan_var_decls(md).unwrap_err(),
                VarsError::FromConflict("X".to_string(), attr)
            );
        }
    }

    #[test]
    fn declared_vars_normalizes_a_bare_from_to_the_root_node_id() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"my-root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" from=\"create\" -->\n",
        );
        let c = canvas(doc);
        let decls = declared_vars(&c).unwrap();
        assert_eq!(
            decls[0].from,
            Some(crate::fence::BlockRef {
                node_id: Some("my-root".to_string()),
                block_name: "create".to_string(),
            })
        );
    }

    #[test]
    fn declared_vars_leaves_an_already_qualified_from_untouched() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"my-root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" from=\"other-node/create\" -->\n",
        );
        let c = canvas(doc);
        let decls = declared_vars(&c).unwrap();
        assert_eq!(decls[0].from.as_ref().unwrap().node_id.as_deref(), Some("other-node"));
    }

    #[test]
    fn resolve_takes_a_from_decl_only_from_computed() {
        let decls = vec![from_decl("X", block_ref("provision", "create"))];
        let cache = VarCache::in_memory();
        let mut computed = HashMap::new();
        computed.insert("X".to_string(), "computed-val".to_string());
        let resolved = resolve(&decls, &HashMap::new(), &cache, &computed);
        assert_eq!(
            resolved.values.get("X").map(String::as_str),
            Some("computed-val")
        );
        assert!(resolved.missing.is_empty());
        assert!(resolved.unresolved_from.is_empty());
    }

    #[test]
    fn resolve_never_lets_overrides_impersonate_a_computed_value() {
        // The whole point of `from=` is that a value can only come from
        // actually running its source block — --set/a submitted form must
        // never be able to shadow that.
        let decls = vec![from_decl("X", block_ref("provision", "create"))];
        let cache = VarCache::in_memory();
        let mut overrides = HashMap::new();
        overrides.insert("X".to_string(), "sneaky-val".to_string());
        let resolved = resolve(&decls, &overrides, &cache, &HashMap::new());
        assert!(resolved.values.is_empty());
        assert_eq!(resolved.unresolved_from, decls);
        assert!(resolved.missing.is_empty());
    }

    #[test]
    fn resolve_reports_an_unresolved_from_separately_from_missing() {
        let decls = vec![
            decl("PLAIN", None, false),
            from_decl("COMPUTED", block_ref("provision", "create")),
        ];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert_eq!(resolved.missing.len(), 1);
        assert_eq!(resolved.missing[0].name, "PLAIN");
        assert_eq!(resolved.unresolved_from.len(), 1);
        assert_eq!(resolved.unresolved_from[0].name, "COMPUTED");
    }

    #[test]
    fn resolve_block_env_surfaces_unresolved_from_for_a_referenced_computed_var() {
        let decls = vec![from_decl("X", block_ref("provision", "create"))];
        let env_refs = vec![crate::fence::EnvRef {
            local_name: "X".to_string(),
            var_name: "X".to_string(),
        }];
        let cache = VarCache::in_memory();
        let resolution =
            resolve_block_env(&env_refs, &decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolution.env.is_empty());
        assert!(resolution.missing.is_empty());
        assert_eq!(resolution.unresolved_from, decls);
    }

    // --- session ---

    fn session_decl(name: &str, required: bool) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type: VarType::String,
            prompt: name.to_string(),
            default: None,
            choices: Vec::new(),
            secret: false,
            required,
            from: None,
            session: true,
            default_var: None,
            choices_var: None,
        }
    }

    #[test]
    fn scans_session_flag() {
        let md = "<!-- meshfox:var name=\"X\" session -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert!(decls[0].session);
    }

    #[test]
    fn session_defaults_to_false() {
        let md = "<!-- meshfox:var name=\"X\" -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert!(!decls[0].session);
    }

    #[test]
    fn session_conflicts_with_from() {
        let md = "<!-- meshfox:var name=\"X\" from=\"a\" session -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::FromConflict("X".to_string(), "session")
        );
    }

    #[test]
    fn resolve_never_reads_a_session_var_from_cache() {
        let decls = vec![session_decl("X", false)];
        let mut cache = VarCache::in_memory();
        cache.set("X", "stale-cached-val").unwrap();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolved.values.is_empty());
        assert_eq!(resolved.missing.len(), 1);
    }

    #[test]
    fn resolve_session_and_required_together_always_lands_in_missing() {
        // Neither an env-var-less default nor a prior cache entry can
        // silently resolve this -- every call needs a fresh answer.
        let decls = vec![{
            let mut d = session_decl("X", true);
            d.default = Some("staging".to_string());
            d
        }];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert!(resolved.values.is_empty());
        assert_eq!(resolved.missing.len(), 1);
        // Still carries the default as the prompt's own pre-filled
        // suggestion, same as a plain `required` var does.
        assert_eq!(resolved.missing[0].default.as_deref(), Some("staging"));
    }

    #[test]
    fn resolve_session_still_prefers_an_override_or_env_value() {
        let decls = vec![session_decl("X", true)];
        let mut overrides = HashMap::new();
        overrides.insert("X".to_string(), "ci-val".to_string());
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &overrides, &cache, &HashMap::new());
        assert_eq!(resolved.values.get("X").map(String::as_str), Some("ci-val"));
        assert!(resolved.missing.is_empty());
    }

    // --- default_var / choices_var ---

    #[test]
    fn scans_default_var_and_choices_var() {
        let md = concat!(
            "<!-- meshfox:var name=\"LIST\" from=\"a/b\" -->\n",
            "<!-- meshfox:var name=\"X\" type=\"select\" choices_var=\"LIST\" -->\n",
            "<!-- meshfox:var name=\"Y\" default_var=\"LIST\" -->\n",
        );
        let decls = scan_var_decls(md).unwrap();
        assert_eq!(decls[1].choices_var.as_deref(), Some("LIST"));
        assert_eq!(decls[2].default_var.as_deref(), Some("LIST"));
    }

    #[test]
    fn default_conflicts_with_default_var() {
        let md = "<!-- meshfox:var name=\"X\" default=\"a\" default_var=\"Y\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::DefaultConflict("X".to_string())
        );
    }

    #[test]
    fn choices_conflicts_with_choices_var() {
        let md =
            "<!-- meshfox:var name=\"X\" type=\"select\" choices=\"a,b\" choices_var=\"Y\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::ChoicesConflict("X".to_string())
        );
    }

    #[test]
    fn choices_var_requires_select_type() {
        let md = "<!-- meshfox:var name=\"X\" choices_var=\"Y\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::ChoicesVarRequiresSelect("X".to_string(), "string".to_string())
        );
    }

    #[test]
    fn choices_var_alone_satisfies_the_select_choices_requirement() {
        let md = "<!-- meshfox:var name=\"X\" type=\"select\" choices_var=\"Y\" -->\n";
        let decls = scan_var_decls(md).unwrap();
        assert_eq!(decls[0].choices_var.as_deref(), Some("Y"));
        assert!(decls[0].choices.is_empty());
    }

    #[test]
    fn default_var_conflicts_with_from() {
        let md = "<!-- meshfox:var name=\"X\" from=\"a\" default_var=\"Y\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::FromConflict("X".to_string(), "default_var")
        );
    }

    #[test]
    fn choices_var_conflicts_with_from() {
        let md = "<!-- meshfox:var name=\"X\" type=\"select\" from=\"a\" choices_var=\"Y\" -->\n";
        assert_eq!(
            scan_var_decls(md).unwrap_err(),
            VarsError::FromConflict("X".to_string(), "choices_var")
        );
    }

    fn default_var_decl(name: &str, default_var: &str) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type: VarType::String,
            prompt: name.to_string(),
            default: None,
            choices: Vec::new(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: Some(default_var.to_string()),
            choices_var: None,
        }
    }

    fn choices_var_decl(name: &str, choices_var: &str) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type: VarType::Select,
            prompt: name.to_string(),
            default: None,
            choices: Vec::new(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: None,
            choices_var: Some(choices_var.to_string()),
        }
    }

    #[test]
    fn resolve_substitutes_a_resolved_default_var_into_missing() {
        let decls = vec![decl("BASE", Some("us-east-1"), false), {
            let mut d = default_var_decl("REGION", "BASE");
            // `required` so REGION doesn't just silently take its
            // (substituted) default the way BASE itself does -- this is
            // what lets the test observe the substituted value landing in
            // `missing`'s own `default` field.
            d.required = true;
            d
        }];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        // BASE resolves silently (has a plain default, not required).
        assert_eq!(
            resolved.values.get("BASE").map(String::as_str),
            Some("us-east-1")
        );
        let region = resolved
            .missing
            .iter()
            .find(|d| d.name == "REGION")
            .unwrap();
        assert_eq!(region.default.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn resolve_substitutes_a_resolved_choices_var_into_missing() {
        let decls = vec![
            decl("REGIONS_LIST", Some("us,eu,ap"), false),
            choices_var_decl("REGION", "REGIONS_LIST"),
        ];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        let region = resolved
            .missing
            .iter()
            .find(|d| d.name == "REGION")
            .unwrap();
        assert_eq!(region.choices, vec!["us", "eu", "ap"]);
    }

    #[test]
    fn resolve_defers_a_decl_whose_choices_var_is_not_yet_resolved() {
        // REGIONS_LIST has no default and isn't overridden -- it lands in
        // `missing` itself, so REGION can't know its choices yet this
        // round and must be deferred (neither resolved nor `missing`).
        let decls = vec![
            decl("REGIONS_LIST", None, false),
            choices_var_decl("REGION", "REGIONS_LIST"),
        ];
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &HashMap::new(), &cache, &HashMap::new());
        assert_eq!(resolved.missing.len(), 1);
        assert_eq!(resolved.missing[0].name, "REGIONS_LIST");
        assert!(!resolved.values.contains_key("REGION"));
        assert!(!resolved.missing.iter().any(|d| d.name == "REGION"));
    }

    #[test]
    fn resolve_an_override_resolves_a_choices_var_decl_even_when_its_reference_never_resolves() {
        // The whole point of an override is that it already IS the final
        // answer -- a client (e.g. a submitted web form, `--set`) that
        // already supplied a value for a `choices_var`-declared variable
        // must not be blocked on the referenced variable resolving at
        // all, let alone requiring it to happen in this same call.
        let decls = vec![
            decl("REGIONS_LIST", None, false), // never resolves in this call
            choices_var_decl("REGION", "REGIONS_LIST"),
        ];
        let mut overrides = HashMap::new();
        overrides.insert("REGION".to_string(), "eu-west-1".to_string());
        let cache = VarCache::in_memory();
        let resolved = resolve(&decls, &overrides, &cache, &HashMap::new());
        assert_eq!(
            resolved.values.get("REGION").map(String::as_str),
            Some("eu-west-1")
        );
        assert!(!resolved.missing.iter().any(|d| d.name == "REGION"));
    }

    #[test]
    fn resolve_block_env_pulls_in_a_choices_var_reference_not_in_env() {
        // The block's own env= only asks for REGION, never REGIONS_LIST
        // directly -- but REGIONS_LIST still has to be resolved so
        // REGION's choices can be materialized.
        let decls = vec![
            decl("REGIONS_LIST", Some("us,eu"), false),
            choices_var_decl("REGION", "REGIONS_LIST"),
        ];
        let env_refs = vec![crate::fence::EnvRef {
            local_name: "REGION".to_string(),
            var_name: "REGION".to_string(),
        }];
        let cache = VarCache::in_memory();
        let resolution =
            resolve_block_env(&env_refs, &decls, &HashMap::new(), &cache, &HashMap::new());
        let region = resolution
            .missing
            .iter()
            .find(|d| d.name == "REGION")
            .unwrap();
        assert_eq!(region.choices, vec!["us", "eu"]);
    }

    #[test]
    fn close_over_var_refs_follows_a_multi_hop_chain() {
        let decls = vec![
            decl("A", Some("a"), false),
            default_var_decl("B", "A"),
            default_var_decl("C", "B"),
        ];
        let closure = close_over_var_refs(&decls, ["C"]);
        assert_eq!(
            closure,
            ["A", "B", "C"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn close_over_var_refs_terminates_on_a_cycle() {
        let decls = vec![default_var_decl("A", "B"), default_var_decl("B", "A")];
        let closure = close_over_var_refs(&decls, ["A"]);
        assert_eq!(closure, ["A", "B"].into_iter().map(String::from).collect());
    }

    #[test]
    fn validate_var_refs_ok_when_every_reference_is_declared() {
        let doc = concat!(
            "# Root\n\n",
            "<!-- meshfox:var name=\"BASE\" default=\"1\" -->\n",
            "<!-- meshfox:var name=\"X\" default_var=\"BASE\" -->\n",
        );
        assert!(validate_var_refs(&canvas(doc)).is_ok());
    }

    #[test]
    fn validate_var_refs_catches_an_undeclared_reference() {
        let doc = concat!(
            "# Root\n\n",
            "<!-- meshfox:var name=\"X\" default_var=\"NOPE\" -->\n",
        );
        assert_eq!(
            validate_var_refs(&canvas(doc)).unwrap_err(),
            VarsError::UndeclaredVarRef(
                "X".to_string(),
                "NOPE".to_string(),
                "default_var"
            )
        );
    }

    #[test]
    fn validate_var_refs_catches_a_cycle() {
        let doc = concat!(
            "# Root\n\n",
            "<!-- meshfox:var name=\"A\" default_var=\"B\" -->\n",
            "<!-- meshfox:var name=\"B\" default_var=\"A\" -->\n",
        );
        assert!(matches!(
            validate_var_refs(&canvas(doc)),
            Err(VarsError::VarRefCycle(_))
        ));
    }
}
