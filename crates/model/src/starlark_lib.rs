//! Small standard-library namespaces for embedded Starlark (`render` and
//! `constraint` fences): `re`, `math` and `fmt`. Starlark itself has no
//! regular expressions, no trigonometry and no `%.2f`, which a script that
//! generates geometry or parses a label runs into immediately. Everything
//! here is pure and deterministic (no clock, no randomness, no I/O), so the
//! same-input-same-output property of a fence is kept.
use regex::{Regex, RegexBuilder};
use starlark::environment::GlobalsBuilder;
use starlark::values::float::UnpackFloat;

/// Compiled-regex size cap, so a hostile pattern cannot exhaust memory. The
/// `regex` crate matches in time linear in the input, so there is no
/// catastrophic backtracking to bound.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

fn compile(pattern: &str) -> anyhow::Result<Regex> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| anyhow::anyhow!("invalid regular expression: {e}"))
}

/// A match as a list: the whole match first, then each capture group (`None`
/// for a group that did not take part).
fn groups(caps: &regex::Captures<'_>) -> serde_json::Value {
    caps.iter()
        .map(|g| g.map_or(serde_json::Value::Null, |m| m.as_str().into()))
        .collect()
}

/// `re.search`, `re.fullmatch`, `re.findall`, `re.sub`, `re.split`: RE2-style
/// syntax of the Rust `regex` crate (no backreferences or look-around).
#[starlark::starlark_module]
pub(crate) fn re_globals(builder: &mut GlobalsBuilder) {
    /// First match anywhere in `text` as `[whole, group1, ...]`, or `None`.
    fn search(pattern: &str, text: &str) -> anyhow::Result<serde_json::Value> {
        Ok(compile(pattern)?
            .captures(text)
            .map_or(serde_json::Value::Null, |c| groups(&c)))
    }

    /// Like `search`, but the pattern must match all of `text`.
    fn fullmatch(pattern: &str, text: &str) -> anyhow::Result<serde_json::Value> {
        let re = compile(&format!(r"\A(?:{pattern})\z"))?;
        Ok(re
            .captures(text)
            .map_or(serde_json::Value::Null, |c| groups(&c)))
    }

    /// Every non-overlapping match, each as `[whole, group1, ...]`.
    fn findall(pattern: &str, text: &str) -> anyhow::Result<serde_json::Value> {
        Ok(compile(pattern)?
            .captures_iter(text)
            .map(|c| groups(&c))
            .collect())
    }

    /// Replace matches (all of them, or the first `count` when `count > 0`).
    /// `repl` may refer to groups as `$1` or `${name}`; write `$$` for a
    /// literal dollar sign.
    fn sub(
        pattern: &str,
        repl: &str,
        text: &str,
        #[starlark(default = 0)] count: i32,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(count >= 0, "count must not be negative");
        let re = compile(pattern)?;
        Ok(re.replacen(text, count as usize, repl).into_owned())
    }

    /// Split `text` around matches of `pattern`.
    fn split(pattern: &str, text: &str) -> anyhow::Result<Vec<String>> {
        Ok(compile(pattern)?.split(text).map(str::to_owned).collect())
    }
}

fn to_int(value: f64, what: &str) -> anyhow::Result<i32> {
    anyhow::ensure!(
        value.is_finite() && value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX),
        "{what}: result does not fit an integer"
    );
    Ok(value as i32)
}

