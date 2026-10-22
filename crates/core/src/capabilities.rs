//! Restrictions a core (worker) is started with, on top of whatever the
//! canvas's filesystem already imposes.
//!
//! Two independent switches, fixed for a worker's whole lifetime:
//!
//! - `read_only` — the worker is served as if the canvas were unwritable
//!   (see `worker_lock` and SPEC.md's "Read-only canvases"): no lock, no
//!   port file, in-memory session state, every edit refused. Unlike the
//!   filesystem probe this can be asked for on a writable canvas, which is
//!   what lets any number of cores serve one canvas side by side.
//! - `no_exec` — the worker refuses to run anything: blocks, services,
//!   interactive sessions, debug shells.
//!
//! A restriction is a property of the *process* that serves the canvas, so it
//! is set once at startup ([`set_requested`], from `--read-only`/`--no-exec`
//! or the `MESHFOX_READ_ONLY`/`MESHFOX_NO_EXEC` environment) and every
//! worker the process starts reads it back with [`requested`]. It can only
//! ever be tightened, never loosened again.

use std::sync::atomic::{AtomicU8, Ordering};

const ENV_READ_ONLY: &str = "MESHFOX_READ_ONLY";
const ENV_NO_EXEC: &str = "MESHFOX_NO_EXEC";

const READ_ONLY_BIT: u8 = 1;
const NO_EXEC_BIT: u8 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub read_only: bool,
    pub no_exec: bool,
}

impl Capabilities {
    pub const UNRESTRICTED: Capabilities = Capabilities {
        read_only: false,
        no_exec: false,
    };

    pub const READ_ONLY: Capabilities = Capabilities {
        read_only: true,
        no_exec: false,
    };

    pub const NO_EXEC: Capabilities = Capabilities {
        read_only: false,
        no_exec: true,
    };

    /// The restrictions named by `MESHFOX_READ_ONLY`/`MESHFOX_NO_EXEC`. A
    /// variable counts as set unless it is empty, `0`, `false`, `no` or `off`.
    pub fn from_env() -> Self {
        Capabilities {
            read_only: env_flag(ENV_READ_ONLY),
            no_exec: env_flag(ENV_NO_EXEC),
        }
    }

    /// Both sets of restrictions at once.
    pub fn tighten(self, other: Capabilities) -> Self {
        Capabilities {
            read_only: self.read_only || other.read_only,
            no_exec: self.no_exec || other.no_exec,
        }
    }

    pub fn is_restricted(self) -> bool {
        self.read_only || self.no_exec
    }

    /// `--read-only` / `--no-exec`, for passing the same restrictions on to a
    /// worker process this one spawns.
    pub fn args(self) -> Vec<&'static str> {
        let mut args = Vec::new();
        if self.read_only {
            args.push("--read-only");
        }
        if self.no_exec {
            args.push("--no-exec");
        }
        args
    }

    fn bits(self) -> u8 {
        (u8::from(self.read_only) * READ_ONLY_BIT) | (u8::from(self.no_exec) * NO_EXEC_BIT)
    }

    fn from_bits(bits: u8) -> Self {
        Capabilities {
            read_only: bits & READ_ONLY_BIT != 0,
            no_exec: bits & NO_EXEC_BIT != 0,
        }
    }
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

/// The last line of defence for `--no-exec`: every place that starts a
/// process on behalf of a canvas calls this first, so a request path nobody
/// thought to gate still cannot run anything.
pub fn require_exec() -> std::io::Result<()> {
    if requested().no_exec {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "running is disabled: this core was started with --no-exec",
        ));
    }
    Ok(())
}

static REQUESTED: AtomicU8 = AtomicU8::new(0);

/// What this process was asked to enforce — the environment plus whatever
/// [`set_requested`] added.
pub fn requested() -> Capabilities {
    Capabilities::from_env().tighten(Capabilities::from_bits(REQUESTED.load(Ordering::Relaxed)))
}

/// Adds `caps` to this process's restrictions. Restrictions only accumulate:
/// there is no way to lift one again.
pub fn set_requested(caps: Capabilities) {
    REQUESTED.fetch_or(caps.bits(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tighten_keeps_every_restriction_of_both_sides() {
        let a = Capabilities {
            read_only: true,
            no_exec: false,
        };
        let b = Capabilities {
            read_only: false,
            no_exec: true,
        };
        assert_eq!(
            a.tighten(b),
            Capabilities {
                read_only: true,
                no_exec: true
            }
        );
        assert_eq!(a.tighten(Capabilities::UNRESTRICTED), a);
    }

    #[test]
    fn bits_round_trip() {
        for read_only in [false, true] {
            for no_exec in [false, true] {
                let caps = Capabilities { read_only, no_exec };
                assert_eq!(Capabilities::from_bits(caps.bits()), caps);
            }
        }
    }

    #[test]
    fn args_name_exactly_the_active_restrictions() {
        assert!(Capabilities::UNRESTRICTED.args().is_empty());
        assert_eq!(
            Capabilities {
                read_only: true,
                no_exec: true
            }
            .args(),
            ["--read-only", "--no-exec"]
        );
    }
}
