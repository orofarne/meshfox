//! Local signatures attached to the immediately following runnable fence.
use crate::{attrs, fence, vars, Canvas, CodeBlock, VarDecl};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct ArgDecl {
    pub name: String,
    pub var_type: vars::VarType,
    pub choices: Vec<String>,
    pub default: Option<String>,
    pub prompt: String,
    pub required: bool,
}

impl ArgDecl {
    pub fn is_required(&self) -> bool {
        self.required || self.default.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    pub block: CodeBlock,
    pub args: Vec<ArgDecl>,
}

#[derive(Debug, thiserror::Error)]
#[error("argument signature in node {node_id:?}, line {line}: {message}")]
pub struct ArgsError {
    pub node_id: String,
    pub line: usize,
    pub message: String,
}

fn identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn declaration(rest: &str) -> Result<ArgDecl, String> {
    if rest.chars().filter(|c| *c == '"').count() % 2 != 0 {
        return Err("unterminated attribute quote".into());
    }
    let tokens = attrs::tokenize(rest);
    let mut keys = HashSet::new();
    for token in &tokens {
        let key = token.split('=').next().unwrap();
        if !keys.insert(key) {
            return Err(format!("duplicate attribute {key:?}"));
        }
        if !["name", "type", "choices", "default", "prompt", "required"].contains(&key) {
            return Err(format!("unknown meshfox:arg attribute {key:?}"));
        }
    }
    let attributes = attrs::attrs_from_tokens(tokens);
    if let Some(value) = attributes.get("required") {
        if value != "true" && value != "false" {
            return Err("required expects true or false".into());
        }
    }
    let decl: VarDecl = vars::build_var_decl(attributes).map_err(|e| e.to_string())?;
    if !identifier(&decl.name) || decl.name.starts_with("MESHFOX_") {
        return Err(format!("invalid or reserved argument name {:?}", decl.name));
    }
    if decl.var_type != vars::VarType::Select && !decl.choices.is_empty() {
        return Err("choices requires type=select".into());
    }
    if let Some(default) = &decl.default {
        vars::validate_value(&decl, default)?;
    }
    Ok(ArgDecl {
        name: decl.name,
        var_type: decl.var_type,
        choices: decl.choices,
        default: decl.default,
        prompt: decl.prompt,
        required: decl.required,
    })
}

pub fn scan_signatures(node_id: &str, markdown: &str) -> Result<Vec<Signature>, ArgsError> {
    let blocks = fence::scan_runnable_blocks(node_id, markdown);
    let fences = fence::fenced_byte_ranges(markdown);
    let outputs = crate::output::output_byte_ranges(markdown);
    let mut pending = Vec::new();
    let mut signatures = Vec::new();
    let mut offset = 0;
    let mut first_line = 0;
    let error = |line, message| ArgsError {
        node_id: node_id.into(),
        line,
        message,
    };
    for (index, line) in markdown.split_inclusive('\n').enumerate() {
        let line_number = index + 1;
        if let Some(block) = blocks.iter().find(|b| b.span.start == offset) {
            if !pending.is_empty() {
                if block.env.iter().any(|env| {
                    pending
                        .iter()
                        .any(|arg: &ArgDecl| arg.name == env.local_name)
                }) {
                    return Err(error(
                        line_number,
                        "env binding targets an argument name".into(),
                    ));
                }
                signatures.push(Signature {
                    block: block.clone(),
                    args: std::mem::take(&mut pending),
                });
            }
        } else if !fences.iter().any(|r| r.contains(&offset))
            && !outputs.iter().any(|r| r.contains(&offset))
        {
            let trimmed = line.trim();
            if !attrs::is_indented_as_code(line)
                && trimmed.starts_with("<!-- meshfox:arg ")
                && !trimmed.ends_with("-->")
            {
                return Err(error(
                    line_number,
                    "unterminated meshfox:arg declaration".into(),
                ));
            }
            let rest = (!attrs::is_indented_as_code(line))
                .then_some(trimmed)
                .and_then(|s| s.strip_prefix("<!--"))
                .and_then(|s| s.strip_suffix("-->"))
                .and_then(|s| s.trim().strip_prefix("meshfox:arg"))
                .filter(|s| s.is_empty() || s.starts_with(char::is_whitespace));
            if let Some(rest) = rest {
                let arg = declaration(rest.trim()).map_err(|m| error(line_number, m))?;
                if pending.iter().any(|a: &ArgDecl| a.name == arg.name) {
                    return Err(error(
                        line_number,
                        format!("duplicate argument {:?}", arg.name),
                    ));
                }
                if pending.is_empty() {
                    first_line = line_number;
                }
                pending.push(arg);
            } else if !trimmed.is_empty() && !pending.is_empty() {
                return Err(error(
                    first_line,
                    "arguments must immediately precede a runnable fence".into(),
                ));
            }
        } else if !pending.is_empty() {
            return Err(error(
                first_line,
                "arguments must immediately precede a runnable fence".into(),
            ));
        }
        offset += line.len();
    }
    if !pending.is_empty() {
        return Err(error(
            first_line,
            "argument declarations have no runnable fence".into(),
        ));
    }
    Ok(signatures)
}

pub fn validate(canvas: &Canvas) -> Result<(), ArgsError> {
    for node in &canvas.nodes {
        scan_signatures(&node.id, &node.text)?;
    }
    Ok(())
}

/// Separate commas only outside quoted strings and application brackets.
pub fn split_list(text: &str) -> Vec<&str> {
    let mut start = 0;
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escape = false;
    let mut result = Vec::new();
    for (index, c) in text.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if quoted && c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted {
            match c {
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    result.push(text[start..index].trim());
                    start = index + 1;
                }
                _ => (),
            }
        }
    }
    result.push(text[start..].trim());
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    Literal(String),
    Reference(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    pub definition: String,
    pub bindings: std::collections::BTreeMap<String, Binding>,
}

