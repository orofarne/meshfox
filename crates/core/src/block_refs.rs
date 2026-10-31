//! Pure helpers about what a fenced block declares and references —
//! `interpreter=` variable words, `env=` selections, `inputs=`/`outputs=`
//! paths, and which fence languages are runnable at all. No filesystem, no
//! process, no environment: this is part of the document model that
//! `fence`, `mdcanvas` and friends need just to *read* a canvas. The
//! execution-side modules (`exec`, `args`, `artifacts`) re-export these so
//! their existing paths keep working.

use crate::fence::{self, CodeBlock};

/// The fence "language" for a `button` shortcut fence (see SPEC.md's
/// "Button fences") — a runnable fence with no real code of its own. Its
/// whole point is its `deps=` chain (already run before it, like any other
/// block's dependencies) plus a prominent UI button in place of the usual
/// small run icon; the fence's own body is just an optional description for
/// a person reading the document, never executed. Always runnable, same as
/// `bash`/`sh` — see `is_supported_lang` — but `crate::deps::validate`
/// rejects `interpreter=`/`cache`/`env=`/`tty` alongside it, since none of
/// those mean anything without a real process of the block's own.
pub const BUTTON_LANG: &str = "button";

/// The fence "language" for a `form` fence (see SPEC.md's "Form fences") —
/// a runnable-addressable fence with no real code of its own, same
/// non-executing family as `button` (`BUTTON_LANG`). Its body is a
/// line-based list of `field var="..."` entries (see `crate::form`)
/// referencing existing `meshfox:var` declarations, plus an optional
/// `send=` caption; submitting it writes values into a session-lifetime
/// override store rather than running anything. Always recognized, same as
/// `bash`/`sh`/`button` — see `is_supported_lang` — but `crate::deps::
/// validate` rejects `interpreter=`/`cache`/`env=`/`tty`/`service`/`deps=`/
/// `autorun` alongside it, since none of those mean anything without a real
/// process of the block's own, and a form has no "done" state for anything
/// else to depend on.
pub const FORM_LANG: &str = "form";

/// Fence languages meshfox actually knows how to execute — what every real
/// spawner (`stream_exec::supports`) checks before treating a fence with no
/// `interpreter=` as runnable at all. Exposed here (not just inlined at
/// each call site) so fence scanning (`crate::fence`) can decide whether a
/// fence is eligible to be treated as runnable — named or implicitly named
/// — before anything ever tries to execute it. This is what keeps an
/// ordinary Markdown doc's non-`bash`/`sh` example fences (a `yaml` config
/// sample, a `json` snippet, ...) from being mistaken for "the" runnable
/// block in a node that has no real meshfox structure of its own.
pub fn is_supported_lang(lang: &str) -> bool {
    matches!(lang, "bash" | "sh" | "meshfox" | BUTTON_LANG | FORM_LANG)
}

/// True if `word` is a bare `$NAME` token in its entirety — a whole shell
/// word, not a prefix/suffix of a longer one (`$PYTHON` matches,
/// `/opt/$PYTHON/bin` and `$PYTHON-3.11` don't) — see
/// `interpreter_var_refs`/`resolve_interpreter` for why only this shape is
/// recognized.
pub fn whole_token_var_name(word: &str) -> Option<&str> {
    let name = word.strip_prefix('$')?;
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name)
}

/// Every declared-variable name an `interpreter=` attribute's raw value
/// references as a whole shell word (`interpreter="$PYTHON -u"` ->
/// `["PYTHON"]`), in first-occurrence order, deduplicated — what a caller
/// needs to have resolved (and, if a `from=`-computed one, already run the
/// source block for — see `crate::deps::visit`) before calling
/// `resolve_interpreter`.
///
/// Unlike `env=`'s entries (always a name reference, so a leading `$` is
/// purely cosmetic there — see `crate::fence::strip_dollar`'s own doc
/// comment), `interpreter=` mixes literal tokens (`python3`, `-u`) with
/// possibly-a-reference ones, so the `$` can't be optional here: a bare
/// `PYTHON` token is a literal program/argument, never treated as a
/// variable reference. Only a *whole* `$NAME` word counts — a reference
/// embedded partway through a token (`/opt/$NAME/bin`) is deliberately not
/// recognized, so there's never any ambiguity about where a name starts
/// and ends. Malformed shell-word syntax (unterminated quotes, same as
/// `split_interpreter` itself would reject) yields no references at all —
/// `resolve_command`'s own error handling is what surfaces that to a user,
/// not this.
pub fn interpreter_var_refs(spec: &str) -> Vec<String> {
    let Some(words) = shlex::split(spec) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    for word in &words {
        if let Some(name) = whole_token_var_name(word) {
            if seen.insert(name.to_string()) {
                names.push(name.to_string());
            }
        }
    }
    names
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
