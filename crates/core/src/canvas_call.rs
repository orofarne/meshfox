//! Declarative invocation of an application owned by another canvas.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CanvasCall {
    #[serde(default)]
    pub canvas: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub set: HashMap<String, String>,
    #[serde(default)]
    pub fresh: bool,
    #[serde(default)]
    pub no_deps: bool,
    #[serde(default)]
    pub skip_fresh: bool,
}
impl CanvasCall {
    pub fn parse(code: &str) -> Result<Self, String> {
        let call: Self = serde_norway::from_str(code).map_err(|e| format!("meshfox block: {e}"))?;
        if call.canvas.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("meshfox block: canvas must not be empty".into());
        }
        if call.run.is_some() == call.stop.is_some() {
            return Err("meshfox block: specify exactly one of run or stop".into());
        }
        if call.stop.is_some()
            && (call.fresh || call.no_deps || call.skip_fresh || !call.set.is_empty())
        {
            return Err("meshfox block: stop does not accept run options".into());
        }
        call.address()?;
        if call.fresh && (call.no_deps || call.skip_fresh) {
            return Err("meshfox block: fresh conflicts with no-deps and skip-fresh".into());
        }
        Ok(call)
    }
    pub fn address(&self) -> Result<(Vec<String>, String), String> {
        let mut args = shlex::split(
            self.run
                .as_deref()
                .or(self.stop.as_deref())
                .unwrap_or_default(),
        )
        .ok_or("meshfox block: invalid quoting in run")?;
        let block = args
            .pop()
            .filter(|s| !s.is_empty())
            .ok_or("meshfox block: run must not be empty")?;
        if args
            .iter()
            .chain(std::iter::once(&block))
            .any(|s| s.starts_with('-'))
        {
            return Err("meshfox block: run contains an option; use YAML option fields".into());
        }
        Ok((args, block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_application_and_options_without_shell_execution() {
        let call = CanvasCall::parse("canvas: ../../README.md\nrun: 'build install[DEST=local]'\nset:\n  INSTALL_PATH: ~/.local/bin\nfresh: true\n").unwrap();
        assert_eq!(
            call.address().unwrap(),
            (vec!["build".into()], "install[DEST=local]".into())
        );
        assert_eq!(call.set["INSTALL_PATH"], "~/.local/bin");
        assert!(call.fresh);
    }
    #[test]
    fn local_run_and_stop() {
        assert!(CanvasCall::parse("run: build").unwrap().canvas.is_none());
        let stop = CanvasCall::parse("stop: 'database server'").unwrap();
        assert_eq!(
            stop.address().unwrap(),
            (vec!["database".into()], "server".into())
        );
        for bad in ["run: x\nstop: y", "{}", "stop: x\nfresh: true"] {
            assert!(CanvasCall::parse(bad).is_err());
        }
    }
    #[test]
    fn rejects_ambiguous_or_unknown_configuration() {
        for code in [
            "canvas: x\nrun: y\nfrehs: true",
            "canvas: ''\nrun: y",
            "canvas: x\nrun: ''",
            "canvas: x\nrun: y\nfresh: true\nskip-fresh: true",
            "canvas: x\nrun: y\nconfirm: true",
            "canvas: x\nrun: '--confirm y'",
            "canvas: x\ncanvas: y\nrun: z",
        ] {
            assert!(CanvasCall::parse(code).is_err(), "{code}");
        }
    }
}
