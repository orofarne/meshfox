//! Snapshot and follow views of worker-owned, immutable invocation history.
use crate::{coordinator, worker_client};
use meshfox_server::run_journal::{Event, Summary};
use serde_json::{json, Value};
use std::{
    io::{IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};

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
                        "#{}\t{}\t{}\t{}::{}\t{}",
                        run.id, run.started_at, run.status, run.node_id, run.block, run.source
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
                    println!("Block was not started in this invocation.");
                }
                if summary.incomplete {
                    println!("[transcript incomplete: some events could not be saved]");
                }
                if summary.truncated {
                    println!("[output truncated by the transcript byte limit]");
                }
                if summary.output_disabled {
                    println!("[storing output was disabled for this invocation]");
                }
                println!(
                    "\nRun #{} · {}{}",
                    summary.id,
                    summary.status,
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
        "Run #{} · {}::{} · {}\n{} · {}",
        run.id, run.node_id, run.block, run.source, run.started_at, run.status
    );
}
fn banner(text: &str) {
    let color = std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    if color {
        println!("\n\x1b[1;36m==> {text}\x1b[0m");
    } else {
        println!("\n==> {text}");
    }
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
            println!("not started: {}", string(event, "reason"));
        }
        "output" => {
            let text = string(event, "text");
            if event["stream"] == "stderr" {
                println!("[stderr] {text}");
            } else {
                println!("{text}");
            }
        }
        "step-skipped" => {
            banner(&addr);
            println!("skipped: {}", string(event, "reason"));
            if effective {
                let source = &event["effective"];
                if let Some(id) = source["runId"].as_i64() {
                    println!("using block run #{id} · {}", string(source, "startedAt"));
                }
                if source["available"] == true {
                    if let Some(lines) = source["lines"].as_array() {
                        for line in lines {
                            let prefix = if line["stream"] == "stderr" {
                                "    [stderr] "
                            } else {
                                "    "
                            };
                            for text in string(line, "text").lines() {
                                println!("{prefix}{text}");
                            }
                        }
                    }
                    if let Some(code) = source["exitCode"].as_i64() {
                        println!(
                            "    (exit {code} · {})",
                            meshfox_core::format_duration_ms(
                                source["durationMs"].as_u64().unwrap_or(0)
                            )
                        );
                    }
                } else {
                    println!("    [original output unavailable]");
                }
                if event["truncated"] == true {
                    println!("    [effective output truncated]");
                }
            }
        }
        "step-end" => println!(
            "(exit {} · {})",
            event["exitCode"],
            meshfox_core::format_duration_ms(event["durationMs"].as_u64().unwrap_or(0))
        ),
        "service-started" => println!(
            "{addr}: service {} (pid {})",
            if event["reused"] == true {
                "reused"
            } else {
                "ready"
            },
            event["pid"]
        ),
        "tty-start" => println!("{addr}: TTY session (terminal output is not recorded)"),
        "error" => println!("error: {}", string(event, "message")),
        "lock-conflict" => println!("{addr}: locked by {}", string(event, "ownerDesc")),
        "killed" => println!("{addr}: killed"),
        _ => {}
    }
}
