//! Native, worker-to-worker canvas applications. No shell or nested CLI owns
//! input or terminal status: preflight fields and output belong to the caller.
use super::*;
use meshfox_core::{canvas_call::CanvasCall, BlockAddr};
use serde_json::{json, Value};

tokio::task_local! { static ANCESTRY: Vec<String>; static LEASE_SCOPE: Option<String>; }
pub(super) fn lease_scope() -> Option<String> {
    LEASE_SCOPE.try_with(Clone::clone).ok().flatten()
}
// Embedded workers cannot outlive a one-shot CLI host. Never start a
// long-lived service there and accidentally orphan it when that host exits.
static EMBEDDED_PORTS: std::sync::OnceLock<Mutex<std::collections::HashSet<u16>>> =
    std::sync::OnceLock::new();
fn embedded_ports() -> &'static Mutex<std::collections::HashSet<u16>> {
    EMBEDDED_PORTS.get_or_init(Default::default)
}
pub(super) fn ancestry() -> Vec<String> {
    ANCESTRY.try_with(Clone::clone).unwrap_or_default()
}
fn error(message: impl ToString) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.to_string())
}
pub(super) fn prefix(addr: &BlockAddr) -> String {
    format!("@canvas[{}:{}]/", addr.node_id, addr.block_name)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Preparation {
    #[serde(default)]
    pub fields: Vec<Value>,
    #[serde(default)]
    pub confirmation: Vec<String>,
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub services: bool,
}
#[derive(Clone)]
pub(super) struct PreparedCall {
    port: u16,
    canvas: PathBuf,
    local: bool,
    spec: CanvasCall,
    values: HashMap<String, String>,
    ancestry: Vec<String>,
    pub prepared: Preparation,
}
#[derive(Serialize, Deserialize)]
pub(super) struct PrepareRequest {
    path: Vec<String>,
    block: String,
    #[serde(default)]
    stop: bool,
    no_deps: bool,
    #[serde(default)]
    values: HashMap<String, String>,
    #[serde(default)]
    ancestry: Vec<String>,
}
#[derive(Serialize, Deserialize)]
pub(super) struct NativeRun {
    request: RunRequest,
    ancestry: Vec<String>,
    #[serde(default)]
    lease_scope: Option<String>,
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
async fn post(
    port: u16,
    route: &str,
    value: &impl Serialize,
) -> Result<reqwest::Response, ApiError> {
    let response = client()
        .post(format!("http://127.0.0.1:{port}/api/canvas-call/{route}"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(value).map_err(error)?)
        .send()
        .await
        .map_err(error)?;
    if !response.status().is_success() {
        return Err(error(response.text().await.map_err(error)?));
    }
    Ok(response)
}

// The ordinary coordinator decides worker ownership. A standalone embedded
// TUI/run has no watcher: acquire the same discovery lock and serve a quiet
// worker on a separate runtime, without depending on a possibly stale PATH.
async fn port(state: &AppState, path: &std::path::Path) -> Result<u16, ApiError> {
    if let Some(socket) = &state.watcher_socket {
        return watcher_protocol::request_port(socket, path)
            .await
            .map_err(error);
    }
    let path = path.to_owned();
    let acquired = tokio::task::spawn_blocking({
        let path = path.clone();
        move || worker_lock::try_acquire(&path)
    })
    .await
    .map_err(error)?
    .map_err(error)?;
    match acquired {
        worker_lock::Acquired::Other { port } => Ok(port),
        worker_lock::Acquired::Us(guard) => {
            let (tx, rx) = oneshot::channel();
            std::thread::Builder::new()
                .name("meshfox-canvas-worker".into())
                .spawn(move || {
                    if let Ok(runtime) = tokio::runtime::Runtime::new() {
                        let _ = runtime.block_on(serve_as_worker(
                            path,
                            0,
                            false,
                            None,
                            true,
                            guard,
                            Some(tx),
                        ));
                    }
                })
                .map_err(error)?;
            let port = tokio::time::timeout(Duration::from_secs(15), rx)
                .await
                .map_err(error)?
                .map_err(error)?;
            embedded_ports().lock().unwrap().insert(port);
            Ok(port)
        }
    }
}

pub(super) async fn plan(
    state: &Arc<AppState>,
    canvas: &Canvas,
    chain: &[BlockAddr],
    values: &HashMap<String, String>,
    ancestors: &[String],
) -> Result<HashMap<BlockAddr, PreparedCall>, ApiError> {
    let mut calls = HashMap::new();
    let mut expanded = chain.to_vec();
    expanded.extend(meshfox_core::deps::cleanup_blocks(canvas, chain).map_err(error)?);
    for addr in &expanded {
        if calls.contains_key(addr) {
            continue;
        }
        let block = meshfox_core::deps::find_block(canvas, addr).map_err(error)?;
        if block.lang != "meshfox" {
            continue;
        }
        let spec = CanvasCall::parse(&block.code).map_err(error)?;
        let node = canvas
            .node(&addr.node_id)
            .ok_or_else(|| error("canvas call node disappeared"))?;
        let target = match &spec.canvas {
            Some(path) => node.cwd(canvas_root_dir(&state.canvas_path)).join(path),
            None => state.canvas_path.clone(),
        }
        .canonicalize()
        .map_err(error)?;
        if !meshfox_core::mdcanvas::is_canvas(&target) {
            return Err(error(format!("{} is not a canvas", target.display())));
        }
        let identity = format!(
            "{}:{}",
            state.canvas_path.canonicalize().map_err(error)?.display(),
            prefix(addr)
        );
        if ancestors.len() >= 32 || ancestors.contains(&identity) {
            return Err(error(format!(
                "canvas call cycle/depth limit: {} -> {identity}",
                ancestors.join(" -> ")
            )));
        }
        let mut ancestry = ancestors.to_vec();
        ancestry.push(identity);
        let port = port(state, &target).await?;
        let mut supplied = spec.set.clone();
        let namespace = prefix(addr);
        supplied.extend(values.iter().filter_map(|(name, value)| {
            name.strip_prefix(&namespace)
                .map(|name| (name.into(), value.clone()))
        }));
        let (path, block) = spec.address().map_err(error)?;
        let mut prepared: Preparation = serde_json::from_slice(
            &post(
                port,
                "prepare",
                &PrepareRequest {
                    path,
                    block,
                    stop: spec.stop.is_some(),
                    no_deps: spec.no_deps,
                    values: supplied.clone(),
                    ancestry: ancestry.clone(),
                },
            )
            .await?
            .bytes()
            .await
            .map_err(error)?,
        )
        .map_err(error)?;
        for gate in &mut prepared.confirmation {
            *gate = format!("{} → {gate}", target.display());
        }
        if prepared.services && embedded_ports().lock().unwrap().contains(&port) {
            return Err(error("a canvas call that starts services needs an independently running child worker; open that canvas with meshfox view or a coordinator first"));
        }
        calls.insert(
            addr.clone(),
            PreparedCall {
                port,
                local: target == state.canvas_path.canonicalize().map_err(error)?,
                canvas: target,
                spec,
                values: supplied,
                ancestry,
                prepared,
            },
        );
    }
    Ok(calls)
}

pub(super) async fn prepare(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PrepareRequest>,
) -> Result<Json<Preparation>, ApiError> {
    if req.stop {
        let (_, block) = stop_target(&state, &req.path, &req.block)?;
        return Ok(Json(Preparation {
            path: req.path,
            block: Some(req.block.clone()),
            confirmation: if block.requires_confirmation() {
                vec![req.block]
            } else {
                vec![]
            },
            ..Default::default()
        }));
    }
    let response = prepare_arguments_local(
        State(Arc::clone(&state)),
        Json(ArgumentsRequest {
            path: req.path,
            block: req.block,
            no_deps: req.no_deps,
            args: Default::default(),
        }),
    )
    .await?
    .0;
    if response.tty {
        return Err(error("a meshfox canvas call cannot relay a tty application; run that application directly in its canvas"));
    }
    if response.prepared.block.is_none() {
        return Err(error("canvas call requires bound application arguments; provide them in run, e.g. run: 'build[MODE=release]'"));
    }
    let mut result = Preparation {
        path: response.path,
        block: response.prepared.block,
        confirmation: response.confirmation,
        ..Default::default()
    };
    if let Some(block) = &result.block {
        let fields = get_vars_local(
            State(Arc::clone(&state)),
            Query(VarsQuery {
                path: result.path.join(","),
                block: block.clone(),
                no_deps: req.no_deps,
            }),
            &req.values,
        )
        .await?
        .0;
        for field in fields {
            result
                .fields
                .push(serde_json::to_value(field).map_err(error)?);
        }
        let raw = state.raw.lock().unwrap().clone();
        let mut canvas = resolved_canvas(&raw, &state.canvas_path)?;
        let declarations = meshfox_core::declared_vars(&canvas).map_err(error)?;
        for name in req
            .values
            .keys()
            .filter(|name| !name.starts_with("@canvas["))
        {
            if !declarations.iter().any(|d| &d.name == name) {
                return Err(error(format!("unknown child variable {name:?}")));
            }
        }
        validate_var_overrides(&declarations, &req.values)?;
        let path: Vec<_> = result.path.iter().map(String::as_str).collect();
        let chain = prepare_run_plan(
            &state,
            &mut canvas,
            &path,
            block,
            !req.no_deps,
            &effective_overrides(&state, &req.values),
        )?
        .chain;
        let leased = meshfox_core::deps::lease_targets(&canvas, &chain).map_err(error)?;
        result.services = chain.iter().any(|addr| {
            !leased.contains(addr)
                && meshfox_core::deps::find_block(&canvas, addr).is_ok_and(|block| block.service)
        });
        let calls = Box::pin(plan(&state, &canvas, &chain, &req.values, &req.ancestry)).await?;
        for (addr, call) in calls {
            result.confirmation.extend(call.prepared.confirmation);
            for mut field in call.prepared.fields {
                field["name"] = json!(format!(
                    "{}{}",
                    prefix(&addr),
                    field["name"].as_str().unwrap_or_default()
                ));
                result.fields.push(field);
            }
        }
    }
    Ok(Json(result))
}

fn stop_target(
    state: &AppState,
    path: &[String],
    block: &str,
) -> Result<(BlockAddr, meshfox_core::CodeBlock), ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = resolved_canvas(&raw, &state.canvas_path)?;
    let path: Vec<_> = path.iter().map(String::as_str).collect();
    let addr = meshfox_core::resolve_target(&canvas, &path, block).map_err(error)?;
    let block = meshfox_core::deps::find_block(&canvas, &addr).map_err(error)?;
    if !block.service {
        return Err(error("stop target must be a service"));
    }
    Ok((addr, block))
}

pub(super) async fn stop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NativeRun>,
) -> Result<StatusCode, ApiError> {
    let (addr, block) = stop_target(&state, &req.request.path, &req.request.block)?;
    if block.requires_confirmation() && !req.request.confirm {
        return Err(error("service stop requires confirmation"));
    }
    let handle = state
        .services
        .lock()
        .unwrap()
        .get(&(addr.node_id, addr.block_name))
        .cloned();
    if let Some(handle) = handle {
        handle.stop().await.map_err(error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn admit(
    calls: &HashMap<BlockAddr, PreparedCall>,
    confirm: bool,
) -> Result<(), ApiError> {
    for (addr, call) in calls {
        let missing: Vec<_> = call
            .prepared
            .fields
            .iter()
            .filter(|field| field["resolved"] != true)
            .filter_map(|field| field["name"].as_str())
            .collect();
        if call.prepared.block.is_none() || !missing.is_empty() {
            return Err(error(format!(
                "canvas call {} requires input: {}",
                addr.block_name,
                missing.join(", ")
            )));
        }
        if !confirm && !call.prepared.confirmation.is_empty() {
            return Err(error(format!(
                "canvas call {} requires confirmation: {}",
                addr.block_name,
                call.prepared.confirmation.join(", ")
            )));
        }
    }
    Ok(())
}

pub(super) async fn run(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NativeRun>,
) -> Result<Response, ApiError> {
    // The preparation's ancestry is captured by run_block_impl before it builds
    // its lazy response stream; child calls then carry it over the next hop.
    LEASE_SCOPE
        .scope(
            req.lease_scope,
            ANCESTRY.scope(req.ancestry, Box::pin(run_block_impl(state, req.request))),
        )
        .await
}

pub(super) fn attach(
    handle: &Arc<run_registry::RunHandle>,
    call: PreparedCall,
    confirm: bool,
    persist: bool,
    save_secrets: &std::collections::HashSet<String>,
    row: Option<(run_ledger::RunLedger, i64)>,
    scope: Option<Arc<lifecycle::LeaseScope>>,
) {
    let namespace = prefix(&BlockAddr::new(&handle.node_id, &handle.block_name));
    let save_secrets = save_secrets
        .iter()
        .filter_map(|name| name.strip_prefix(&namespace).map(str::to_string))
        .collect();
    run_registry::attach_native(handle, row, move |handle, mut kill| async move {
        let request = RunRequest {
            path: call.prepared.path.clone(),
            block: call.prepared.block.clone().unwrap(),
            confirm,
            persist,
            no_deps: call.spec.no_deps,
            fresh: call.spec.fresh,
            skip_fresh: call.spec.skip_fresh,
            vars: call
                .values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            save_secrets,
        };
        if !call.local {
            if let Some(scope) = &scope {
                scope.child(call.port);
            }
        }
        let result = drive(
            &handle,
            &call,
            request,
            &mut kill,
            scope.as_ref().map(|s| s.id.clone()),
        )
        .await;
        match result {
            Ok(outcome) => outcome,
            Err(e) => {
                handle.publish_child(
                    stream_exec::OutputStream::Stderr,
                    e.1,
                    json!({"canvas":call.canvas,"type":"error"}),
                );
                run_registry::RunOutcome::Exited { exit_code: 1 }
            }
        }
    });
}

async fn drive(
    handle: &run_registry::RunHandle,
    call: &PreparedCall,
    request: RunRequest,
    kill: &mut oneshot::Receiver<()>,
    lease_scope: Option<String>,
) -> Result<run_registry::RunOutcome, ApiError> {
    if call.spec.stop.is_some() {
        let request = NativeRun {
            request,
            ancestry: call.ancestry.clone(),
            lease_scope,
        };
        tokio::select! {
            result = post(call.port, "stop", &request) => { result?; },
            _ = &mut *kill => return Ok(run_registry::RunOutcome::Killed),
        }
        return Ok(run_registry::RunOutcome::Exited { exit_code: 0 });
    }
    let mut response = post(
        call.port,
        "run",
        &NativeRun {
            request,
            ancestry: call.ancestry.clone(),
            lease_scope,
        },
    )
    .await?;
    let mut buffer = Vec::new();
    let mut run_id = None;
    let mut cancelling = false;
    let mut error_seen = false;
    loop {
        tokio::select! {
            chunk = response.chunk() => {
                let Some(chunk) = chunk.map_err(error)? else { return Err(error("child worker disconnected before Done")); };
                buffer.extend_from_slice(&chunk);
                if buffer.len() > 8 * 1024 * 1024 { return Err(error("child event exceeds 8 MiB")); }
                while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                    let line: Vec<_> = buffer.drain(..=end).collect();
                    let event: Value = serde_json::from_slice(&line).map_err(error)?;
                    let kind = event["type"].as_str().unwrap_or_default();
                    if kind == "error" { error_seen = true; }
                    if kind == "started" {
                        run_id = event["runId"].as_str().map(str::to_owned);
                        if cancelling {
                            if let Some(id) = &run_id { cancel(call.port, id).await?; }
                        }
                    }
                    if kind == "done" {
                        return Ok(if cancelling { run_registry::RunOutcome::Killed } else {
                            run_registry::RunOutcome::Exited { exit_code: if error_seen { 1 } else { event["exitCode"].as_i64().unwrap_or(1) as i32 } }
                        });
                    }
                    let block = event["block"].as_str().unwrap_or_default();
                    let text = match kind {
                        "step-start" => format!("==> {} → {block}", call.canvas.display()),
                        "step-end" => format!("<== {block} (exit {})", event["exitCode"]),
                        "step-skipped" => format!("==> {block} (skipped)"),
                        "output" => event["text"].as_str().unwrap_or_default().to_string(),
                        "service-started" => format!("==> {block} (service started)"),
                        "error" => format!("child error: {}", event["message"].as_str().unwrap_or_default()),
                        "lock-conflict" => return Err(error(format!("child application {block} is already running"))),
                        "killed" => return Ok(run_registry::RunOutcome::Killed),
                        _ => continue,
                    };
                    let stream = if event["stream"] == "stderr" || kind == "error" { stream_exec::OutputStream::Stderr } else { stream_exec::OutputStream::Stdout };
                    handle.publish_child(stream, text, json!({"canvas":call.canvas,"port":call.port,"event":event}));
                }
            }
            _ = &mut *kill, if !cancelling => {
                cancelling = true;
                if let Some(id) = &run_id { cancel(call.port, id).await?; }
            }
        }
    }
}

async fn cancel(port: u16, run_id: &str) -> Result<(), ApiError> {
    client()
        .post(format!("http://127.0.0.1:{port}/api/kill"))
        .timeout(Duration::from_secs(5))
        .header("content-type", "application/json")
        .body(json!({"runId":run_id}).to_string())
        .send()
        .await
        .map_err(error)?;
    Ok(())
}
