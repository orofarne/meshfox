//! Per-step status of one chain, folded from the worker's saved journal events.
//! Presentation only: like `logs`, nothing here opens the canvas or the session database.
use crate::{coordinator, logs_cmd::style, worker_client};
use meshfox_core::{datevars::parse_datetime, format_duration_ms};
use meshfox_server::run_journal::{Event, Summary};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    time::Duration,
};
use time::OffsetDateTime;

const TAIL_LINES: usize = 20;

pub struct Options {
    pub selector: Vec<String>,
    pub chain: bool,
    pub id: Option<i64>,
    pub watch: bool,
    pub json: bool,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Pending,
    Running,
    /// A service that reported readiness; it has no end event of its own.
    Ready,
    Succeeded,
    Failed,
    Skipped,
    Killed,
    NotStarted,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Running => "running",
            State::Ready => "ready",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::Skipped => "skipped",
            State::Killed => "killed",
            State::NotStarted => "not-started",
        }
    }
    fn mark(self) -> (&'static str, &'static str) {
        match self {
            State::Succeeded => ("✔", "1;32"),
            State::Ready => ("●", "32"),
            State::Running => ("▶", "1;33"),
            State::Failed | State::Killed => ("✘", "1;31"),
            State::Skipped => ("↷", "2"),
            State::Pending | State::NotStarted => ("·", "2"),
        }
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tests {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub node: String,
    pub block: String,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<Tests>,
}

impl Step {
    fn addr(&self) -> String {
        format!("{}/{}", self.node, self.block)
    }
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub step: String,
    pub tests: Vec<String>,
    pub tail: Vec<String>,
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub id: i64,
    pub target: String,
    pub status: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub run: RunInfo,
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    pub failures: Vec<Failure>,
}

/// Per-step output scratch: the last lines and what the test runners said.
#[derive(Default)]
struct Scratch {
    tail: VecDeque<String>,
    failed_tests: Vec<String>,
    tests: Option<Tests>,
}

pub fn exit_code(status: &str) -> i32 {
    match status {
        "failed" | "killed" | "interrupted" => 1,
        _ => 0,
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `"… 507 passed; 0 failed"` → the number in front of `word`.
fn count(line: &str, word: &str) -> Option<u64> {
    line.split(';').find_map(|part| {
        let mut words = part.split_whitespace().rev();
        (words.next()? == word).then_some(())?;
        words.next()?.parse().ok()
    })
}

/// Recognize the summary lines of `cargo test`, Playwright and `node --test`.
fn scan(line: &str, scratch: &mut Scratch) {
    let line = line.trim();
    let add = |scratch: &mut Scratch, passed: u64, failed: u64, ignored: u64| {
        let tests = scratch.tests.get_or_insert_with(Tests::default);
        tests.passed += passed;
        tests.failed += failed;
        tests.ignored += ignored;
    };
    if let Some(rest) = line.strip_prefix("test result:") {
        add(
            scratch,
            count(rest, "passed").unwrap_or(0),
            count(rest, "failed").unwrap_or(0),
            count(rest, "ignored").unwrap_or(0),
        );
    } else if let Some(name) = line
        .strip_prefix("test ")
        .and_then(|rest| rest.strip_suffix(" ... FAILED"))
    {
        scratch.failed_tests.push(name.to_string());
    } else if let Some(rest) = line.strip_prefix("# ") {
        let mut words = rest.split_whitespace();
        if let (Some(kind), Some(Ok(n))) = (words.next(), words.next().map(str::parse::<u64>)) {
            match kind {
                "pass" => add(scratch, n, 0, 0),
                "fail" => add(scratch, 0, n, 0),
                "skipped" => add(scratch, 0, 0, n),
                _ => {}
            }
        }
    } else if let Some((n, kind)) = line
        .split_once(' ')
        .and_then(|(n, rest)| Some((n.parse::<u64>().ok()?, rest.split_whitespace().next()?)))
    {
        match kind {
            "passed" => add(scratch, n, 0, 0),
            "failed" => add(scratch, 0, n, 0),
            "skipped" => add(scratch, 0, 0, n),
            _ => {}
        }
    } else if let Some((digits, rest)) = line.split_once(") [") {
        // Playwright's failure list: `1) [chrome-x] › file:line › title`.
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            scratch.failed_tests.push(format!("[{rest}"));
        }
    }
}

fn millis_between(from: &str, to: OffsetDateTime) -> Option<u64> {
    let from = parse_datetime(from)?;
    u64::try_from((to - from).whole_milliseconds()).ok()
}

/// Fold a chain's summary and events into one report. `now` times a running chain.
pub fn fold(run: &Summary, events: &[Event], now: Option<OffsetDateTime>) -> Report {
    let mut steps: Vec<Step> = Vec::new();
    let mut scratch: Vec<Scratch> = Vec::new();
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut slot = |node: &str, block: &str, steps: &mut Vec<Step>, scratch: &mut Vec<Scratch>| {
        *index
            .entry((node.to_string(), block.to_string()))
            .or_insert_with(|| {
                steps.push(Step {
                    node: node.to_string(),
                    block: block.to_string(),
                    state: State::Pending,
                    started_at: None,
                    duration_ms: None,
                    exit_code: None,
                    reason: None,
                    tests: None,
                });
                scratch.push(Scratch::default());
                steps.len() - 1
            })
    };
    for planned in run.options["steps"].as_array().into_iter().flatten() {
        if let (Some(node), Some(block)) = (planned["nodeId"].as_str(), planned["block"].as_str()) {
            slot(node, block, &mut steps, &mut scratch);
        }
    }
    slot(&run.node_id, &run.block, &mut steps, &mut scratch);

    let mut current: Option<usize> = None;
    for item in events {
        let event = &item.event;
        let node = event["nodeId"].as_str().unwrap_or("");
        let block = event["block"].as_str().unwrap_or("");
        match event["type"].as_str().unwrap_or("") {
            "step-start" | "tty-start" => {
                let i = slot(node, block, &mut steps, &mut scratch);
                steps[i].state = State::Running;
                steps[i].started_at = Some(item.time.clone());
                current = Some(i);
            }
            "step-end" => {
                let i = slot(node, block, &mut steps, &mut scratch);
                let code = event["exitCode"].as_i64().unwrap_or(1);
                steps[i].exit_code = Some(code);
                steps[i].duration_ms = event["durationMs"].as_u64();
                steps[i].state = if code == 0 {
                    State::Succeeded
                } else {
                    State::Failed
                };
                if current == Some(i) {
                    current = None;
                }
            }
            "service-started" => {
                let i = slot(node, block, &mut steps, &mut scratch);
                steps[i].state = State::Ready;
                if current == Some(i) {
                    current = None;
                }
            }
            "step-skipped" => {
                let i = slot(node, block, &mut steps, &mut scratch);
                steps[i].state = State::Skipped;
                steps[i].reason = event["reason"].as_str().map(str::to_string);
            }
            "step-not-started" => {
                let i = slot(node, block, &mut steps, &mut scratch);
                steps[i].state = State::NotStarted;
                steps[i].reason = event["reason"].as_str().map(str::to_string);
            }
            "killed" => {
                if node.is_empty() {
                    for step in steps.iter_mut().filter(|s| s.state == State::Running) {
                        step.state = State::Killed;
                    }
                } else {
                    let i = slot(node, block, &mut steps, &mut scratch);
                    steps[i].state = State::Killed;
                }
            }
            "output" => {
                let i = if node.is_empty() {
                    current
                } else {
                    Some(slot(node, block, &mut steps, &mut scratch))
                };
                let Some(i) = i else { continue };
                for line in strip_ansi(event["text"].as_str().unwrap_or("")).lines() {
                    scan(line, &mut scratch[i]);
                    let tail = &mut scratch[i].tail;
                    if tail.len() == TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line.to_string());
                }
            }
            _ => {}
        }
    }

    let finished = run.status != "running";
    let mut failures = Vec::new();
    for (step, scratch) in steps.iter_mut().zip(scratch) {
        if finished && step.state == State::Running {
            step.state = State::Killed;
        }
        step.tests = scratch.tests;
        if matches!(step.state, State::Failed | State::Killed) {
            failures.push(Failure {
                step: step.addr(),
                tests: scratch.failed_tests,
                tail: scratch.tail.into(),
            });
        }
    }
    let end = run
        .ended_at
        .as_deref()
        .and_then(parse_datetime)
        .or(if finished { None } else { now });
    Report {
        run: RunInfo {
            id: run.id,
            target: format!("{}::{}", run.node_id, run.block),
            status: run.status.clone(),
            started_at: run.started_at.clone(),
            ended_at: run.ended_at.clone(),
            elapsed_ms: end.and_then(|end| millis_between(&run.started_at, end)),
            exit_code: run.exit_code,
            truncated: run.truncated,
        },
        current: if finished {
            None
        } else {
            current.map(|i| steps[i].addr())
        },
        steps,
        failures,
    }
}

fn tests_text(tests: &Tests) -> String {
    let mut text = format!("{} passed, {} failed", tests.passed, tests.failed);
    if tests.ignored > 0 {
        text += &format!(", {} ignored", tests.ignored);
    }
    text
}

fn step_line(step: &Step, now: Option<OffsetDateTime>) -> String {
    let (mark, sgr) = step.state.mark();
    let time = step
        .duration_ms
        .or_else(|| {
            let started = step.started_at.as_deref()?;
            (step.state == State::Running)
                .then(|| millis_between(started, now?))
                .flatten()
        })
        .map(format_duration_ms)
        .unwrap_or_default();
    let detail = match (&step.tests, &step.reason) {
        (Some(tests), _) => tests_text(tests),
        (None, Some(reason)) => reason.clone(),
        _ => String::new(),
    };
    format!(
        "  {} {:<34} {} {:>8}  {}",
        style(mark, sgr),
        step.addr(),
        style(format!("{:<11}", step.state.label()), sgr),
        time,
        style(detail, "2")
    )
    .trim_end()
    .to_string()
}

fn render(report: &Report, now: Option<OffsetDateTime>) -> String {
    let run = &report.run;
    let mut out = format!(
        "{} · {} · {}{}\n",
        style(format!("Run #{}", run.id), "1;36"),
        style(&run.target, "1"),
        style(
            &run.status,
            match run.status.as_str() {
                "running" => "1;33",
                "succeeded" => "1;32",
                "failed" | "killed" => "1;31",
                _ => "2",
            }
        ),
        run.elapsed_ms
            .map_or(String::new(), |ms| format!(" · {}", format_duration_ms(ms)))
    );
    for step in &report.steps {
        out += &step_line(step, now);
        out.push('\n');
    }
    for failure in &report.failures {
        out += &format!("\n{}\n", style(format!("Failed: {}", failure.step), "1;31"));
        for test in &failure.tests {
            out += &format!("  {test}\n");
        }
        if failure.tests.is_empty() {
            for line in &failure.tail {
                out += &format!("  {}\n", style(line, "2"));
            }
        }
    }
    if run.truncated {
        out += &format!(
            "\n{}\n",
            style(
                "[output truncated by the transcript byte limit: test counts may be incomplete]",
                "33"
            )
        );
    }
    out
}

fn typed(kind: &str, mut value: Value) -> Value {
    value["type"] = json!(kind);
    value
}

fn snapshot_json(report: &Report) -> Value {
    typed(
        "snapshot",
        serde_json::to_value(report).unwrap_or(Value::Null),
    )
}

/// The changes between two folds of the same chain, as `--watch --json` lines.
fn changes(before: &Report, after: &Report) -> Vec<Value> {
    let mut out = Vec::new();
    for step in &after.steps {
        let old = before
            .steps
            .iter()
            .find(|s| s.node == step.node && s.block == step.block);
        if old.map(|s| s.state) != Some(step.state) {
            out.push(typed(
                "step",
                json!({"runId":after.run.id,"step":serde_json::to_value(step).unwrap_or(Value::Null)}),
            ));
        }
    }
    for failure in &after.failures {
        if !before.failures.iter().any(|f| f.step == failure.step) {
            out.push(typed(
                "failure",
                json!({"runId":after.run.id,"failure":serde_json::to_value(failure).unwrap_or(Value::Null)}),
            ));
        }
    }
    out
}

struct Source {
    path: PathBuf,
    port: u16,
    id: Option<i64>,
    since: i64,
    events: Vec<Event>,
}

impl Source {
    /// Fetch the next page of events; returns the chain summary and whether more is waiting.
    async fn poll(
        &mut self,
        options: &Options,
    ) -> Result<(Summary, bool), worker_client::LogsError> {
        let fail = |message: String| worker_client::LogsError {
            message,
            retryable: false,
        };
        let mut params = vec![
            (
                "selector".to_string(),
                serde_json::to_string(&options.selector).map_err(|e| fail(e.to_string()))?,
            ),
            ("chain".into(), options.chain.to_string()),
            ("effective".into(), "false".into()),
            ("list".into(), "false".into()),
            ("sinceSeq".into(), self.since.to_string()),
        ];
        if let Some(id) = self.id {
            params.push(("id".into(), id.to_string()));
        }
        let page = worker_client::logs(self.port, &params).await?;
        let summary = page
            .selected
            .ok_or_else(|| fail("no selected run".into()))?;
        self.id = Some(summary.id);
        self.since = page.next_seq;
        self.events.extend(page.events);
        Ok((summary, page.more))
    }

    async fn settle(&mut self, options: &Options) -> Result<Summary, worker_client::LogsError> {
        loop {
            let (summary, more) = self.poll(options).await?;
            if !more {
                return Ok(summary);
            }
        }
    }
}

/// Returns the process exit code: 0 for a running or succeeded chain, 1 for a failed one.
pub async fn run(path: PathBuf, options: Options) -> Result<i32, String> {
    tokio::select! {
        result=read(path,options)=>result,
        signal=tokio::signal::ctrl_c()=>signal.map(|()|0).map_err(|e|e.to_string()),
    }
}

async fn read(path: PathBuf, options: Options) -> Result<i32, String> {
    let port = coordinator::get_or_spawn(&path)
        .await
        .map_err(|e| e.to_string())?;
    let mut source = Source {
        path,
        port,
        id: options.id,
        since: 0,
        events: Vec::new(),
    };
    let summary = source.settle(&options).await.map_err(|e| e.to_string())?;
    let now = OffsetDateTime::now_utc();
    let mut report = fold(&summary, &source.events, Some(now));
    if options.json {
        println!("{}", snapshot_json(&report));
    } else {
        print!("{}", render(&report, Some(now)));
    }
    if !options.watch {
        return Ok(exit_code(&report.run.status));
    }
    let mut disconnected = false;
    while report.run.status == "running" {
        let (summary, more) = match source.poll(&options).await {
            Ok(page) => {
                if disconnected {
                    eprintln!("Reconnected");
                    disconnected = false;
                }
                page
            }
            Err(error) if error.retryable => {
                if !disconnected {
                    eprintln!("Connection lost ({error}); reconnecting…");
                    disconnected = true;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                if let Ok(found) = coordinator::get_or_spawn(&source.path).await {
                    source.port = found;
                }
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        if more {
            continue;
        }
        let now = OffsetDateTime::now_utc();
        let next = fold(&summary, &source.events, Some(now));
        for change in changes(&report, &next) {
            if options.json {
                println!("{change}");
            }
        }
        if !options.json {
            for step in &next.steps {
                let old = report
                    .steps
                    .iter()
                    .find(|s| s.node == step.node && s.block == step.block);
                if old.map(|s| s.state) != Some(step.state) {
                    println!("{}", step_line(step, Some(now)));
                }
            }
        }
        report = next;
        if report.run.status == "running" {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let code = exit_code(&report.run.status);
    if options.json {
        println!(
            "{}",
            typed(
                "done",
                json!({"run":serde_json::to_value(&report.run).unwrap_or(Value::Null),"exitCode":code})
            )
        );
    } else {
        print!("\n{}", render(&report, None));
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(status: &str) -> Summary {
        serde_json::from_value(json!({
            "id": 7, "liveId": "x", "nodeId": "full", "block": "full",
            "path": ["full"], "source": "cli",
            "options": {"steps": [
                {"nodeId": "unit", "block": "run"},
                {"nodeId": "e2e", "block": "run"},
            ]},
            "status": status, "exitCode": null,
            "startedAt": "2026-10-28T10:00:00.000Z", "endedAt": null,
            "truncated": false, "outputDisabled": false, "incomplete": false,
        }))
        .unwrap()
    }

    fn events(list: Vec<Value>) -> Vec<Event> {
        list.into_iter()
            .enumerate()
            .map(|(seq, event)| Event {
                seq: seq as i64,
                time: "2026-10-28T10:00:01.000Z".into(),
                event,
            })
            .collect()
    }

    fn out(node: &str, text: &str) -> Value {
        json!({"type":"output","nodeId":node,"block":"run","stream":"stdout","text":text})
    }

    #[test]
    fn planned_steps_start_pending_and_the_running_one_is_current() {
        let report = fold(
            &summary("running"),
            &events(vec![
                json!({"type":"step-start","nodeId":"unit","block":"run"}),
                out(
                    "unit",
                    "\x1b[32mtest result: ok. 507 passed; 0 failed; 2 ignored\x1b[0m",
                ),
                out("unit", "test result: ok. 3 passed; 0 failed; 0 ignored"),
            ]),
            None,
        );
        let states: Vec<_> = report.steps.iter().map(|s| (s.addr(), s.state)).collect();
        assert_eq!(
            states,
            [
                ("unit/run".into(), State::Running),
                ("e2e/run".into(), State::Pending),
                ("full/full".into(), State::Pending),
            ]
        );
        assert_eq!(report.current.as_deref(), Some("unit/run"));
        assert_eq!(
            report.steps[0].tests,
            Some(Tests {
                passed: 510,
                failed: 0,
                ignored: 2
            })
        );
        assert!(report.failures.is_empty());
    }

    #[test]
    fn a_failed_step_reports_its_failing_tests_and_the_output_tail() {
        let report = fold(
            &summary("failed"),
            &events(vec![
                json!({"type":"step-start","nodeId":"unit","block":"run"}),
                out("unit", "test a::ok ... ok\ntest a::bad ... FAILED"),
                out("unit", "test result: FAILED. 1 passed; 1 failed; 0 ignored"),
                json!({"type":"step-end","nodeId":"unit","block":"run","exitCode":101,"durationMs":1500}),
                json!({"type":"step-not-started","nodeId":"e2e","block":"run","reason":"chain ended before this step"}),
                json!({"type":"done"}),
            ]),
            None,
        );
        assert_eq!(report.steps[0].state, State::Failed);
        assert_eq!(report.steps[0].exit_code, Some(101));
        assert_eq!(report.steps[1].state, State::NotStarted);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].step, "unit/run");
        assert_eq!(report.failures[0].tests, ["a::bad"]);
        assert_eq!(report.failures[0].tail.len(), 3);
        assert_eq!(report.current, None);
        assert_eq!(exit_code(&report.run.status), 1);
    }

    #[test]
    fn a_ready_service_is_not_a_failure_when_its_chain_ends() {
        let report = fold(
            &summary("succeeded"),
            &events(vec![
                json!({"type":"step-start","nodeId":"unit","block":"run"}),
                json!({"type":"service-started","nodeId":"unit","block":"run","pid":9,"reused":false}),
                json!({"type":"done"}),
            ]),
            None,
        );
        assert_eq!(report.steps[0].state, State::Ready);
        assert!(report.failures.is_empty());
        assert_eq!(report.current, None);
    }

    #[test]
    fn a_step_left_running_when_the_chain_ended_counts_as_killed() {
        let report = fold(
            &summary("interrupted"),
            &events(vec![
                json!({"type":"step-start","nodeId":"unit","block":"run"}),
            ]),
            None,
        );
        assert_eq!(report.steps[0].state, State::Killed);
        assert_eq!(report.failures[0].step, "unit/run");
    }

    #[test]
    fn playwright_and_node_test_summaries_are_counted() {
        let mut scratch = Scratch::default();
        for line in [
            "  1) [chrome-x] › a.spec.ts:3:1 › does a thing",
            "  355 passed (3.2m)",
            "  1 failed",
            "  2 skipped",
            "# pass 12",
            "# fail 0",
        ] {
            scan(line, &mut scratch);
        }
        assert_eq!(
            scratch.tests,
            Some(Tests {
                passed: 367,
                failed: 1,
                ignored: 2
            })
        );
        assert_eq!(
            scratch.failed_tests,
            ["[chrome-x] › a.spec.ts:3:1 › does a thing"]
        );
    }

    #[test]
    fn elapsed_time_runs_to_now_while_running_and_to_the_end_when_finished() {
        let now = parse_datetime("2026-10-28T10:01:30.000Z");
        let running = fold(&summary("running"), &[], now);
        assert_eq!(running.run.elapsed_ms, Some(90_000));
        let mut done = summary("succeeded");
        done.ended_at = Some("2026-10-28T10:00:10.000Z".into());
        assert_eq!(fold(&done, &[], now).run.elapsed_ms, Some(10_000));
    }

    #[test]
    fn changes_report_only_new_states_and_new_failures() {
        let before = fold(
            &summary("running"),
            &events(vec![
                json!({"type":"step-start","nodeId":"unit","block":"run"}),
            ]),
            None,
        );
        let after = fold(
            &summary("running"),
            &events(vec![
                json!({"type":"step-start","nodeId":"unit","block":"run"}),
                json!({"type":"step-end","nodeId":"unit","block":"run","exitCode":1,"durationMs":5}),
            ]),
            None,
        );
        let kinds: Vec<_> = changes(&before, &after)
            .iter()
            .map(|v| v["type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, ["step", "failure"]);
        assert!(changes(&after, &after).is_empty());
    }

    #[test]
    fn json_snapshot_has_a_stable_shape() {
        let value = snapshot_json(&fold(&summary("running"), &[], None));
        assert_eq!(value["type"], "snapshot");
        assert_eq!(value["run"]["target"], "full::full");
        assert_eq!(value["steps"][0]["state"], "pending");
        assert_eq!(value["steps"][0]["node"], "unit");
        assert!(value["failures"].as_array().unwrap().is_empty());
        assert!(value.get("current").is_none());
    }
}