impl Application {
    pub fn parse(address: &str) -> Result<Self, String> {
        let Some((definition, tail)) = address.split_once('[') else {
            if address.is_empty() || address.contains(']') {
                return Err("invalid block address".into());
            }
            return Ok(Self {
                definition: address.into(),
                bindings: Default::default(),
            });
        };
        if definition.is_empty() {
            return Err("missing block name".into());
        }
        let body = tail
            .strip_suffix(']')
            .ok_or("unclosed application brackets")?;
        let mut bindings = std::collections::BTreeMap::new();
        for entry in split_list(body) {
            let (name, value) = entry.split_once('=').ok_or("arguments must be named")?;
            let name = name.trim();
            let value = value.trim();
            if !identifier(name) {
                return Err(format!("invalid argument name {name:?}"));
            }
            let binding = if value.starts_with('"') {
                Binding::Literal(
                    serde_json::from_str::<String>(value)
                        .map_err(|e| format!("invalid argument string: {e}"))?,
                )
            } else if let Some(reference) = value.strip_prefix('$') {
                let reference = if reference.starts_with('{') {
                    reference
                        .strip_prefix('{')
                        .and_then(|s| s.strip_suffix('}'))
                        .ok_or("invalid argument reference")?
                } else {
                    reference
                };
                if !identifier(reference) {
                    return Err("invalid argument reference".into());
                }
                Binding::Reference(reference.into())
            } else {
                if value.is_empty()
                    || value
                        .chars()
                        .any(|c| c.is_whitespace() || ",[]=!$\"\\".contains(c))
                {
                    return Err("invalid bare argument value; quote it as a JSON string".into());
                }
                Binding::Literal(value.into())
            };
            if bindings.insert(name.into(), binding).is_some() {
                return Err(format!("duplicate argument {name:?}"));
            }
        }
        Ok(Self {
            definition: definition.into(),
            bindings,
        })
    }
}

