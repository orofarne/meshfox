//! Per-invocation context must not leak from a parent Meshfox execution.
//! Ordinary environment and coordinator/debug settings remain inherited.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;

pub fn is_run_context(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    matches!(
        name.as_ref(),
        "MESHFOX_ENV_NAMES" | "MESHFOX_VARS_OUT" | "MESHFOX_BLOCK_LANG" | "MESHFOX_VENV_DIR"
    ) || name.starts_with("MESHFOX_CONFIG_")
}

pub struct RunEnvironment {
    pub remove: Vec<OsString>,
    pub values: Vec<(OsString, OsString)>,
}

/// Config overrides < block locals < fresh builtin context. Reserved context
/// cannot be restored by process_env; VARS_OUT is supplied by the runner.
pub fn prepare<I, K, V>(
    cwd: Option<&Path>,
    locals: I,
    extras: &[(String, String)],
) -> RunEnvironment
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    prepare_with(
        std::env::vars_os(),
        crate::config::env_overrides(cwd.unwrap_or_else(|| Path::new("."))),
        locals,
        extras,
        default_utf8_locale,
    )
}

fn prepare_with<I, K, V>(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    overrides: impl IntoIterator<Item = (String, String)>,
    locals: I,
    extras: &[(String, String)],
    fallback_locale: impl FnOnce() -> Option<OsString>,
) -> RunEnvironment
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let inherited: BTreeMap<_, _> = inherited.into_iter().collect();
    let mut remove: Vec<_> = inherited
        .iter()
        .map(|(k, _)| k.clone())
        .filter(|k| is_run_context(k))
        .collect();
    for name in [
        "MESHFOX_ENV_NAMES",
        "MESHFOX_VARS_OUT",
        "MESHFOX_BLOCK_LANG",
        "MESHFOX_VENV_DIR",
    ] {
        remove.push(name.into());
    }
    remove.sort();
    remove.dedup();
    let mut values: BTreeMap<OsString, OsString> = overrides
        .into_iter()
        .filter(|(k, _)| !is_run_context(OsStr::new(k)))
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    let mut names = Vec::new();
    for (name, value) in locals {
        let name = name.as_ref();
        if !is_run_context(name) {
            names.push(name.to_string_lossy().into_owned());
        } else if name != OsStr::new(crate::VARS_OUT_ENV) {
            continue;
        }
        values.insert(name.to_owned(), value.as_ref().to_owned());
    }
    names.sort();
    names.dedup();
    values.insert("MESHFOX_ENV_NAMES".into(), names.join(",").into());
    values.extend(extras.iter().map(|(k, v)| (k.into(), v.into())));
    let lang = values
        .get(OsStr::new("LANG"))
        .or_else(|| inherited.get(OsStr::new("LANG")));
    if lang.is_none_or(|value| value.is_empty()) {
        if let Some(locale) = fallback_locale() {
            values.insert("LANG".into(), locale);
        }
    }
    RunEnvironment {
        remove,
        values: values.into_iter().collect(),
    }
}

/// Probe without changing the worker's process-wide locale. Cache only the
/// selected name; locale objects are released immediately.
fn default_utf8_locale() -> Option<OsString> {
    static LOCALE: std::sync::OnceLock<Option<OsString>> = std::sync::OnceLock::new();
    LOCALE
        .get_or_init(|| select_utf8_locale(locale_available).map(OsString::from))
        .clone()
}

fn select_utf8_locale(mut available: impl FnMut(&str) -> bool) -> Option<&'static str> {
    ["C.UTF-8", "en_US.UTF-8"]
        .into_iter()
        .find(|name| available(name))
}

// The `libc` crate has no `LC_ALL_MASK` for musl; this is musl's value.
#[cfg(target_env = "musl")]
const LC_ALL_MASK: libc::c_int = 0x7fff_ffff;
#[cfg(all(unix, not(target_env = "musl")))]
const LC_ALL_MASK: libc::c_int = libc::LC_ALL_MASK;

#[cfg(unix)]
fn locale_available(name: &str) -> bool {
    let name = std::ffi::CString::new(name).expect("locale name has no NUL");
    // SAFETY: name is NUL-terminated, a null base creates an independent
    // object, and only a successfully allocated object is freed.
    unsafe {
        let locale = libc::newlocale(LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut());
        if locale.is_null() {
            false
        } else {
            libc::freelocale(locale);
            true
        }
    }
}

