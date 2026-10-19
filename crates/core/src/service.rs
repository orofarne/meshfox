//! Declarative service lifecycle. Commands are bash scripts, not YAML interpolation.
use serde::Deserialize;
use std::{io, time::Duration};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub start: String,
    pub reuse: Option<String>,
    #[serde(default)]
    pub oneshot: bool,
    pub stop: Option<String>,
    pub monitor: Option<String>,
    pub check: Option<String>,
    pub ready: Option<String>,
    #[serde(default = "default_timeout", deserialize_with = "duration")]
    pub start_timeout: Duration,
    #[serde(default)]
    pub restart_on_fail: bool,
    #[serde(default = "default_interval", deserialize_with = "duration")]
    pub check_interval: Duration,
    #[serde(default = "default_delay", deserialize_with = "duration")]
    pub restart_delay: Duration,
    #[serde(default = "default_timeout", deserialize_with = "duration")]
    pub stop_timeout: Duration,
}
fn default_interval() -> Duration {
    Duration::from_secs(5)
}
fn default_delay() -> Duration {
    Duration::from_secs(2)
}
fn default_timeout() -> Duration {
    Duration::from_secs(30)
}
fn duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let value = String::deserialize(d)?;
    let (number, scale) = if let Some(n) = value.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = value.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = value.strip_suffix('m') {
        (n, 60.0)
    } else {
        return Err(serde::de::Error::custom(
            "duration requires ms, s or m suffix",
        ));
    };
    let seconds = number.parse::<f64>().unwrap_or(f64::NAN) * scale;
    if !seconds.is_finite() || seconds < 0.001 || seconds > 3600.0 {
        return Err(serde::de::Error::custom(
            "duration must be between 1ms and 1h",
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}
impl ServiceConfig {
    pub fn parse(code: &str) -> io::Result<Self> {
        let config: Self = serde_norway::from_str(code)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("@service: {e}")))?;
        for (name, command) in [
            ("start", Some(&config.start)),
            ("reuse", config.reuse.as_ref()),
            ("stop", config.stop.as_ref()),
            ("monitor", config.monitor.as_ref()),
            ("check", config.check.as_ref()),
            ("ready", config.ready.as_ref()),
        ] {
            if command.is_some_and(|s| s.trim().is_empty()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("@service: {name} must not be empty"),
                ));
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_is_strict_and_defaults_are_bounded() {
        let c =
            ServiceConfig::parse("start: sleep 30\ncheck_interval: 250ms\nrestart_on_fail: true")
                .unwrap();
        assert_eq!(c.check_interval, Duration::from_millis(250));
        assert_eq!(c.stop_timeout, Duration::from_secs(30));
        for bad in [
            "stop: true",
            "start: ''",
            "start: ok\nrestart_on_fail: yes",
            "start: ok\nunknown: true",
            "start: ok\nreuse: ''",
            "start: ok\noneshot: yes",
            "start: ok\nrestart_delay: 0s",
            "start: ok\nstop_timeout: infs",
        ] {
            assert!(ServiceConfig::parse(bad).is_err(), "{bad}");
        }
    }
}