pub fn canonical_name(
    definition: &str,
    values: &std::collections::BTreeMap<String, String>,
) -> String {
    if values.is_empty() {
        return definition.into();
    }
    let entries: Vec<_> = values
        .iter()
        .map(|(name, value)| {
            let literal = if !value.is_empty()
                && !value
                    .chars()
                    .any(|c| c.is_whitespace() || ",[]=!$\"\\".contains(c))
            {
                value.clone()
            } else {
                serde_json::to_string(value).expect("string encoding")
            };
            format!("{name}={literal}")
        })
        .collect();
    format!("{definition}[{}]", entries.join(","))
}

/// Shared literal validation for both signatures' dependency schemas and binding.
pub fn canonical_value(arg: &ArgDecl, value: &str) -> Result<String, String> {
    let mut attrs = std::collections::HashMap::from([
        ("name".into(), arg.name.clone()),
        ("type".into(), arg.var_type.as_str().into()),
    ]);
    if !arg.choices.is_empty() {
        attrs.insert("choices".into(), arg.choices.join(","));
    }
    let declaration = vars::build_var_decl(attrs).map_err(|e| e.to_string())?;
    vars::validate_value(&declaration, value)?;
    Ok(if arg.var_type == vars::VarType::Int {
        value.parse::<i64>().unwrap().to_string()
    } else {
        value.to_owned()
    })
}

/// Names used by dependency bindings, before the caller's local scope is applied.
pub fn dependency_refs(block: &CodeBlock) -> Vec<String> {
    block
        .deps
        .iter()
        .filter_map(|dep| Application::parse(&dep.block_name).ok())
        .flat_map(|app| app.bindings.into_values())
        .filter_map(|binding| match binding {
            Binding::Reference(name) => Some(name),
            Binding::Literal(_) => None,
        })
        .collect()
}

/// Only `${argument}` placeholders are accepted in an env variable name.
/// Values are inserted once, never parsed as another reference.
pub fn env_name_parts(template: &str) -> Result<Vec<(bool, String)>, String> {
    let mut parts: Vec<(bool, String)> = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        parts.push((false, rest[..start].into()));
        let tail = rest[start..]
            .strip_prefix("${")
            .ok_or_else(|| format!("env name template {template:?} expects ${{argument}}"))?;
        let (name, after) = tail
            .split_once('}')
            .ok_or_else(|| format!("unclosed env name template {template:?}"))?;
        if !identifier(name) {
            return Err(format!(
                "invalid argument placeholder in env name {template:?}"
            ));
        }
        parts.push((true, name.into()));
        rest = after;
    }
    parts.push((false, rest.into()));
    if parts.iter().any(|(placeholder, text)| {
        !placeholder && !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }) {
        return Err(format!("invalid literal in env name template {template:?}"));
    }
    Ok(parts)
}

pub fn bind_env_name(
    template: &str,
    arguments: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    if !template.contains('$') {
        return Ok(template.into());
    }
    let mut result = String::new();
    for (placeholder, text) in env_name_parts(template)? {
        if placeholder {
            result.push_str(arguments.get(&text).ok_or_else(|| {
                format!("env name {template:?} references unknown argument {text:?}")
            })?);
        } else {
            result.push_str(&text);
        }
    }
    if !identifier(&result) || result.starts_with("MESHFOX_") {
        return Err(format!(
            "env name {template:?} selected invalid or reserved variable {result:?}"
        ));
    }
    Ok(result)
}

/// Names available to static declaration/scope checks. Open string/int arguments
/// defer the selected declaration to application planning. Finite choices are checked.
pub fn static_env_names(block: &CodeBlock, args: &[ArgDecl]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for env in &block.env {
        if !env.var_name.contains('$') {
            names.push(env.var_name.clone());
            continue;
        }
        if !identifier(&env.local_name) || env.local_name.starts_with("MESHFOX_") {
            return Err("an env name template requires a valid explicit local alias".into());
        }
        let parts = env_name_parts(&env.var_name)?;
        let mut combinations = vec![std::collections::BTreeMap::new()];
        let mut open = false;
        for (placeholder, name) in parts {
            if !placeholder {
                continue;
            }
            let arg = args.iter().find(|a| a.name == name).ok_or_else(|| {
                format!(
                    "env name {:?} references unknown argument {name:?}",
                    env.var_name
                )
            })?;
            if combinations[0].contains_key(&name) {
                continue;
            }
            let choices = match arg.var_type {
                vars::VarType::Select => arg.choices.clone(),
                vars::VarType::Bool => vec!["true".into(), "false".into()],
                _ => {
                    open = true;
                    continue;
                }
            };
            if combinations.len().saturating_mul(choices.len()) > 4096 {
                return Err("env name choices exceed 4096 combinations".into());
            }
            let mut expanded = Vec::new();
            for values in combinations {
                for choice in &choices {
                    let mut selected = values.clone();
                    selected.insert(name.clone(), choice.clone());
                    expanded.push(selected);
                }
            }
            combinations = expanded;
        }
        if !open {
            for values in combinations {
                names.push(bind_env_name(&env.var_name, &values)?);
            }
        }
    }
    Ok(names)
}