#[cfg(not(unix))]
fn locale_available(_name: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_run_context_and_preserves_precedence() {
        let inherited = [
            "HOME",
            "MESHFOX_SERVER_SOCKET",
            "MESHFOX_DEBUG_IDLE_SECS",
            "MESHFOX_CONFIG_OLD_KEY",
            "MESHFOX_ENV_NAMES",
            "MESHFOX_VARS_OUT",
            "MESHFOX_VENV_DIR",
        ]
        .map(|s| (s.into(), "parent".into()));
        let env = prepare_with(
            inherited,
            [
                ("TOPIC".into(), "config".into()),
                ("MESHFOX_ENV_NAMES".into(), "SECRET".into()),
            ],
            [("TOPIC", "local"), (crate::VARS_OUT_ENV, "fresh-path")],
            &[("MESHFOX_CONFIG_CURRENT_KEY".into(), "fresh".into())],
            || None,
        );
        for name in ["HOME", "MESHFOX_SERVER_SOCKET", "MESHFOX_DEBUG_IDLE_SECS"] {
            assert!(!env.remove.contains(&OsString::from(name)));
        }
        assert!(env
            .remove
            .contains(&OsString::from("MESHFOX_CONFIG_OLD_KEY")));
        let values: BTreeMap<_, _> = env.values.into_iter().collect();
        assert_eq!(values[OsStr::new("TOPIC")], "local");
        assert_eq!(values[OsStr::new("MESHFOX_ENV_NAMES")], "TOPIC");
        assert_eq!(values[OsStr::new(crate::VARS_OUT_ENV)], "fresh-path");
        assert!(!values.contains_key(OsStr::new("MESHFOX_VENV_DIR")));
        assert!(!values.contains_key(OsStr::new("MESHFOX_CONFIG_OLD_KEY")));
    }

    #[test]
    fn utf8_locale_selection_uses_available_candidates_in_order() {
        let mut probed = Vec::new();
        assert_eq!(
            select_utf8_locale(|name| {
                probed.push(name.to_owned());
                true
            }),
            Some("C.UTF-8")
        );
        assert_eq!(probed, ["C.UTF-8"]);
        assert_eq!(
            select_utf8_locale(|name| name == "en_US.UTF-8"),
            Some("en_US.UTF-8")
        );
        assert_eq!(select_utf8_locale(|_| false), None);
    }

    #[test]
    fn lang_fallback_respects_effective_environment_and_locale_overrides() {
        for (inherited, config, local, expected) in [
            (None, None, None, "C.UTF-8"),
            (Some(""), None, None, "C.UTF-8"),
            (Some("de_DE.UTF-8"), None, None, "de_DE.UTF-8"),
            (Some("C"), None, None, "C"),
            (Some("de_DE.UTF-8"), Some(""), None, "C.UTF-8"),
            (None, Some("fr_FR.UTF-8"), None, "fr_FR.UTF-8"),
            (None, Some("fr_FR.UTF-8"), Some("C"), "C"),
        ] {
            let inherited = inherited.map(|v| (OsString::from("LANG"), OsString::from(v)));
            let config = config.map(|v| ("LANG".to_owned(), v.to_owned()));
            let mut locals = vec![("LC_ALL", "C"), ("LC_CTYPE", "en_US.UTF-8")];
            if let Some(local) = local {
                locals.push(("LANG", local));
            }
            let env = prepare_with(inherited.clone(), config, locals, &[], || {
                Some("C.UTF-8".into())
            });
            let mut effective: BTreeMap<_, _> = inherited.into_iter().collect();
            effective.extend(env.values);
            assert_eq!(effective[OsStr::new("LANG")], expected);
            assert_eq!(effective[OsStr::new("LC_ALL")], "C");
            assert_eq!(effective[OsStr::new("LC_CTYPE")], "en_US.UTF-8");
        }
    }

    #[cfg(unix)]
    #[test]
    fn selected_locale_is_accepted_by_a_child_process() {
        assert!(locale_available("C"));
        // musl's `newlocale` accepts any name: UTF-8 is its only multibyte mode.
        #[cfg(not(target_env = "musl"))]
        assert!(!locale_available("meshfox_nonexistent_locale"));
        let Some(locale) = default_utf8_locale() else {
            return;
        };
        let env = prepare_with([], [], [] as [(&str, &str); 0], &[], || Some(locale));
        let output = std::process::Command::new("locale")
            .arg("charmap")
            .env_clear()
            .envs(env.values)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stderr.is_empty(), "{:?}", output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_uppercase(),
            "UTF-8"
        );
    }

    #[test]
    fn empty_locals_always_reset_names() {
        let env = prepare_with([], [], [] as [(&str, &str); 0], &[], || None);
        assert_eq!(env.values, vec![("MESHFOX_ENV_NAMES".into(), "".into())]);
    }
}
