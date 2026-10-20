//! Statistics over retained invocation journals only. No persistent aggregates.
use crate::run_journal::{Event, Summary};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

type Address = (String, String);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockAddress {
    pub node_id: String,
    pub block: String,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcomes {
    pub succeeded: u64,
    pub failed: u64,
    pub killed: u64,
    pub interrupted: u64,
    pub running: u64,
    pub unknown: u64,
}
impl Outcomes {
    fn add(&mut self, status: &str) {
        *match status {
            "succeeded" => &mut self.succeeded,
            "failed" => &mut self.failed,
            "killed" => &mut self.killed,
            "interrupted" => &mut self.interrupted,
            "running" => &mut self.running,
            _ => &mut self.unknown,
        } += 1;
    }
    pub fn total(&self) -> u64 {
        self.succeeded + self.failed + self.killed + self.interrupted + self.running + self.unknown
    }
    pub fn success_rate(&self) -> Option<f64> {
        let n = self.succeeded + self.failed;
        (n > 0).then(|| 100.0 * self.succeeded as f64 / n as f64)
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationStats {
    pub samples: usize,
    pub total_ms: u64,
    pub median_ms: Option<f64>,
    pub p95_ms: Option<u64>,
}
impl DurationStats {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        let n = samples.len();
        let median_ms = if n == 0 {
            None
        } else if n % 2 == 0 {
            Some(samples[n / 2 - 1] as f64 / 2.0 + samples[n / 2] as f64 / 2.0)
        } else {
            Some(samples[n / 2] as f64)
        };
        Self {
            samples: n,
            total_ms: samples.iter().fold(0u64, |sum, v| sum.saturating_add(*v)),
            median_ms,
            p95_ms: (n > 0).then(|| samples[(n * 95).div_ceil(100) - 1]),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub chain_id: i64,
    pub time: String,
    pub status: String,
    pub duration_ms: Option<u64>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockStats {
    pub node_id: String,
    pub block: String,
    pub outcomes: Outcomes,
    pub success_rate_percent: Option<f64>,
    pub skipped: u64,
    pub not_started: u64,
    pub pending: u64,
    pub unobserved: u64,
    pub services_started: u64,
    pub services_reused: u64,
    pub skip_rate_percent: Option<f64>,
    pub estimated_avoided_ms: u64,
    pub estimated_avoided_samples: u64,
    pub duration: DurationStats,
    pub recent: Vec<Recent>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub retained_chains: usize,
    pub retained_from: Option<String>,
    pub retained_to: Option<String>,
    pub matching_chains: usize,
    pub from: Option<String>,
    pub to: Option<String>,
    pub requested_since: Option<String>,
    pub since_unix: Option<i64>,
    pub incomplete_chains: usize,
    pub truncated_chains: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub scope: String,
    pub address: Option<BlockAddress>,
    pub coverage: Coverage,
    pub chains: Outcomes,
    pub chain_success_rate_percent: Option<f64>,
    pub chain_duration: DurationStats,
    pub blocks: Vec<BlockStats>,
}

pub(crate) struct Snapshot {
    pub summary: Summary,
    pub duration_ms: Option<u64>,
    pub steps: Vec<Address>,
    pub events: Vec<Event>,
}

#[derive(Default)]
struct Accumulator {
    stats: BlockStats,
    durations: Vec<u64>,
}
impl Accumulator {
    fn record(&mut self, chain: i64, time: &str, status: &str, duration: Option<u64>) {
        self.stats.outcomes.add(status);
        // Only completed executions contribute samples; cancellations and
        // interruption timestamps do not represent a full execution's duration.
        let duration = duration.filter(|_| matches!(status, "succeeded" | "failed"));
        if let Some(ms) = duration {
            self.durations.push(ms);
        }
        self.recent(chain, time, status, duration);
    }
    fn recent(&mut self, chain: i64, time: &str, status: &str, duration: Option<u64>) {
        self.stats.recent.push(Recent {
            chain_id: chain,
            time: time.into(),
            status: status.into(),
            duration_ms: duration,
        });
    }
    fn finish(mut self) -> BlockStats {
        self.stats.duration = DurationStats::from_samples(self.durations);
        self.stats.success_rate_percent = self.stats.outcomes.success_rate();
        let attempts = self.stats.outcomes.total() + self.stats.skipped;
        self.stats.skip_rate_percent =
            (attempts > 0).then(|| 100.0 * self.stats.skipped as f64 / attempts as f64);
        // Concurrent chains may complete in a different order from their IDs.
        self.stats
            .recent
            .sort_by(|a, b| a.time.cmp(&b.time).then(a.chain_id.cmp(&b.chain_id)));
        let keep_from = self.stats.recent.len().saturating_sub(12);
        self.stats.recent.drain(..keep_from);
        self.stats
    }
}

fn accumulator<'a>(
    blocks: &'a mut BTreeMap<Address, Accumulator>,
    address: &Address,
) -> &'a mut Accumulator {
    blocks
        .entry(address.clone())
        .or_insert_with(|| Accumulator {
            stats: BlockStats {
                node_id: address.0.clone(),
                block: address.1.clone(),
                ..Default::default()
            },
            ..Default::default()
        })
}

pub(crate) fn summarize(
    retained: &[Summary],
    runs: Vec<Snapshot>,
    address: Option<Address>,
    chain: bool,
    since: Option<(String, i64)>,
) -> Report {
    let mut report = Report {
        scope: if address.is_none() {
            "canvas"
        } else if chain {
            "chain"
        } else {
            "block"
        }
        .into(),
        address: address.as_ref().map(|(n, b)| BlockAddress {
            node_id: n.clone(),
            block: b.clone(),
        }),
        coverage: Coverage {
            retained_chains: retained.len(),
            retained_from: retained.iter().map(|r| r.started_at.clone()).min(),
            retained_to: retained.iter().map(|r| r.started_at.clone()).max(),
            matching_chains: runs.len(),
            requested_since: since.as_ref().map(|s| s.0.clone()),
            since_unix: since.map(|s| s.1),
            ..Default::default()
        },
        chains: Outcomes::default(),
        chain_success_rate_percent: None,
        chain_duration: DurationStats::default(),
        blocks: Vec::new(),
    };
    let mut chain_durations = Vec::new();
    let mut blocks = BTreeMap::<Address, Accumulator>::new();
    for run in runs {
        let summary = &run.summary;
        report.chains.add(&summary.status);
        if matches!(summary.status.as_str(), "succeeded" | "failed") {
            if let Some(ms) = run.duration_ms {
                chain_durations.push(ms);
            }
        }
        report.coverage.incomplete_chains += usize::from(summary.incomplete);
        report.coverage.truncated_chains += usize::from(summary.truncated);
        let from = &mut report.coverage.from;
        if from.as_ref().is_none_or(|v| v > &summary.started_at) {
            *from = Some(summary.started_at.clone());
        }
        let to = &mut report.coverage.to;
        if to.as_ref().is_none_or(|v| v < &summary.started_at) {
            *to = Some(summary.started_at.clone());
        }
        let mut pending = BTreeMap::<Address, String>::new();
        let mut observed = BTreeSet::new();
        for item in run.events {
            let e = &item.event;
            let (Some(node), Some(block)) = (e["nodeId"].as_str(), e["block"].as_str()) else {
                continue;
            };
            let key = (node.to_string(), block.to_string());
            if !chain && address.as_ref().is_some_and(|a| a != &key) {
                continue;
            }
            let kind = e["type"].as_str().unwrap_or("");
            let stats = accumulator(&mut blocks, &key);
            observed.insert(key.clone());
            match kind {
                "step-start" => {
                    if let Some(time) = pending.insert(key, item.time.clone()) {
                        stats.record(summary.id, &time, "unknown", None);
                    }
                }
                "tty-start" => {
                    pending.entry(key).or_insert(item.time.clone());
                }
                "step-end" => {
                    pending.remove(&key);
                    let status = match e["exitCode"].as_i64() {
                        Some(0) => "succeeded",
                        Some(_) => "failed",
                        None => "unknown",
                    };
                    stats.record(summary.id, &item.time, status, e["durationMs"].as_u64());
                }
                "step-skipped" => {
                    pending.remove(&key);
                    stats.stats.skipped += 1;
                    stats.stats.estimated_avoided_samples +=
                        u64::from(e["durationMs"].as_u64().is_some());
                    stats.stats.estimated_avoided_ms = stats
                        .stats
                        .estimated_avoided_ms
                        .saturating_add(e["durationMs"].as_u64().unwrap_or(0));
                    stats.recent(summary.id, &item.time, "skipped", None);
                }
                "step-not-started" => {
                    stats.stats.not_started += 1;
                    stats.recent(summary.id, &item.time, "not-started", None);
                }
                "service-started" => {
                    pending.remove(&key);
                    if e["reused"] == true {
                        stats.stats.services_reused += 1;
                    } else {
                        stats.stats.services_started += 1;
                    }
                    stats.recent(
                        summary.id,
                        &item.time,
                        if e["reused"] == true {
                            "service-reused"
                        } else {
                            "service-started"
                        },
                        None,
                    );
                }
                "killed" => {
                    // The terminal killed event can name a step which already
                    // ended while cleanup was running. Do not count it twice.
                    if pending.remove(&key).is_some() {
                        stats.record(summary.id, &item.time, "killed", None);
                    }
                }
                _ => {}
            }
        }
        for (key, time) in pending {
            let state = if summary.incomplete {
                "unknown"
            } else {
                match summary.status.as_str() {
                    "running" => "running",
                    "interrupted" => "interrupted",
                    "failed" => "failed",
                    "killed" => "killed",
                    _ => "unknown",
                }
            };
            accumulator(&mut blocks, &key).record(summary.id, &time, state, None);
        }
        // Plans have no output to read. Include waiting/unreached steps even
        // when a crash prevented the final not-started markers from being emitted.
        for key in run.steps {
            if observed.contains(&key) || (!chain && address.as_ref().is_some_and(|a| a != &key)) {
                continue;
            }
            let stats = accumulator(&mut blocks, &key);
            if summary.status == "running" {
                stats.stats.pending += 1;
            } else if summary.incomplete {
                stats.stats.unobserved += 1;
            } else {
                stats.stats.not_started += 1;
            }
        }
    }
    report.chain_success_rate_percent = report.chains.success_rate();
    report.chain_duration = DurationStats::from_samples(chain_durations);
    report.blocks = blocks.into_values().map(Accumulator::finish).collect();
    report
}

/// Resolve the cutoff once, with checked duration arithmetic and RFC3339 offsets.
pub fn parse_since(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Some(time) = meshfox_core::timestamp::unix_timestamp(value) {
        return Some(time);
    }
    let (index, _) = value.char_indices().next_back()?;
    let (number, unit) = value.split_at(index);
    if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n = number.parse::<i64>().ok()?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        _ => return None,
    };
    let now = meshfox_core::timestamp::unix_timestamp(&meshfox_core::timestamp::now_utc_rfc3339())?;
    now.checked_sub(n.checked_mul(factor)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Query {
    #[serde(default)]
    selector: String,
    #[serde(default)]
    chain: bool,
    since: Option<String>,
}

pub(crate) async fn api(
    axum::extract::State(state): axum::extract::State<Arc<crate::AppState>>,
    axum::extract::Query(query): axum::extract::Query<Query>,
) -> Result<axum::Json<Report>, crate::ApiError> {
    use axum::http::StatusCode;
    let selector: Vec<String> = if query.selector.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&query.selector)
            .map_err(|e| crate::ApiError(StatusCode::BAD_REQUEST, e.to_string()))?
    };
    let address = crate::run_journal::resolve_selector(&state, &selector)?;
    let since = query
        .since
        .map(|value| {
            parse_since(&value)
                .map(|time| (value, time))
                .ok_or_else(|| {
                    crate::ApiError(
                        StatusCode::BAD_REQUEST,
                        "invalid since: expected RFC3339 or a duration such as 7d, 2w, 1h".into(),
                    )
                })
        })
        .transpose()?;
    let journal = state.run_journal.clone();
    tokio::task::spawn_blocking(move || journal.stats(address, query.chain, since))
        .await
        .map_err(|e| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(axum::Json)
        .map_err(|e| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn run(
        id: i64,
        status: &str,
        duration: Option<u64>,
        events: Vec<serde_json::Value>,
    ) -> Snapshot {
        let time = format!("2026-10-20T10:00:{id:02}.000Z");
        let steps = events
            .iter()
            .filter_map(|e| {
                Some((
                    e["nodeId"].as_str()?.to_string(),
                    e["block"].as_str()?.to_string(),
                ))
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Snapshot {
            summary: Summary {
                id,
                live_id: id.to_string(),
                node_id: "root".into(),
                block: "target".into(),
                path: Vec::new(),
                source: "test".into(),
                options: json!({}),
                status: status.into(),
                exit_code: None,
                started_at: time.clone(),
                ended_at: (status != "running").then(|| time.clone()),
                truncated: false,
                output_disabled: false,
                incomplete: false,
            },
            duration_ms: duration,
            steps,
            events: events
                .into_iter()
                .enumerate()
                .map(|(seq, event)| Event {
                    seq: seq as i64,
                    time: time.clone(),
                    event,
                })
                .collect(),
        }
    }
    fn start(block: &str) -> serde_json::Value {
        json!({"type":"step-start","nodeId":"root","block":block})
    }
    fn end(block: &str, code: i32, duration: u64) -> serde_json::Value {
        json!({"type":"step-end","nodeId":"root","block":block,"exitCode":code,"durationMs":duration})
    }
    fn report(runs: Vec<Snapshot>, address: Option<Address>, chain: bool) -> Report {
        let retained = runs.iter().map(|r| r.summary.clone()).collect::<Vec<_>>();
        summarize(&retained, runs, address, chain, None)
    }
    #[test]
    fn skips_and_nonterminal_outcomes_are_not_successes_or_duration_samples() {
        let result = report(
            vec![
                run(
                    1,
                    "succeeded",
                    Some(500),
                    vec![
                        start("dep"),
                        end("dep", 0, 100),
                        start("target"),
                        end("target", 0, 400),
                    ],
                ),
                run(
                    2,
                    "failed",
                    Some(1000),
                    vec![
                        json!({"type":"step-skipped","nodeId":"root","block":"dep","durationMs":100}),
                        start("target"),
                        end("target", 7, 900),
                    ],
                ),
                run(
                    3,
                    "killed",
                    Some(10000),
                    vec![
                        start("target"),
                        json!({"type":"killed","nodeId":"root","block":"target"}),
                    ],
                ),
                run(4, "interrupted", Some(90000000), vec![start("target")]),
                run(
                    5,
                    "running",
                    None,
                    vec![start("dep"), end("dep", 0, 300), start("target")],
                ),
            ],
            None,
            false,
        );
        assert_eq!(result.chains.total(), 5);
        assert_eq!(result.chain_success_rate_percent, Some(50.0));
        assert_eq!(result.chain_duration.samples, 2);
        assert_eq!(result.chain_duration.median_ms, Some(750.0));
        assert_eq!(result.chain_duration.p95_ms, Some(1000));
        let target = result.blocks.iter().find(|b| b.block == "target").unwrap();
        assert_eq!(
            (
                target.outcomes.succeeded,
                target.outcomes.failed,
                target.outcomes.killed,
                target.outcomes.interrupted,
                target.outcomes.running
            ),
            (1, 1, 1, 1, 1)
        );
        assert_eq!(target.duration.median_ms, Some(650.0));
        assert_eq!(target.duration.total_ms, 1300);
        let dep = result.blocks.iter().find(|b| b.block == "dep").unwrap();
        assert_eq!(dep.outcomes.succeeded, 2);
        assert_eq!(dep.skipped, 1);
        assert_eq!(dep.estimated_avoided_ms, 100);
        assert_eq!(dep.duration.samples, 2);
        assert_eq!(dep.duration.median_ms, Some(200.0));
        assert!((dep.skip_rate_percent.unwrap() - 100.0 / 3.0).abs() < 0.0001);
    }
    #[test]
    fn services_and_unreached_steps_are_not_executions_and_block_view_isolated() {
        let result = report(
            vec![
                run(
                    1,
                    "succeeded",
                    Some(200),
                    vec![
                        start("service"),
                        json!({"type":"service-started","nodeId":"root","block":"service","reused":false}),
                        start("dep"),
                        end("dep", 0, 100),
                    ],
                ),
                run(
                    2,
                    "failed",
                    Some(300),
                    vec![
                        start("service"),
                        json!({"type":"service-started","nodeId":"root","block":"service","reused":true}),
                        json!({"type":"step-not-started","nodeId":"root","block":"dep"}),
                    ],
                ),
            ],
            None,
            false,
        );
        let service = result.blocks.iter().find(|b| b.block == "service").unwrap();
        assert_eq!(service.outcomes.total(), 0);
        assert_eq!((service.services_started, service.services_reused), (1, 1));
        assert_eq!(service.duration.samples, 0);
        let dep = result.blocks.iter().find(|b| b.block == "dep").unwrap();
        assert_eq!(dep.outcomes.succeeded, 1);
        assert_eq!(dep.not_started, 1);
        let single = report(
            vec![run(
                1,
                "succeeded",
                Some(400),
                vec![
                    start("dep"),
                    end("dep", 0, 100),
                    start("target"),
                    end("target", 0, 300),
                ],
            )],
            Some(("root".into(), "dep".into())),
            false,
        );
        assert_eq!(single.scope, "block");
        assert_eq!(single.blocks.len(), 1);
        assert_eq!(single.blocks[0].block, "dep");
    }
    #[test]
    fn incomplete_transcript_does_not_invent_a_success_from_chain_outcome() {
        let mut snapshot = run(1, "succeeded", Some(500), vec![start("target")]);
        snapshot.steps.push(("root".into(), "unobserved".into()));
        snapshot.summary.incomplete = true;
        snapshot.summary.truncated = true;
        let result = report(vec![snapshot], None, false);
        assert_eq!(result.coverage.incomplete_chains, 1);
        assert_eq!(result.coverage.truncated_chains, 1);
        assert_eq!(result.blocks[0].outcomes.unknown, 1);
        assert_eq!(result.blocks[0].outcomes.succeeded, 0);
        assert_eq!(result.blocks[0].duration.samples, 0);
        let unobserved = result
            .blocks
            .iter()
            .find(|b| b.block == "unobserved")
            .unwrap();
        assert_eq!(unobserved.unobserved, 1);
        assert_eq!(unobserved.outcomes.total(), 0);
    }
    #[test]
    fn percentile_uses_nearest_rank_and_empty_metrics_are_null() {
        let samples = DurationStats::from_samples((1..=20).collect());
        assert_eq!(samples.median_ms, Some(10.5));
        assert_eq!(samples.p95_ms, Some(19));
        let empty = report(Vec::new(), None, false);
        assert_eq!(empty.coverage.retained_chains, 0);
        assert_eq!(empty.chain_duration.median_ms, None);
        assert_eq!(empty.chain_success_rate_percent, None);
    }
    #[test]
    fn cutoff_handles_offsets_overflow_and_unicode_without_panicking() {
        assert_eq!(
            parse_since("2026-10-20T14:00:00+04:00"),
            parse_since("2026-10-20T10:00:00Z")
        );
        assert!(parse_since("7d").is_some());
        for bad in ["", "💥", "7💥", "-1d", "tomorrow", "9223372036854775807w"] {
            assert!(parse_since(bad).is_none(), "{bad}");
        }
    }
}