/// Bind a definition without changing its source, signature, or canvas variables.
pub fn bind_block(
    node_id: &str,
    markdown: &str,
    address: &str,
    values: &std::collections::HashMap<String, String>,
) -> Result<CodeBlock, String> {
    let app = Application::parse(address)?;
    let mut block = fence::scan_runnable_blocks(node_id, markdown)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(app.definition.as_str()))
        .ok_or_else(|| {
            format!(
                "no runnable block named {:?} in node {node_id:?}",
                app.definition
            )
        })?;
    let signatures = scan_signatures(node_id, markdown).map_err(|e| e.to_string())?;
    let signature = signatures.iter().find(|s| s.block.span == block.span);
    let args = signature.map(|s| s.args.as_slice()).unwrap_or_default();
    static_env_names(&block, args)?;
    for name in app.bindings.keys() {
        if !args.iter().any(|arg| &arg.name == name) {
            return Err(format!(
                "unknown argument {name:?} for {:?}",
                app.definition
            ));
        }
    }
    for arg in args {
        let value = match app.bindings.get(&arg.name) {
            Some(Binding::Literal(value)) => value.clone(),
            Some(Binding::Reference(name)) => values
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unresolved argument reference ${name}"))?,
            None if !arg.is_required() => arg.default.clone().unwrap(),
            None => {
                return Err(format!(
                    "missing required argument {:?} for {:?}",
                    arg.name, app.definition
                ))
            }
        };
        let canonical = canonical_value(arg, &value)?;
        block.arguments.insert(arg.name.clone(), canonical);
    }
    block.name = Some(canonical_name(&app.definition, &block.arguments));
    for env in &mut block.env {
        env.var_name = bind_env_name(&env.var_name, &block.arguments)
            .map_err(|e| format!("{node_id}/{}: {e}", block.name.as_deref().unwrap()))?;
    }
    Ok(block)
}

/// Prepare a manual launch without treating UI suggestions as supplied values.
/// The caller sends confirmed fields back as literal answers; globals remain
/// available only through explicit references in the application address.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgumentStatus {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: &'static str,
    pub prompt: String,
    pub choices: Vec<String>,
    pub required: bool,
    pub resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgumentPreparation {
    pub definition: String,
    pub node_id: String,
    pub fields: Vec<ArgumentStatus>,
    pub block: Option<String>,
}

