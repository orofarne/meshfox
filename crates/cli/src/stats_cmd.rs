//! Presentation of worker-computed statistics; no canvas or database reads here.
use crate::{coordinator, logs_cmd::style, worker_client};
use meshfox_server::run_stats::{BlockStats, DurationStats, Outcomes, Report};
use std::path::PathBuf;

pub fn since_arg(value: &str) -> Result<String, String> {
    meshfox_server::run_stats::parse_since(value)
        .map(|_| value.to_string())
        .ok_or_else(|| "expected an RFC3339 timestamp or a duration such as 7d, 2w, 1h".into())
}

pub async fn run(
    path: PathBuf,
    selector: Vec<String>,
    chain: bool,
    since: Option<String>,
    json: bool,
) -> Result<(), String> {
    tokio::select! {
        result=read(path,selector,chain,since,json)=>result,
        signal=tokio::signal::ctrl_c()=>signal.map_err(|e|e.to_string()),
    }
}
async fn read(
    path: PathBuf,
    selector: Vec<String>,
    chain: bool,
    since: Option<String>,
    json: bool,
) -> Result<(), String> {
    let port = coordinator::get_or_spawn(&path)
        .await
        .map_err(|e| e.to_string())?;
    let mut params = vec![
        (
            "selector".into(),
            serde_json::to_string(&selector).map_err(|e| e.to_string())?,
        ),
        ("chain".into(), chain.to_string()),
    ];
    if let Some(since) = since {
        params.push(("since".into(), since));
    }
    let report = worker_client::stats(port, &params).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        );
    } else {
        print_report(&report);
    }
    Ok(())
}
fn ms(value: Option<f64>) -> String {
    value.map_or_else(
        || "n/a".into(),
        |value| meshfox_core::format_duration_ms(value.round() as u64),
    )
}
fn label(name: &str) -> String {
    style(format!("{name}:"), "1")
}

/// `println!` into a buffer, so the same text can go to a terminal or the TUI.
macro_rules! outln {
    ($out:expr) => {
        $out.push('\n')
    };
    ($out:expr, $($arg:tt)*) => {{
        use std::fmt::Write as _;
        let _ = writeln!($out, $($arg)*);
    }};
}

fn duration(out: &mut String, stats: &DurationStats) {
    outln!(
        out,
        "{} median {} · p95 {} · {} {} (completed successes/failures)",
        label("Duration"),
        ms(stats.median_ms),
        ms(stats.p95_ms.map(|v| v as f64)),
        stats.samples,
        if stats.samples == 1 {
            "sample"
        } else {
            "samples"
        }
    );
}
fn outcomes(out: &mut String, name: &str, counts: &Outcomes) {
    let parts = [
        (counts.succeeded, "succeeded", "32"),
        (counts.failed, "failed", "31"),
        (counts.killed, "killed", "31"),
        (counts.interrupted, "interrupted", "33"),
        (counts.running, "running", "33"),
        (counts.unknown, "unknown", "33"),
    ]
    .into_iter()
    .map(|(n, name, color)| style(format!("{n} {name}"), if n == 0 { "2" } else { color }))
    .collect::<Vec<_>>();
    outln!(
        out,
        "{} {} · {}",
        label(name),
        counts.total(),
        parts.join(" · ")
    );
    match counts.success_rate() {
        Some(rate) => outln!(
            out,
            "{} {rate:.1}% ({}/{} succeeded+failed)",
            label("Success"),
            counts.succeeded,
            counts.succeeded + counts.failed
        ),
        None => outln!(
            out,
            "{} n/a (no known succeeded/failed outcomes)",
            label("Success")
        ),
    }
}
fn skips(out: &mut String, blocks: &[BlockStats]) {
    let skipped: u64 = blocks.iter().map(|b| b.skipped).sum();
    let attempts: u64 = blocks.iter().map(|b| b.outcomes.total()).sum();
    let rate = if skipped + attempts == 0 {
        "n/a".into()
    } else {
        format!(
            "{:.1}%",
            100.0 * skipped as f64 / (skipped + attempts) as f64
        )
    };
    outln!(
        out,
        "{} {skipped} · {rate} of attempted/skipped steps · {} {} · {} {}",
        label("Skipped"),
        label("not started"),
        blocks.iter().map(|b| b.not_started).sum::<u64>(),
        label("pending"),
        blocks.iter().map(|b| b.pending).sum::<u64>()
    );
    let unobserved: u64 = blocks.iter().map(|b| b.unobserved).sum();
    if unobserved > 0 {
        outln!(
            out,
            "{} {unobserved} (incomplete transcripts; not counted as attempts)",
            label("Unobserved planned steps")
        );
    }
    let services_started: u64 = blocks.iter().map(|b| b.services_started).sum();
    let services_reused: u64 = blocks.iter().map(|b| b.services_reused).sum();
    if services_started + services_reused > 0 {
        outln!(out, "{} {services_started} started · {services_reused} reused (outside execution success rates)", label("Services"));
    }
    let avoided = blocks
        .iter()
        .fold(0u64, |sum, b| sum.saturating_add(b.estimated_avoided_ms));
    if skipped > 0 {
        outln!(
            out,
            "{} {} ({} of {skipped} skipped source durations; not elapsed time saved)",
            label("Estimated work avoided"),
            meshfox_core::format_duration_ms(avoided),
            blocks
                .iter()
                .map(|b| b.estimated_avoided_samples)
                .sum::<u64>()
        );
    }
}
fn print_report(report: &Report) {
    print!("{}", render_report(report));
}

