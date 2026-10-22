//! Run-scoped cleanup. Cleanup has its own deadline and is never cancelled by
//! the foreground run's cancellation signal.
use super::*;

#[derive(Clone)]
enum LeaseEntry {
    Service(meshfox_core::BlockAddr, services::ServiceHandle),
    Child(u16),
}
pub(super) struct LeaseScope {
    pub id: String,
    entries: Mutex<Vec<LeaseEntry>>,
    releasing: AtomicBool,
}
impl LeaseScope {
    pub fn new(id: String) -> Self {
        Self {
            id,
            entries: Mutex::new(Vec::new()),
            releasing: AtomicBool::new(false),
        }
    }
    pub fn has_leases(&self) -> bool {
        !self.entries.lock().unwrap().is_empty()
    }
    pub fn insert(&self, addr: meshfox_core::BlockAddr, service: services::ServiceHandle) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries
            .iter_mut()
            .find(|e| matches!(e, LeaseEntry::Service(a, _) if a == &addr))
        {
            *entry = LeaseEntry::Service(addr, service);
        } else {
            entries.push(LeaseEntry::Service(addr, service));
        }
    }
    pub fn child(&self, port: u16) {
        let mut entries = self.entries.lock().unwrap();
        if !entries
            .iter()
            .any(|e| matches!(e, LeaseEntry::Child(p) if *p == port))
        {
            entries.push(LeaseEntry::Child(port));
        }
    }
}
#[derive(Deserialize, Serialize)]
pub(super) struct ReleaseRequest {
    scope: String,
}

pub(super) async fn release(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReleaseRequest>,
) -> Json<Vec<String>> {
    Json(release_scope(&state, &req.scope).await)
}