/// `math.pi`, `math.e` and the usual functions on ints or floats.
#[starlark::starlark_module]
pub(crate) fn math_globals(builder: &mut GlobalsBuilder) {
    const pi: f64 = std::f64::consts::PI;
    const e: f64 = std::f64::consts::E;

    /// Largest integer not above `x`.
    fn floor(x: UnpackFloat) -> anyhow::Result<i32> {
        to_int(x.0.floor(), "math.floor")
    }
    /// Smallest integer not below `x`.
    fn ceil(x: UnpackFloat) -> anyhow::Result<i32> {
        to_int(x.0.ceil(), "math.ceil")
    }
    /// `x` rounded to the nearest integer, halves away from zero.
    fn round(x: UnpackFloat) -> anyhow::Result<i32> {
        to_int(x.0.round(), "math.round")
    }
    fn sqrt(x: UnpackFloat) -> anyhow::Result<f64> {
        anyhow::ensure!(x.0 >= 0.0, "math.sqrt: negative argument");
        Ok(x.0.sqrt())
    }
    fn pow(x: UnpackFloat, y: UnpackFloat) -> anyhow::Result<f64> {
        let r = x.0.powf(y.0);
        anyhow::ensure!(r.is_finite(), "math.pow: result is not finite");
        Ok(r)
    }
    fn sin(x: UnpackFloat) -> anyhow::Result<f64> {
        Ok(x.0.sin())
    }
    fn cos(x: UnpackFloat) -> anyhow::Result<f64> {
        Ok(x.0.cos())
    }
    fn tan(x: UnpackFloat) -> anyhow::Result<f64> {
        Ok(x.0.tan())
    }
    fn atan2(y: UnpackFloat, x: UnpackFloat) -> anyhow::Result<f64> {
        Ok(y.0.atan2(x.0))
    }
    fn radians(x: UnpackFloat) -> anyhow::Result<f64> {
        Ok(x.0.to_radians())
    }
    fn degrees(x: UnpackFloat) -> anyhow::Result<f64> {
        Ok(x.0.to_degrees())
    }
}

/// Number formatting. Starlark's own `%` has no precision (`"%.1f"` is an
/// error), so SVG coordinates and the like go through here.
#[starlark::starlark_module]
pub(crate) fn fmt_globals(builder: &mut GlobalsBuilder) {
    /// `x` with exactly `precision` digits after the point (0 to 20), like
    /// C's `%.Nf`.
    fn float(x: UnpackFloat, #[starlark(default = 2)] precision: i32) -> anyhow::Result<String> {
        anyhow::ensure!(
            (0..=20).contains(&precision),
            "fmt.float: precision must be between 0 and 20"
        );
        anyhow::ensure!(x.0.is_finite(), "fmt.float: not a finite number");
        Ok(format!("{:.*}", precision as usize, x.0))
    }

    /// Like `float`, then trailing zeros (and a bare trailing point) removed:
    /// `fmt.compact(2.50, 3)` is `"2.5"`, `fmt.compact(4.0)` is `"4"`.
    fn compact(x: UnpackFloat, #[starlark(default = 2)] precision: i32) -> anyhow::Result<String> {
        anyhow::ensure!(
            (0..=20).contains(&precision),
            "fmt.compact: precision must be between 0 and 20"
        );
        anyhow::ensure!(x.0.is_finite(), "fmt.compact: not a finite number");
        let s = format!("{:.*}", precision as usize, x.0);
        let s = if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        };
        Ok(if s == "-0" { "0".to_owned() } else { s })
    }

    /// `text` left-padded with `fill` (default a space) to `width` characters.
    fn pad_left(
        text: &str,
        width: i32,
        #[starlark(default = " ")] fill: &str,
    ) -> anyhow::Result<String> {
        pad(text, width, fill, true)
    }

    /// `text` right-padded with `fill` (default a space) to `width` characters.
    fn pad_right(
        text: &str,
        width: i32,
        #[starlark(default = " ")] fill: &str,
    ) -> anyhow::Result<String> {
        pad(text, width, fill, false)
    }
}

fn pad(text: &str, width: i32, fill: &str, left: bool) -> anyhow::Result<String> {
    let mut fill_chars = fill.chars();
    let (Some(fill), None) = (fill_chars.next(), fill_chars.next()) else {
        anyhow::bail!("fill must be exactly one character");
    };
    anyhow::ensure!(
        (0..=4096).contains(&width),
        "width must be between 0 and 4096"
    );
    let missing = (width as usize).saturating_sub(text.chars().count());
    let padding: String = std::iter::repeat_n(fill, missing).collect();
    Ok(if left {
        format!("{padding}{text}")
    } else {
        format!("{text}{padding}")
    })
}

/// Register `re`, `math`, `fmt` and `json` (the latter is starlark's own
/// extension, enabled by the caller through `LibraryExtension::Json`).
pub(crate) fn register(globals: &mut GlobalsBuilder) {
    globals.namespace("re", re_globals);
    globals.namespace("math", math_globals);
    globals.namespace("fmt", fmt_globals);
}