/// The report as the CLI prints it (ANSI-styled when colour is on); the
/// TUI's statistics view shows the same text.
pub fn render_report(report: &Report) -> String {
    let mut buf = String::new();
    let out = &mut buf;
    write_report(out, report);
    buf
}

fn write_report(out: &mut String, report: &Report) {
    let selected = report.address.as_ref().map_or_else(
        || "canvas".into(),
        |a| format!("{}::{} · {}", a.node_id, a.block, report.scope),
    );
    outln!(out, "{}", style(format!("Statistics · {selected}"), "1;36"));
    let c = &report.coverage;
    outln!(
        out,
        "{} {} chains · {} — {}",
        label("Retained history"),
        c.retained_chains,
        c.retained_from.as_deref().unwrap_or("n/a"),
        c.retained_to.as_deref().unwrap_or("n/a")
    );
    if let Some(since) = &c.requested_since {
        outln!(
            out,
            "{} {since} · only retained journals are available",
            label("Requested since")
        );
    }
    outln!(
        out,
        "{} {} chains · {} — {}",
        label("Selected"),
        c.matching_chains,
        c.from.as_deref().unwrap_or("n/a"),
        c.to.as_deref().unwrap_or("n/a")
    );
    outln!(
        out,
        "{}",
        style(
            "Coverage is limited to retained journals; no all-time aggregates.",
            "2"
        )
    );
    if c.incomplete_chains > 0 {
        outln!(
            out,
            "{}",
            style(
                format!(
                    "{} incomplete transcripts: missing step outcomes may be unknown.",
                    c.incomplete_chains
                ),
                "33"
            )
        );
    }
    if c.truncated_chains > 0 {
        outln!(
            out,
            "{}",
            style(
                format!(
                    "{} transcripts have truncated output; retained metadata still contributes.",
                    c.truncated_chains
                ),
                "2"
            )
        );
    }
    if c.matching_chains == 0 {
        outln!(out, "\nNo saved invocations match this selection.");
        return;
    }
    outln!(out);
    if report.scope == "block" {
        if let Some(block) = report.blocks.first() {
            outcomes(out, "Block attempts", &block.outcomes);
            duration(out, &block.duration);
            skips(out, &report.blocks);
            recent(out, block);
        }
        return;
    }
    outcomes(out, "Chains", &report.chains);
    duration(out, &report.chain_duration);
    skips(out, &report.blocks);
    let mut slow: Vec<_> = report
        .blocks
        .iter()
        .filter(|b| b.duration.samples > 0)
        .collect();
    slow.sort_by(|a, b| {
        b.duration
            .total_ms
            .cmp(&a.duration.total_ms)
            .then(a.node_id.cmp(&b.node_id))
            .then(a.block.cmp(&b.block))
    });
    if !slow.is_empty() {
        outln!(
            out,
            "\n{}",
            style(
                "Slowest blocks · sum of completed step time (separate from chain time)",
                "1;36"
            )
        );
        for b in slow.iter().take(5) {
            outln!(
                out,
                "  {}::{} · total {} · median {} · p95 {} · {} samples",
                b.node_id,
                b.block,
                meshfox_core::format_duration_ms(b.duration.total_ms),
                ms(b.duration.median_ms),
                ms(b.duration.p95_ms.map(|v| v as f64)),
                b.duration.samples
            );
        }
    }
    let mut failures: Vec<_> = report
        .blocks
        .iter()
        .filter(|b| b.outcomes.failed > 0)
        .collect();
    failures.sort_by(|a, b| {
        b.outcomes
            .failed
            .cmp(&a.outcomes.failed)
            .then(a.node_id.cmp(&b.node_id))
            .then(a.block.cmp(&b.block))
    });
    if !failures.is_empty() {
        outln!(
            out,
            "\n{}",
            style("Most failures · known succeeded/failed attempts", "1;31")
        );
        for b in failures.iter().take(5) {
            outln!(
                out,
                "  {}::{} · {}/{} failed",
                b.node_id,
                b.block,
                b.outcomes.failed,
                b.outcomes.succeeded + b.outcomes.failed
            );
        }
    }
}
fn recent(out: &mut String, block: &BlockStats) {
    if block.recent.is_empty() {
        return;
    }
    let items: Vec<_> = block
        .recent
        .iter()
        .map(|r| match r.status.as_str() {
            "succeeded" => style("✓", "32"),
            "failed" => style("✗", "31"),
            "skipped" => style("↷", "2"),
            "killed" => style("×", "31"),
            "interrupted" => style("!", "33"),
            "running" => style("…", "33"),
            "not-started" => style("–", "2"),
            "service-started" => style("+", "32"),
            "service-reused" => style("=", "32"),
            _ => "?".into(),
        })
        .collect();
    outln!(
        out,
        "{} {}",
        label(&format!("Recent {} (oldest → newest)", items.len())),
        items.join(" ")
    );
    let mut legend = Vec::new();
    for entry in &block.recent {
        let name = match entry.status.as_str() {
            "succeeded" => "✓ succeeded",
            "failed" => "✗ failed",
            "skipped" => "↷ skipped",
            "killed" => "× killed",
            "interrupted" => "! interrupted",
            "running" => "… running",
            "not-started" => "– not started",
            "service-started" => "+ service started",
            "service-reused" => "= service reused",
            _ => "? unknown",
        };
        if !legend.contains(&name) {
            legend.push(name);
        }
    }
    outln!(out, "{}", style(legend.join(" · "), "2"));
    let last = block.recent.last().unwrap();
    outln!(
        out,
        "{} chain #{} · {} · {}",
        label("Last"),
        last.chain_id,
        last.time,
        last.status
    );
}
