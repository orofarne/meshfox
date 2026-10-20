//! Passive monitor: all state and output come from the existing worker.
use crate::{coordinator, terminal_status, worker_client as api};
use std::{
    collections::HashMap,
    io::{IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};
use terminal_status::State;

type Address = (String, String);

#[derive(Default)]
struct Tasks(Vec<tokio::task::AbortHandle>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

// Discover by name only: a passive client never opens canvas contents.
pub fn discover() -> Result<PathBuf, String> {
    let mut paths: Vec<_> = std::fs::read_dir(".")
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|path| meshfox_core::mdcanvas::is_canvas_path(path))
        .collect();
    paths.sort();
    match paths.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err("no .canvas.md file found; pass --canvas explicitly".into()),
        _ => Err("multiple canvases found; pass --canvas explicitly".into()),
    }
}

fn output(addr: &Address, text: &str, stderr: bool) {
    let color = if stderr {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    } && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && std::env::var_os("TERM").is_none_or(|v| v != "dumb");
    let prefix = format!(
        "[{}::{}{}]",
        addr.0,
        addr.1,
        if stderr { " stderr" } else { "" }
    );
    for line in text.lines() {
        let rendered = if color {
            format!(
                "\x1b[{}m{prefix}\x1b[0m {line}",
                if stderr { "31" } else { "36" }
            )
        } else {
            format!("{prefix} {line}")
        };
        if stderr {
            eprintln!("{rendered}");
        } else {
            println!("{rendered}");
        }
    }
    let _ = std::io::stdout().flush();
}

pub async fn run(path: PathBuf, mode: terminal_status::Mode) -> Result<(), String> {
    tokio::select! {
        result = monitor(path, mode) => result,
        signal = tokio::signal::ctrl_c() => signal.map_err(|e| e.to_string()),
    }
}

