//! Aggregate one canvas's activity. The enclosing TUI owns terminal output;
//! each nested main loop retains its own history while another canvas is open.
use super::app::App;
use crate::terminal_status::State;
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Revision {
    Worker(u64),
    Local(Instant),
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Completion {
    label: String,
    failed: bool,
    revision: Revision,
}
#[derive(Default)]
struct Snapshot {
    waiting: Option<String>,
    active: BTreeMap<String, String>,
    completed: BTreeMap<String, Completion>,
    services: usize,
    service_failure: Option<String>,
}

impl Snapshot {
    fn from_app(app: &App) -> Self {
        let mut snapshot = Self::default();
        snapshot.waiting = if app.run_confirmation.is_some() {
            Some("confirmation".into())
        } else if let Some(conflict) = &app.service_conflict {
            Some(conflict.block_name.clone())
        } else if app.var_form.is_some() {
            Some(
                app.pending_http_run
                    .as_ref()
                    .map(|r| r.block_name.clone())
                    .unwrap_or_else(|| "configuration".into()),
            )
        } else {
            None
        };
        let key = |node: &str, block: &str| format!("{node}/{block}");
        let is_service = |node: &str, block: &str| {
            app.service_list
                .iter()
                .any(|service| service.node_id == node && service.block == block)
        };
        for service in &app.service_list {
            match service.status.as_str() {
                "running" => snapshot.services += 1,
                "crashed" | "stop_failed" => {
                    snapshot
                        .service_failure
                        .get_or_insert_with(|| service.block.clone());
                }
                "starting" | "restarting" | "stopping" => {
                    snapshot
                        .active
                        .insert(key(&service.node_id, &service.block), service.block.clone());
                }
                _ => {}
            }
        }
        for run in app.block_runs.iter().flatten() {
            // Services have a separate lifecycle, not a permanently busy task.
            if is_service(&run.node_id, &run.block) {
                continue;
            }
            let addr = key(&run.node_id, &run.block);
            if run.status == "running" {
                snapshot.active.insert(addr, run.block.clone());
            } else {
                snapshot.completed.insert(
                    addr,
                    Completion {
                        label: run.block.clone(),
                        failed: run.status == "killed" || run.exit_code != Some(0),
                        revision: Revision::Worker(run.uptime_ms),
                    },
                );
            }
        }
        for (addr, output) in &app.step_output {
            if is_service(&addr.node_id, &addr.block_name) {
                continue;
            }
            let addr_key = key(&addr.node_id, &addr.block_name);
            if output.running {
                snapshot.active.insert(addr_key, addr.block_name.clone());
            } else {
                // Keep ledger running entries until its next refresh: this
                // output may belong to the previous invocation of that block.
                snapshot.completed.insert(
                    addr_key,
                    Completion {
                        label: addr.block_name.clone(),
                        failed: output.exit_code != 0,
                        revision: Revision::Worker(output.duration_ms),
                    },
                );
            }
        }
        for addr in app.external_running.keys() {
            if is_service(&addr.node_id, &addr.block_name) {
                continue;
            }
            snapshot.active.insert(
                key(&addr.node_id, &addr.block_name),
                addr.block_name.clone(),
            );
        }
        if let Some(run) = &app.run {
            let name = run
                .chain
                .last()
                .map(|a| a.block_name.as_str())
                .unwrap_or("preparing");
            if run.is_running() {
                if let Some(addr) = run.current_addr() {
                    let label = app
                        .child_steps
                        .get(addr)
                        .map(|child| format!("{name} → {child}"))
                        .unwrap_or_else(|| name.into());
                    snapshot
                        .active
                        .insert(key(&addr.node_id, &addr.block_name), label);
                } else {
                    snapshot.active.insert("foreground".into(), name.into());
                }
            } else if run.finished {
                snapshot.completed.insert(
                    "foreground".into(),
                    Completion {
                        label: name.into(),
                        failed: run.had_failure || run.killed,
                        revision: Revision::Local(run.step_started),
                    },
                );
            }
        }
        let handoff = app
            .pending_http_tty
            .as_ref()
            .map(|tty| &tty.block_name)
            .or_else(|| {
                app.pending_http_tty_attach
                    .as_ref()
                    .map(|tty| &tty.block_name)
            });
        if let Some(name) = handoff {
            if !snapshot.active.values().any(|active| active == name) {
                snapshot.active.insert("tty-handoff".into(), name.clone());
            }
        }
        snapshot
    }
}

#[derive(Default)]
pub(super) struct CanvasStatus {
    previous: Option<Snapshot>,
    result: Option<State>,
    batch_failure: Option<String>,
}
impl CanvasStatus {
    pub fn update(&mut self, app: &App) {
        let state = self.observe(Snapshot::from_app(app));
        crate::terminal_status::set(state);
    }

    fn observe(&mut self, snapshot: Snapshot) -> State {
        if let Some(previous) = &self.previous {
            let new_completion = snapshot
                .completed
                .iter()
                .any(|(key, completion)| previous.completed.get(key) != Some(completion));
            if previous.active.is_empty() && (!snapshot.active.is_empty() || new_completion) {
                self.result = None;
                self.batch_failure = None;
            }
            for (key, completion) in &snapshot.completed {
                if previous.completed.get(key) == Some(completion) {
                    continue;
                }
                // New completions while idle can be short external runs which
                // finished between polls. Keep a batch's first error even if
                // another parallel task succeeds later.
                if completion.failed {
                    self.batch_failure
                        .get_or_insert_with(|| completion.label.clone());
                }
                self.result = Some(State::Completed(completion.label.clone()));
            }
        }
        let state = if let Some(waiting) = &snapshot.waiting {
            State::Waiting(waiting.clone())
        } else if !snapshot.active.is_empty() {
            State::Running(if snapshot.active.len() == 1 {
                snapshot.active.values().next().unwrap().clone()
            } else {
                format!("{} blocks", snapshot.active.len())
            })
        } else if let Some(failed) = &snapshot.service_failure {
            State::Failure(failed.clone())
        } else if let Some(failed) = &self.batch_failure {
            State::Failure(failed.clone())
        } else if snapshot.services > 0 {
            State::Ready(snapshot.services)
        } else {
            self.result.clone().unwrap_or(State::Clear)
        };
        self.previous = Some(snapshot);
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn running(names: &[&str]) -> Snapshot {
        Snapshot {
            active: names
                .iter()
                .map(|n| (n.to_string(), n.to_string()))
                .collect(),
            ..Default::default()
        }
    }
    fn complete(snapshot: &mut Snapshot, name: &str, failed: bool) {
        snapshot.active.remove(name);
        snapshot.completed.insert(
            name.into(),
            Completion {
                label: name.into(),
                failed,
                revision: Revision::Worker(42),
            },
        );
    }
    #[test]
    fn parallel_failure_survives_later_success_and_navigation_until_next_run() {
        let mut status = CanvasStatus::default();
        assert_eq!(status.observe(Snapshot::default()), State::Clear);
        let mut snapshot = running(&["build", "tests"]);
        assert_eq!(
            status.observe(running(&["build", "tests"])),
            State::Running("2 blocks".into())
        );
        complete(&mut snapshot, "tests", true);
        assert_eq!(
            status.observe(Snapshot {
                active: snapshot.active.clone(),
                completed: snapshot.completed.clone(),
                ..Default::default()
            }),
            State::Running("build".into())
        );
        complete(&mut snapshot, "build", false);
        assert_eq!(status.observe(snapshot), State::Failure("tests".into()));
        assert_eq!(
            status.observe(Snapshot::default()),
            State::Failure("tests".into())
        );
        assert_eq!(
            status.observe(running(&["retry"])),
            State::Running("retry".into())
        );
        let mut done = Snapshot::default();
        complete(&mut done, "retry", false);
        assert_eq!(status.observe(done), State::Completed("retry".into()));
    }
    #[test]
    fn services_are_ready_but_crashes_and_input_take_priority() {
        let mut status = CanvasStatus::default();
        assert_eq!(
            status.observe(Snapshot {
                services: 2,
                ..Default::default()
            }),
            State::Ready(2)
        );
        assert_eq!(
            status.observe(Snapshot {
                services: 1,
                service_failure: Some("server".into()),
                ..Default::default()
            }),
            State::Failure("server".into())
        );
        assert_eq!(
            status.observe(Snapshot {
                waiting: Some("confirmation".into()),
                ..running(&["build"])
            }),
            State::Waiting("confirmation".into())
        );
        assert_eq!(
            status.observe(Snapshot {
                services: 2,
                ..Default::default()
            }),
            State::Ready(2)
        );
    }
    #[test]
    fn historical_results_do_not_mark_a_new_session_as_failed() {
        let mut snapshot = Snapshot::default();
        complete(&mut snapshot, "old", true);
        assert_eq!(CanvasStatus::default().observe(snapshot), State::Clear);
    }
    #[test]
    fn nested_canvas_has_independent_results() {
        let mut parent = CanvasStatus::default();
        parent.observe(running(&["parent"]));
        let mut done = Snapshot::default();
        complete(&mut done, "parent", true);
        assert_eq!(parent.observe(done), State::Failure("parent".into()));
        assert_eq!(
            CanvasStatus::default().observe(Snapshot::default()),
            State::Clear
        );
        assert_eq!(
            parent.observe(Snapshot::default()),
            State::Failure("parent".into())
        );
    }
    #[tokio::test]
    async fn service_snapshots_ignore_the_transcripts_stale_running_marker() {
        let dir = std::env::temp_dir().join(format!("mfx-status-unit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        let addr = meshfox_core::deps::BlockAddr::new("root", "server");
        app.step_output.insert(
            addr,
            super::super::app::StepOutput {
                text: String::new(),
                stdout: String::new(),
                stderr: String::new(),
                output_markdown: false,
                exit_code: 0,
                duration_ms: 0,
                running: true,
            },
        );
        app.service_list.push(crate::worker_client::ServiceDto {
            node_id: "root".into(),
            block: "server".into(),
            status: "running".into(),
            exit_code: None,
            pid: 1,
            uptime_ms: 0,
            cpu_percent: None,
            mem_bytes: None,
        });
        let mut status = CanvasStatus::default();
        assert_eq!(status.observe(Snapshot::from_app(&app)), State::Ready(1));
        app.service_list[0].status = "crashed".into();
        assert_eq!(
            status.observe(Snapshot::from_app(&app)),
            State::Failure("server".into())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_short_run_seen_only_after_completion_starts_a_new_batch() {
        let mut status = CanvasStatus::default();
        status.observe(running(&["failed"]));
        let mut failed = Snapshot::default();
        complete(&mut failed, "failed", true);
        assert_eq!(status.observe(failed), State::Failure("failed".into()));
        let mut success = Snapshot::default();
        complete(&mut success, "quick", false);
        assert_eq!(status.observe(success), State::Completed("quick".into()));
    }
}
