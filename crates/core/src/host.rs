//! The document model's only contact with the outside world: the current
//! time and a source of randomness, both needed to *edit* a canvas (an
//! automatic `createdAt`/`updatedAt` stamp, a fresh random node id) and
//! nothing else.
//!
//! An embedder that has no operating system underneath it (the browser, via
//! WebAssembly) installs its own with [`set_clock`] / [`set_entropy`] once at
//! start-up. On native targets the defaults read the system clock and use
//! std's OS-seeded hasher keys, so existing callers need do nothing. On
//! `wasm32` the clock has no default (guessing would silently write wrong
//! timestamps into a user's file), and the entropy default is only std's
//! fixed-key hasher, so a wasm host should install both.

use std::sync::OnceLock;

/// Nanoseconds since the Unix epoch (UTC).
pub type Clock = fn() -> i128;
/// Any 64 bits that do not repeat between sessions; not cryptographic.
pub type Entropy = fn() -> u64;

static CLOCK: OnceLock<Clock> = OnceLock::new();
static ENTROPY: OnceLock<Entropy> = OnceLock::new();

/// Installs the clock. The first call wins; returns `false` if one was
/// already installed.
pub fn set_clock(clock: Clock) -> bool {
    CLOCK.set(clock).is_ok()
}

/// Installs the entropy source. The first call wins; returns `false` if one
/// was already installed.
pub fn set_entropy(entropy: Entropy) -> bool {
    ENTROPY.set(entropy).is_ok()
}

/// The current instant, in nanoseconds since the Unix epoch.
pub fn now_nanos() -> i128 {
    match CLOCK.get() {
        Some(clock) => clock(),
        None => default_now_nanos(),
    }
}

/// 64 fresh random bits.
pub fn random_u64() -> u64 {
    match ENTROPY.get() {
        Some(entropy) => entropy(),
        None => default_random_u64(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn default_now_nanos() -> i128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

#[cfg(target_arch = "wasm32")]
fn default_now_nanos() -> i128 {
    panic!("meshfox clock not installed: call host::set_clock before editing a canvas");
}

fn default_random_u64() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish()
}