async fn monitor(path: PathBuf, mode: terminal_status::Mode) -> Result<(), String> {
    let mut port = coordinator::get(&path)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no core running for this canvas")?;
    let _status = terminal_status::start_tui(mode);
    eprintln!(
        "Connected to {} (port {port}); Ctrl-C to detach",
        path.display()
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Address, api::SubscribeEvent)>();
    let mut subscriptions: HashMap<Address, (Option<i64>, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let mut cursors: HashMap<Address, (Option<i64>, std::sync::Arc<std::sync::atomic::AtomicU64>)> =
        HashMap::new();
    let mut seen: HashMap<Address, i64> = HashMap::new();
    let mut services: HashMap<Address, (u32, String, Vec<String>)> = HashMap::new();
    let mut result = State::Clear;
    let mut disconnected = false;
    let mut tasks = Tasks::default();
    let mut baseline = true;
    let mut was_active = false;
    let mut watch = api::watch(port);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            Some(event) = watch.recv() => {
                if matches!(event, api::WatchEvent::RunStarted { .. } | api::WatchEvent::RunsChanged) {
                    tick.reset_immediately();
                }
            }
            Some((addr, event)) = rx.recv() => {
                match event {
                    api::SubscribeEvent::Line { stream, text } => {
                        output(&addr, &text, matches!(stream, meshfox_server::stream_exec::OutputStream::Stderr));
                    }
                    api::SubscribeEvent::Done { exit_code } => {
                        eprintln!("[{}::{}] finished (exit {})", addr.0, addr.1,
                            exit_code.map_or("unknown".into(), |c| c.to_string()));
                        if exit_code != Some(0) { result = State::Failure(addr.1); }
                        else if !matches!(result, State::Failure(_)) { result = State::Completed(addr.1); }
                    }
                }
            }
            _ = tick.tick() => {
                // Re-resolve without spawning: a restarted worker may have a new port.
                let found = coordinator::get(&path).await.map_err(|e| e.to_string());
                let new_port = match found {
                    Ok(Some(p)) => p,
                    _ => {
                        if !disconnected { eprintln!("Core unavailable; waiting to reconnect…"); }
                        disconnected = true;
                        terminal_status::set(State::Waiting("core connection".into()));
                        continue;
                    }
                };
                if new_port != port {
                    for (addr, (_, task)) in subscriptions.drain() {
                        if !task.is_finished() { seen.remove(&addr); }
                        task.abort();
                    }
                    port = new_port;
                    watch = api::watch(port);
                }
                let (runs, current_services) = tokio::join!(api::list_active_runs(port), api::list_services(port));
                let (Ok(runs), Ok(current_services)) = (runs, current_services) else {
                    if !disconnected { eprintln!("Connection lost; reconnecting…"); }
                    disconnected = true;
                    terminal_status::set(State::Waiting("core connection".into()));
                    continue;
                };
                if disconnected { eprintln!("Reconnected (port {port})"); disconnected = false; }
                let active: Vec<_> = runs.iter().filter(|r| r.kind == "plain" && r.status == "running").collect();
                let idle_before = !was_active;
                if idle_before && !active.is_empty() { result = State::Clear; }
                was_active = !active.is_empty();
                for run in runs.iter().filter(|r| r.kind == "plain") {
                    let addr = (run.node_id.clone(), run.block.clone());
                    let subscribed = subscriptions.get(&addr).is_some_and(|(id, task)| *id == run.history_id && !task.is_finished());
                    let new = run.history_id.is_some_and(|id| seen.get(&addr) != Some(&id));
                    if subscribed || (!new && run.status != "running") { continue; }
                    // At first attach, only replay runs still executing.
                    if baseline && run.status != "running" {
                        if let Some(id) = run.history_id { seen.insert(addr, id); }
                        continue;
                    }
                    if idle_before && new { result = State::Clear; }
                    if let Some((_, old)) = subscriptions.remove(&addr) { old.abort(); }
                    if let Some(id) = run.history_id { seen.insert(addr.clone(), id); }
                    eprintln!("[{}::{}] {}", addr.0, addr.1, run.status);
                    let cursor = cursors.entry(addr.clone()).or_insert_with(||
                        (run.history_id, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0))));
                    if cursor.0 != run.history_id {
                        *cursor = (run.history_id, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)));
                    }
                    let mut events = api::subscribe_run_cursor(port, addr.0.clone(), addr.1.clone(), run.history_id, cursor.1.clone());
                    let tx = tx.clone();
                    let task = tokio::spawn(async move {
                        while let Some(event) = events.recv().await {
                            if tx.send((addr.clone(), event)).is_err() { break; }
                        }
                    });
                    tasks.0.retain(|task| !task.is_finished());
                    tasks.0.push(task.abort_handle());
                    subscriptions.insert((run.node_id.clone(), run.block.clone()), (run.history_id, task));
                }
                baseline = false;
                let mut ready = 0;
                let mut failed = None;
                let mut transitioning = Vec::new();
                for service in current_services {
                    let addr = (service.node_id.clone(), service.block.clone());
                    let entry = services.entry(addr).or_insert((service.pid, String::new(), Vec::new()));
                    if entry.0 != service.pid { *entry = (service.pid, String::new(), Vec::new()); }
                    if entry.1 != service.status {
                        eprintln!("[{}::{}] service {} (pid {})", service.node_id, service.block, service.status, service.pid);
                        entry.1 = service.status.clone();
                    }
                    if service.status == "running" { ready += 1; }
                    if matches!(service.status.as_str(), "crashed" | "stop_failed") { failed = Some(service.block.clone()); }
                    if matches!(service.status.as_str(), "starting" | "stopping" | "restarting") {
                        transitioning.push(service.block.clone());
                    }
                    if let Ok(log) = api::get_service_log(port, &service.node_id, &service.block).await {
                        let lines: Vec<_> = log.into_iter().map(|(stream, text)| format!("{stream:?}: {text}")).collect();
                        let overlap = overlap(&entry.2, &lines);
                        for line in &lines[overlap..] {
                            let is_stderr = line.starts_with("Stderr: ");
                            let text = line.split_once(": ").map_or(line.as_str(), |(_, text)| text);
                            output(&(service.node_id.clone(), service.block.clone()), text, is_stderr);
                        }
                        entry.2 = lines;
                    }
                }
                let state = if !active.is_empty() {
                    State::Running(if active.len() == 1 { active[0].block.clone() } else { format!("{} blocks", active.len()) })
                } else if !transitioning.is_empty() {
                    State::Running(transitioning.join(", "))
                } else if let Some(name) = failed { State::Failure(name) }
                else if matches!(result, State::Failure(_)) { result.clone() }
                else if ready > 0 { State::Ready(ready) }
                else { result.clone() };
                terminal_status::set(state);
            }
        }
    }
}

// Service logs are bounded snapshots, so a rolling buffer can lose its prefix.
fn overlap(previous: &[String], current: &[String]) -> usize {
    (0..=previous.len().min(current.len()))
        .rev()
        .find(|&n| previous[previous.len() - n..] == current[..n])
        .unwrap_or(0)
}
