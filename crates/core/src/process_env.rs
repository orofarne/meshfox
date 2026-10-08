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
    )
}

fn prepare_with<I, K, V>(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    overrides: impl IntoIterator<Item = (String, String)>,
    locals: I,
    extras: &[(String, String)],
) -> RunEnvironment
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut remove: Vec<_> = inherited
        .into_iter()
        .map(|(k, _)| k)
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
    RunEnvironment {
        remove,
        values: values.into_iter().collect(),
    }
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
        );
        for name in ["HOME", "MESHFOX_SERVER_SOCKET", "MESHFOX_DEBUG_IDLE_SECS"] {
            assert!(!env.remove.contains(&OsString::from(name)));
        }
        assert!(
            env.remove
                .contains(&OsString::from("MESHFOX_CONFIG_OLD_KEY"))
        );
        let values: BTreeMap<_, _> = env.values.into_iter().collect();
        assert_eq!(values[OsStr::new("TOPIC")], "local");
        assert_eq!(values[OsStr::new("MESHFOX_ENV_NAMES")], "TOPIC");
        assert_eq!(values[OsStr::new(crate::VARS_OUT_ENV)], "fresh-path");
        assert!(!values.contains_key(OsStr::new("MESHFOX_VENV_DIR")));
        assert!(!values.contains_key(OsStr::new("MESHFOX_CONFIG_OLD_KEY")));
    }

    #[test]
    fn empty_locals_always_reset_names() {
        let env = prepare_with([], [], [] as [(&str, &str); 0], &[]);
        assert_eq!(env.values, vec![("MESHFOX_ENV_NAMES".into(), "".into())]);
    }
}