pub fn prepare_arguments(
    node_id: &str,
    markdown: &str,
    address: &str,
    answers: &std::collections::BTreeMap<String, String>,
    values: &std::collections::HashMap<String, String>,
) -> Result<ArgumentPreparation, String> {
    let mut app = Application::parse(address)?;
    for (name, value) in answers {
        app.bindings
            .insert(name.clone(), Binding::Literal(value.clone()));
    }
    let block = fence::scan_runnable_blocks(node_id, markdown)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(app.definition.as_str()))
        .ok_or_else(|| {
            format!(
                "no runnable block named {:?} in node {node_id:?}",
                app.definition
            )
        })?;
    let signatures = scan_signatures(node_id, markdown).map_err(|e| e.to_string())?;
    let args = signatures
        .iter()
        .find(|s| s.block.span == block.span)
        .map(|s| s.args.as_slice())
        .unwrap_or_default();
    for name in app.bindings.keys() {
        if !args.iter().any(|arg| &arg.name == name) {
            return Err(format!(
                "unknown argument {name:?} for {:?}",
                app.definition
            ));
        }
    }
    let mut supplied = std::collections::BTreeMap::new();
    let mut fields = Vec::new();
    for arg in args {
        let value = match app.bindings.get(&arg.name) {
            Some(Binding::Literal(value)) => Some(value.clone()),
            Some(Binding::Reference(name)) => Some(
                values
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("unresolved argument reference ${name}"))?,
            ),
            None => None,
        };
        let resolved = value.is_some() || !arg.is_required();
        let value = value.map(|v| canonical_value(arg, &v)).transpose()?;
        if let Some(value) = &value {
            supplied.insert(arg.name.clone(), value.clone());
        }
        fields.push(ArgumentStatus {
            name: arg.name.clone(),
            var_type: match arg.var_type {
                vars::VarType::String => "string",
                vars::VarType::Int => "int",
                vars::VarType::Bool => "bool",
                vars::VarType::Select => "select",
            },
            prompt: arg.prompt.clone(),
            choices: arg.choices.clone(),
            required: arg.is_required(),
            resolved,
            value: value.or_else(|| arg.default.clone()),
        });
    }
    let block = if fields.iter().all(|f| f.resolved) {
        Some(
            bind_block(
                node_id,
                markdown,
                &canonical_name(&app.definition, &supplied),
                values,
            )?
            .name
            .unwrap(),
        )
    } else {
        None
    };
    Ok(ArgumentPreparation {
        definition: app.definition,
        node_id: node_id.into(),
        fields,
        block,
    })
}

/// Template selection always names a canvas variable, even when an argument
/// shadows that name locally. Ordinary direct env references keep local scope.
pub fn selected_env_var_names(block: &CodeBlock) -> Vec<String> {
    block
        .attrs
        .get("env")
        .map(|raw| {
            fence::parse_env_list(raw)
                .into_iter()
                .zip(&block.env)
                .filter(|(source, _)| source.var_name.contains("${"))
                .map(|(_, selected)| selected.var_name.clone())
                .collect()
        })
        .unwrap_or_default()
}

