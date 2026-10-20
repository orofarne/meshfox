//! Snapshot and follow views of worker-owned, immutable invocation history.
use crate::{coordinator, run_output, worker_client};
use meshfox_server::run_journal::{Event, Summary};
use serde_json::{json, Value};
use std::{io::Write, path::PathBuf, time::Duration};

pub struct Options {
    pub selector: Vec<String>,
    pub chain: bool,
    pub id: Option<i64>,
    pub effective: bool,
    pub follow: bool,
    pub list: bool,
    pub json: bool,
}

pub async fn run(path: PathBuf, options: Options) -> Result<(), String> {
    tokio::select! {
        result=read(path,options)=>result,
        signal=tokio::signal::ctrl_c()=>signal.map_err(|e|e.to_string()),
    }
}

async fn read(path: PathBuf, options: Options) -> Result<(), String> {
    let mut port = coordinator::get_or_spawn(&path)
        .await
        .map_err(|e| e.to_string())?;
    let mut id = options.id;
    let mut since = 0;
    let mut first = true;
    let mut disconnected = false;
    let mut saw_step = false;
    loop {
        let mut params = vec![
            (
                "selector".into(),
                serde_json::to_string(&options.selector).map_err(|e| e.to_string())?,
            ),
            ("chain".into(), options.chain.to_string()),
            ("effective".into(), options.effective.to_string()),
            ("list".into(), options.list.to_string()),
            ("sinceSeq".into(), since.to_string()),
        ];
        if let Some(id) = id {
            params.push(("id".into(), id.to_string()));
        }
        let page = match worker_client::logs(port, &params).await {
            Ok(page) => {
                if disconnected {
                    eprintln!("Reconnected");
                    disconnected = false;
                }
                page
            }
            Err(error) if options.follow && !first && error.retryable => {
                if !disconnected {
                    eprintln!("Connection lost ({error}); reconnecting…");
                    disconnected = true;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                if let Ok(found) = coordinator::get_or_spawn(&path).await {
                    port = found;
                }
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        if options.list {
            for run in page.runs {
                if options.json {
                    println!("{}", json!({"type":"run","run":run}));
                } else {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        style(format!("#{}", run.id), "1;36"),
                        style(&run.started_at, "2"),
                        status(&run.status),
                        style(format!("{}::{}", run.node_id, run.block), "1"),
                        style(&run.source, "2")
                    );
                }
            }
            return Ok(());
        }
        let summary = page.selected.ok_or("no selected run")?;
        id = Some(summary.id);
        if first {
            print_summary(&summary, options.json);
            first = false;
        }
        for event in page.events {
            saw_step |= matches!(
                event.event["type"].as_str(),
                Some("step-start" | "step-skipped" | "tty-start" | "step-not-started")
            );
            if options.json {
                println!(
                    "{}",
                    json!({"type":"event","chainId":summary.id,"seq":event.seq,"time":event.time,"event":event.event})
                );
            } else {
                print_event(&event, options.effective);
            }
        }
        let _ = std::io::stdout().flush();
        since = page.next_seq;
        if !page.more && (!options.follow || summary.status != "running") {
            if !options.json {
                if !options.selector.is_empty() && !saw_step {
                    println!(
                        "{}",
                        style("Block was not started in this invocation.", "2")
                    );
                }
                if summary.incomplete {
                    println!(
                        "{}",
                        style(
                            "[transcript incomplete: some events could not be saved]",
                            "33"
                        )
                    );
                }
                if summary.truncated {
                    println!(
                        "{}",
                        style("[output truncated by the transcript byte limit]", "33")
                    );
                }
                if summary.output_disabled {
                    println!(
                        "{}",
                        style("[storing output was disabled for this invocation]", "33")
                    );
                }
                println!(
                    "\n{} · {}{}",
                    style(format!("Run #{}", summary.id), "1;36"),
                    status(&summary.status),
                    summary
                        .exit_code
                        .map_or(String::new(), |code| format!(" · exit {code}"))
                );
            } else {
                println!("{}", json!({"type":"status","run":summary}));
            }
            return Ok(());
        }
        if !page.more {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

fn print_summary(run: &Summary, json_output: bool) {
    if json_output {
        println!("{}", json!({"type":"run","run":run}));
        return;
    }
    println!(
        "{} · {} · {}\n{} · {}",
        style(format!("Run #{}", run.id), "1;36"),
        style(format!("{}::{}", run.node_id, run.block), "1"),
        style(&run.source, "2"),
        style(&run.started_at, "2"),
        status(&run.status),
    );
}

pub(crate) fn style(text: impl AsRef<str>, sgr: &str) -> String {
    let text = text.as_ref();
    if run_output::color_enabled() {
        format!("\x1b[{sgr}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn status(text: &str) -> String {
    style(
        text,
        match text {
            "running" => "1;33",
            "succeeded" => "1;32",
            "failed" | "killed" => "1;31",
            "interrupted" => "1;33",
            _ => "2",
        },
    )
}

fn banner(text: &str) {
    run_output::banner(text, "\n");
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn print_event(item: &Event, effective: bool) {
    let event = &item.event;
    let addr = format!("{}::{}", string(event, "nodeId"), string(event, "block"));
    match string(event, "type") {
        "step-start" => banner(&addr),
        "step-not-started" => {
            banner(&addr);
            println!(
                "{}",
                style(format!("not started: {}", string(event, "reason")), "2")
            );
        }
        "output" => {
            let text = string(event, "text");
            if event["stream"] == "stderr" {
                println!("{} {text}", style("[stderr]", "31"));
            } else {
                println!("{text}");
            }
        }
        "step-skipped" => {
            banner(&addr);
            println!(
                "{}",
                style(format!("skipped: {}", string(event, "reason")), "2")
            );
            if effective {
                let source = &event["effective"];
                if let Some(id) = source["runId"].as_i64() {
                    println!(
                        "{}",
                        style(
                            format!("using block run #{id} · {}", string(source, "startedAt")),
                            "2"
                        )
                    );
                }
                if source["available"] == true {
                    if let Some(lines) = source["lines"].as_array() {
                        for line in lines {
                            let prefix = if line["stream"] == "stderr" {
                                format!("    {} ", style("[stderr]", "31"))
                            } else {
                                "    ".into()
                            };
                            for text in string(line, "text").lines() {
                                println!("{prefix}{text}");
                            }
                        }
                    }
                    if let Some(code) = source["exitCode"].as_i64() {
                        println!(
                            "{}",
                            style(
                                format!(
                                    "    (exit {code} · {})",
                                    meshfox_core::format_duration_ms(
                                        source["durationMs"].as_u64().unwrap_or(0)
                                    )
                                ),
                                if code == 0 { "32" } else { "31" }
                            )
                        );
                    }
                } else {
                    println!("{}", style("    [original output unavailable]", "33"));
                }
                if event["truncated"] == true {
                    println!("{}", style("    [effective output truncated]", "33"));
                }
            }
        }
        "step-end" => run_output::step_end(
            event["exitCode"].as_i64().unwrap_or(1) as i32,
            event["durationMs"].as_u64().unwrap_or(0),
            "\n",
        ),
        "service-started" => println!(
            "{}",
            style(
                format!(
                    "{addr}: service {} (pid {})",
                    if event["reused"] == true {
                        "reused"
                    } else {
                        "ready"
                    },
                    event["pid"]
                ),
                "32"
            )
        ),
        "tty-start" => println!("{addr}: TTY session (terminal output is not recorded)"),
        "error" => println!(
            "{}",
            style(format!("error: {}", string(event, "message")), "1;31")
        ),
        "lock-conflict" => println!(
            "{}",
            style(
                format!("{addr}: locked by {}", string(event, "ownerDesc")),
                "33"
            )
        ),
        "killed" => println!("{}", style(format!("{addr}: killed"), "1;31")),
        _ => {}
    }
}