pub(super) async fn release_scope(state: &AppState, id: &str) -> Vec<String> {
    // Keep the worker live during graceful release. Nested calls can return
    // to a worker through a different application; mark release before crossing
    // workers so a cycle does not wait for itself.
    let scope = state.lease_scopes.lock().unwrap().get(id).cloned();
    let Some(scope) = scope else {
        return Vec::new();
    };
    if scope.releasing.swap(true, Ordering::SeqCst) {
        return Vec::new();
    }
    let entries = scope.entries.lock().unwrap().clone();
    let mut errors = Vec::new();
    for entry in entries.into_iter().rev() {
        let port = match entry {
            LeaseEntry::Service(_, service) => {
                if let Err(e) = service.release_lease(id).await {
                    errors.push(format!("lease cleanup: {e}"));
                }
                continue;
            }
            LeaseEntry::Child(port) => port,
        };
        let response = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{port}/api/canvas-call/release"))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&ReleaseRequest { scope: id.into() }).unwrap())
            .send()
            .await;
        match response {
            Ok(response) => match response.bytes().await {
                Ok(bytes) => match serde_json::from_slice::<Vec<String>>(&bytes) {
                    Ok(messages) => errors.extend(messages),
                    Err(e) => errors.push(format!("child lease cleanup: {e}")),
                },
                Err(e) => errors.push(format!("child lease cleanup: {e}")),
            },
            Err(e) => errors.push(format!("child lease cleanup: {e}")),
        }
    }
    state.lease_scopes.lock().unwrap().remove(id);
    errors
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn cleanup(
    state: &Arc<AppState>,
    canvas: &Canvas,
    addr: &meshfox_core::BlockAddr,
    values: &HashMap<String, String>,
    mut environment: HashMap<String, String>,
    call: Option<canvas_call::PreparedCall>,
    confirm: bool,
    persist: bool,
    save_secrets: &std::collections::HashSet<String>,
    scope: Option<Arc<LeaseScope>>,
    reservation: Option<i64>,
) -> Result<(i32, Vec<run_registry::SeqLine>), String> {
    let mut block = meshfox_core::deps::find_block(canvas, addr).map_err(|e| e.to_string())?;
    environment.remove(meshfox_core::VARS_OUT_ENV);
    environment.extend(meshfox_core::args::block_env(&block, values));
    if block.interpreter.is_some() {
        block.interpreter = meshfox_core::args::resolve_interpreter(&block, values);
    }
    let row = match reservation {
        Some(row) => row,
        None => state
            .run_ledger
            .start(
                &addr.node_id,
                &addr.block_name,
                run_ledger::RunKind::Plain,
                "defer",
                std::process::id(),
            )
            .map_err(|e| format!("{e:?}"))?,
    };
    let row = Some((state.run_ledger.clone(), row));
    let handle = if block.lang == "meshfox" {
        let Some(call) = call else {
            if let Some((ledger, row)) = row {
                let _ = ledger.discard_reservation(row);
            }
            return Err("cleanup application changed after preparation".into());
        };
        let handle = run_registry::reserve(addr.node_id.clone(), addr.block_name.clone());
        canvas_call::attach(&handle, call, confirm, persist, save_secrets, row, scope);
        handle
    } else {
        let cwd = canvas
            .node(&addr.node_id)
            .unwrap()
            .cwd(canvas_root_dir(&state.canvas_path));
        match stream_exec::spawn_block(&block, &environment, Some(&cwd), Some(&state.canvas_path)) {
            Ok(process) => {
                if let Some((ledger, row)) = &row {
                    let _ = ledger.update_pid(*row, process.child.id().unwrap_or(0));
                }
                run_registry::track(addr.node_id.clone(), addr.block_name.clone(), process, row)
            }
            Err(e) => {
                if let Some((ledger, row)) = row {
                    let _ = ledger.discard_reservation(row);
                }
                return Err(e.to_string());
            }
        }
    };
    state.runs_registry.lock().unwrap().insert(
        (addr.node_id.clone(), addr.block_name.clone()),
        handle.clone(),
    );
    let (_, mut events) = handle.subscribe_from(0);
    let wait = async {
        loop {
            if handle.outcome() != run_registry::RunOutcome::Running {
                return handle.outcome();
            }
            match events.recv().await {
                Ok(run_registry::RunEvent::Done(outcome)) => return outcome,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return handle.outcome(),
                _ => {}
            }
        }
    };
    let outcome = match tokio::time::timeout(Duration::from_secs(30), wait).await {
        Ok(outcome) => outcome,
        Err(_) => {
            handle.kill();
            // Keep the run and its leases alive until the cleanup process has
            // actually exited; the registry owns process termination.
            while handle.outcome() == run_registry::RunOutcome::Running {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            return Err("timed out after 30s".into());
        }
    };
    let (lines, _) = handle.subscribe_from(0);
    let code = match outcome {
        run_registry::RunOutcome::Exited { exit_code } => exit_code,
        _ => 1,
    };
    Ok((code, lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Instant;

    async fn fixture(body: &str) -> Arc<AppState> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("mfx-lifecycle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(
            &path,
            format!("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{body}"),
        )
        .unwrap();
        build_state_with(path, false, None, Capabilities::UNRESTRICTED)
            .await
            .unwrap()
    }
    fn request(block: &str) -> RunRequest {
        RunRequest {
            source: "cleanup".into(),
            path: vec![],
            block: block.into(),
            no_deps: false,
            fresh: false,
            skip_fresh: false,
            vars: HashMap::new(),
            confirm: false,
            persist: false,
            save_secrets: Default::default(),
        }
    }
    async fn run(state: &Arc<AppState>, block: &str) -> Vec<serde_json::Value> {
        let response = run_block_impl(state.clone(), request(block)).await.unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(15),
            axum::body::to_bytes(response.into_body(), 1024 * 1024),
        )
        .await
        .unwrap()
        .unwrap();
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn handle(state: &AppState) -> services::ServiceHandle {
        state
            .services
            .lock()
            .unwrap()
            .get(&("root".into(), "db".into()))
            .unwrap()
            .clone()
    }
    async fn serve(state: &Arc<AppState>) -> (worker_lock::LockGuard, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker_lock::Acquired::Us(mut guard) =
            worker_lock::try_acquire(&state.canvas_path).unwrap()
        else {
            panic!("fixture locked");
        };
        guard
            .write_port(listener.local_addr().unwrap().port())
            .unwrap();
        let app = build_app(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (guard, task)
    }

    #[tokio::test]
    async fn local_meshfox_run_and_idempotent_stop() {
        let state = fixture("```bash name=db service\nsleep 60\n```\n```meshfox name=start\nrun: db\n```\n```meshfox name=stop\nstop: db\n```").await;
        let (_guard, task) = serve(&state).await;
        let events = run(&state, "start").await;
        assert_eq!(events.last().unwrap()["exitCode"], 0, "{events:?}");
        assert!(handle(&state).is_live());
        for _ in 0..2 {
            let events = run(&state, "stop").await;
            assert_eq!(events.last().unwrap()["exitCode"], 0, "{events:?}");
        }
        assert!(!handle(&state).is_live());
        task.abort();
    }

    #[tokio::test]
    async fn nested_canvas_lease_survives_until_parent_cleanup() {
        let child = fixture("```yaml name=db interpreter=@service\nstart: sleep 60\nstop: touch stopped\nstop_timeout: 100ms\n```\n```bash name=work lease=db\necho child\n```").await;
        let (_child_guard, child_task) = serve(&child).await;
        let stopped = child.canvas_path.parent().unwrap().join("stopped");
        let parent = fixture(&format!("```meshfox name=invoke\ncanvas: {}\nrun: work\n```\n```bash name=work deps=invoke defer=cleanup\ntest ! -f '{}'\n```\n```bash name=cleanup\ntest ! -f '{}'; echo parent-cleanup\n```", child.canvas_path.display(), stopped.display(), stopped.display())).await;
        let events = run(&parent, "work").await;
        assert_eq!(events.last().unwrap()["exitCode"], 0, "{events:?}");
        assert!(stopped.exists());
        assert!(!handle(&child).is_live());
        assert!(parent.lease_scopes.lock().unwrap().is_empty());
        assert!(child.lease_scopes.lock().unwrap().is_empty());
        child_task.abort();
    }

    #[tokio::test]
    async fn cancellation_runs_cleanup_before_terminal_event() {
        let state = fixture("```bash name=work defer=cleanup\ntouch running; sleep 60\n```\n```bash name=cleanup\ntouch cleaned; echo cleaned\n```").await;
        let response = run_block_impl(state.clone(), request("work"))
            .await
            .unwrap();
        let body = tokio::spawn(async move {
            axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
        });
        let dir = state.canvas_path.parent().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.join("running").exists() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let run_id = state.runs.lock().unwrap().keys().next().unwrap().clone();
        kill_run(
            State(state.clone()),
            Json(KillRequest {
                run_id: Some(run_id),
                node_id: None,
                block: None,
            }),
        )
        .await;
        let bytes = tokio::time::timeout(Duration::from_secs(5), body)
            .await
            .unwrap()
            .unwrap();
        let events: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert!(dir.join("cleaned").exists());
        assert_eq!(events.last().unwrap()["type"], "killed", "{events:?}");
    }

    const DB: &str = "```yaml name=db interpreter=@service\nstart: sleep 60\nready: test -f ready\nstart_timeout: 2s\nstop_timeout: 50ms\n```\n";

    #[tokio::test]
    async fn lease_survives_checker_and_cleanup_then_stops() {
        let state = fixture(&format!("{DB}\n```bash name=checker lease=db\ntest -f ready\n```\n```bash name=work deps=checker defer=cleanup\necho work\n```\n```bash name=cleanup\necho cleanup\n```\n")).await;
        std::fs::write(state.canvas_path.parent().unwrap().join("ready"), "").unwrap();
        let events = run(&state, "work").await;
        assert_eq!(events.last().unwrap()["exitCode"], 0, "{events:?}");
        let output: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "output")
            .map(|e| e["text"].as_str().unwrap())
            .collect();
        assert_eq!(output, ["work", "cleanup"]);
        assert!(!handle(&state).is_live());
    }

    #[tokio::test]
    async fn lease_does_not_stop_a_preexisting_ordinary_service() {
        let state = fixture(
            "```bash name=db service\nsleep 60\n```\n```bash name=work lease=db\necho work\n```",
        )
        .await;
        run(&state, "db").await;
        run(&state, "work").await;
        assert!(handle(&state).is_live());
        handle(&state).stop().await.unwrap();
    }

    #[tokio::test]
    async fn last_lease_stops_and_duplicate_release_is_harmless() {
        let state = fixture("```bash name=db service\nsleep 60\n```").await;
        run(&state, "db").await;
        let service = handle(&state);
        service.acquire_lease("a", true).await.unwrap();
        service.acquire_lease("b", false).await.unwrap();
        service.release_lease("a").await.unwrap();
        service.release_lease("a").await.unwrap();
        assert!(service.is_live());
        service.release_lease("b").await.unwrap();
        assert!(!service.is_live());
    }

    #[tokio::test]
    async fn readiness_timeout_blocks_consumers_and_cleans_up() {
        let state = fixture("```yaml name=db interpreter=@service\nstart: sleep 60\nready: sleep 60\nstart_timeout: 100ms\nstop_timeout: 50ms\n```\n```bash name=work lease=db\ntouch consumer\n```").await;
        let events = run(&state, "work").await;
        assert!(events.iter().any(|e| e["type"] == "error"), "{events:?}");
        assert!(!state
            .canvas_path
            .parent()
            .unwrap()
            .join("consumer")
            .exists());
        assert!(!handle(&state).is_live());
    }

    #[tokio::test]
    async fn defers_are_lifo_and_all_run_after_failure() {
        let state = fixture("```bash name=prepare defer=first\necho prepare\n```\n```bash name=work deps=prepare defer=second\nexit 7\n```\n```bash name=first\necho first\n```\n```bash name=second\necho second; exit 3\n```").await;
        let events = run(&state, "work").await;
        let output: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "output")
            .map(|e| e["text"].as_str().unwrap())
            .collect();
        assert_eq!(output, ["prepare", "second", "first"]);
        assert_eq!(events.last().unwrap()["exitCode"], 7, "{events:?}");
    }

    #[tokio::test]
    async fn skipped_blocks_do_not_register_cleanup() {
        let state = fixture("```bash name=prepare defer=cleanup\necho prepare\n```\n```bash name=work deps=prepare\necho work\n```\n```bash name=cleanup\necho cleanup\n```").await;
        run(&state, "work").await;
        let events = run(&state, "work").await;
        assert!(events.iter().any(|e| e["type"] == "step-skipped"));
        assert!(!events.iter().any(|e| e["text"] == "cleanup"));
    }

    #[tokio::test]
    async fn autostop_waits_for_cleanup_but_persistent_services_hold_worker() {
        let state = fixture("```yaml name=db interpreter=@service autostop\nstart: sleep 60\nstop: touch cleaned\nstop_timeout: 100ms\n```").await;
        run(&state, "db").await;
        assert!(handle(&state).autostop());
        assert!(!has_persistent_services(&state));
        assert!(stop_idle_services(&state).await);
        assert!(state.canvas_path.parent().unwrap().join("cleaned").exists());
    }
}