pub fn block_env(
    block: &CodeBlock,
    values: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    let mut local_values = values.clone();
    local_values.extend(block.arguments.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mut env = vars::map_block_env(&block.env, &local_values);
    if let Some(raw) = block.attrs.get("env") {
        for (source, selected) in fence::parse_env_list(raw).iter().zip(&block.env) {
            if source.var_name.contains("${") {
                env.remove(&selected.local_name);
                if let Some(value) = values.get(&selected.var_name) {
                    env.insert(selected.local_name.clone(), value.clone());
                }
            }
        }
    }
    env.extend(block.arguments.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

/// Interpreter token substitution in the same local scope as process arguments.
pub fn resolve_interpreter(
    block: &CodeBlock,
    values: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let mut local_values = values.clone();
    local_values.extend(block.arguments.iter().map(|(k, v)| (k.clone(), v.clone())));
    block
        .interpreter
        .as_ref()
        .map(|spec| crate::exec::resolve_interpreter(spec, &local_values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_preparation_keeps_required_suggestions_unconfirmed() {
        let md = "<!-- meshfox:arg name=\"n\" type=\"int\" default=\"2\" required -->\n<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" default=\"en\" -->\n```bash name=\"extract\"\necho ok\n```";
        let prepared = prepare_arguments(
            "root",
            md,
            "extract",
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert!(prepared.block.is_none());
        assert!(!prepared.fields[0].resolved);
        assert_eq!(prepared.fields[0].value.as_deref(), Some("2"));
        assert!(prepared.fields[1].resolved);
        let answers = std::collections::BTreeMap::from([("n".into(), "002".into())]);
        let prepared =
            prepare_arguments("root", md, "extract", &answers, &Default::default()).unwrap();
        assert_eq!(prepared.block.as_deref(), Some("extract[lang=en,n=2]"));
        for address in ["extract[n=bad]", "extract[other=2]", "extract[n=$MISSING]"] {
            assert!(prepare_arguments(
                "root",
                md,
                address,
                &Default::default(),
                &Default::default()
            )
            .is_err());
        }
    }

    #[test]
    fn manual_answers_are_literals_and_override_address_bindings() {
        let md = "<!-- meshfox:arg name=\"q\" -->\n```bash name=\"search\"\necho ok\n```";
        let answers = std::collections::BTreeMap::from([("q".into(), "$NAME, [hy]".into())]);
        let prepared = prepare_arguments(
            "root",
            md,
            "search[q=$MISSING]",
            &answers,
            &Default::default(),
        )
        .unwrap();
        let block = bind_block(
            "root",
            md,
            prepared.block.as_ref().unwrap(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(block.arguments["q"], "$NAME, [hy]");
        let values = std::collections::HashMap::from([("NAME".into(), "hy".into())]);
        let prepared =
            prepare_arguments("root", md, "search[q=$NAME]", &Default::default(), &values).unwrap();
        assert_eq!(prepared.block.as_deref(), Some("search[q=hy]"));
    }

    #[test]
    fn application_identity_and_fingerprints_ignore_binding_order() {
        let canvas = Canvas::from_markdown("# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:arg name=\"lang\" -->\n<!-- meshfox:arg name=\"n\" type=\"int\" default=\"1\" -->\n```bash name=\"x\"\necho \"$lang:$n\"\n```\n").unwrap();
        let first = crate::resolve_run_chain(&canvas, &[], "x[n=01,lang=hy]", false).unwrap();
        let second = crate::resolve_run_chain(&canvas, &[], "x[lang=hy]", false).unwrap();
        assert_eq!(first, second);
        let fingerprint = |name| {
            crate::closure_fingerprint(
                &canvas,
                &crate::BlockAddr::new("root", name),
                &Default::default(),
            )
            .unwrap()
        };
        assert_eq!(fingerprint("x[n=01,lang=hy]"), fingerprint("x[lang=hy]"));
        assert_ne!(fingerprint("x[lang=hy]"), fingerprint("x[lang=en]"));
    }

    #[test]
    fn local_interpreter_and_env_aliases_use_values_without_reinterpreting_them() {
        let md = "<!-- meshfox:arg name=\"tool\" -->\n```text name=\"x\" interpreter=\"$tool -u\" env=\"ALIAS=tool\"\nbody\n```\n";
        let block = bind_block("root", md, r#"x[tool="$GLOBAL"]"#, &Default::default()).unwrap();
        let vars = std::collections::HashMap::from([
            ("GLOBAL".into(), "wrong".into()),
            ("tool".into(), "global-tool".into()),
        ]);
        assert_eq!(block_env(&block, &vars)["ALIAS"], "$GLOBAL");
        assert!(resolve_interpreter(&block, &vars)
            .unwrap()
            .contains("$GLOBAL"));
        assert!(!resolve_interpreter(&block, &vars)
            .unwrap()
            .contains("wrong"));
    }

    #[test]
    fn applications_bind_canonical_values_and_keep_source_unchanged() {
        let md = "<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" -->\n<!-- meshfox:arg name=\"pages\" type=\"int\" default=\"0\" -->\n```bash name=\"extract\" outputs=\"$WORK/pharm_${lang}_${pages}.csv\"\nprintf '%s' \"$lang\"\n```\n";
        let first =
            bind_block("root", md, "extract[pages=01,lang=hy]", &Default::default()).unwrap();
        let second =
            bind_block("root", md, "extract[lang=hy,pages=1]", &Default::default()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.name.as_deref(), Some("extract[lang=hy,pages=1]"));
        assert_eq!(first.attrs["outputs"], "$WORK/pharm_${lang}_${pages}.csv");
        assert!(first.code.contains("$lang"));
        let env = block_env(
            &first,
            &std::collections::HashMap::from([("lang".into(), "global".into())]),
        );
        assert_eq!(env["lang"], "hy");
        assert_eq!(
            bind_block("root", md, "extract[lang=en]", &Default::default())
                .unwrap()
                .arguments["pages"],
            "0"
        );
        for bad in [
            "extract",
            "extract[lang=fr]",
            "extract[hy]",
            "extract[lang=en,lang=hy]",
            "extract[lang=en,unknown=1]",
            "extract[lang=en,pages=no]",
        ] {
            assert!(
                bind_block("root", md, bad, &Default::default()).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn required_defaults_are_ui_only_and_references_are_checked() {
        let md = "<!-- meshfox:arg name=\"n\" type=\"int\" default=\"2\" required -->\n```bash name=\"x\"\necho ok\n```\n";
        assert!(bind_block("root", md, "x", &Default::default()).is_err());
        let values = std::collections::HashMap::from([("N".into(), "003".into())]);
        assert_eq!(
            bind_block("root", md, "x[n=$N]", &values)
                .unwrap()
                .name
                .as_deref(),
            Some("x[n=3]")
        );
        assert!(bind_block("root", md, "x[n=$MISSING]", &values).is_err());
    }

    #[test]
    fn quoted_strings_and_list_separators_are_unambiguous() {
        let address = r#"search[q="ACME, [Inc.] / \\\"",flag=true]"#;
        let app = Application::parse(address).unwrap();
        let values: std::collections::BTreeMap<_, _> = app
            .bindings
            .iter()
            .map(|(k, v)| match v {
                Binding::Literal(v) => (k.clone(), v.clone()),
                _ => panic!(),
            })
            .collect();
        assert_eq!(
            Application::parse(&canonical_name(&app.definition, &values))
                .unwrap()
                .bindings,
            app.bindings
        );
        assert_eq!(
            split_list(&format!("{address},other[n=1]")),
            vec![address, "other[n=1]"]
        );
    }

    #[test]
    fn signatures_are_local_typed_and_keep_ui_order() {
        let md = "<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" -->\n\n<!-- meshfox:arg name=\"pages\" type=\"int\" default=\"0\" required -->\n```bash name=\"extract\"\necho ok\n```\n<!-- meshfox:arg name=\"lang\" default=\"en\" -->\n```bash name=\"other\"\necho ok\n```\n";
        let signatures = scan_signatures("node", md).unwrap();
        assert_eq!(signatures.len(), 2);
        assert_eq!(signatures[0].block.name.as_deref(), Some("extract"));
        assert_eq!(signatures[0].args[0].choices, ["en", "hy"]);
        assert!(signatures[0].args[0].is_required());
        assert!(signatures[0].args[1].is_required());
        assert!(!signatures[1].args[0].is_required());
    }

    #[test]
    fn rejects_invalid_or_unattached_signatures() {
        for md in [
            "<!-- meshfox:arg name=\"lang\" -->\ntext\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"lang\" -->",
            "<!-- meshfox:arg name=\"lang\" -->\n```text\n```",
            "<!-- meshfox:arg name=\"lang\" -->\n<!-- meshfox:arg name=\"lang\" -->\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"n\" type=\"int\" default=\"oops\" -->\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"n\" type=\"select\" -->\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"MESHFOX_X\" -->\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"n\" block=\"x\" -->\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"n\" name=\"n\" -->\n```bash name=\"x\"\n```",
            "<!-- meshfox:arg name=\"n\" -->\n```bash name=\"x\" env=\"n=VAR\"\n```",
        ] {
            assert!(scan_signatures("node", md).is_err(), "accepted {md}");
        }
    }

    #[test]
    fn ignores_examples_and_accepts_implicit_runnable() {
        let md = "````markdown\n<!-- meshfox:arg name=\"bad\" -->\n```bash name=\"example\"\n```\n````\n    <!-- meshfox:arg name=\"indented\" -->\n<!-- meshfox:arg name=\"real\" -->\n```bash\necho ok\n```\n";
        let signatures = scan_signatures("node", md).unwrap();
        assert_eq!(signatures.len(), 1);
        assert_eq!(signatures[0].args[0].name, "real");
        assert_eq!(signatures[0].block.name.as_deref(), Some("node"));
    }
}
