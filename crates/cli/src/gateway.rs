//! The HTTP gateway behind `meshfox serve --http`: one fixed port in front of
//! the per-canvas workers (which stay on `127.0.0.1:<random port>`).
//!
//! ```text
//! /                  index of the canvases under --root (or the token form)
//! /_/ping            liveness, no auth
//! /_/login           POST the token, get the session cookie
//! /_/index.json      the index as JSON
//! /c/<slug>/…        reverse proxy (HTTP + WebSocket) to that canvas's worker
//! ```
//!
//! The gateway knows the coordinator only through [`CanvasResolver`] (path →
//! worker port), so the same module can later front a coordinator it reaches
//! over its unix socket instead of the in-process registry.
//!
//! Security model, in one place:
//! - Everything but `/_/ping` and the login form needs the token, as
//!   `Authorization: Bearer` or the `mfx_session` cookie. No token configured
//!   means open access, which `serve` only allows on a loopback bind. The
//!   token is root-equivalent on the machine (blocks run code), so it is
//!   compared in constant time and failed attempts are rate limited per peer.
//! - The `Host` header must be an allowed name (loopback names on a loopback
//!   bind, so DNS rebinding cannot reach it) and an `Origin`, when present,
//!   must be the page's own authority.
//! - Workers authorise some requests by "Host and Origin are loopback". The
//!   gateway therefore rewrites both to the worker's own address before
//!   forwarding, and strips `Authorization` and `Cookie`: the worker never
//!   sees the credentials, and *this* module is the only access control.
//! - A canvas is reachable only if its slug resolves in the index under the
//!   root, and its real path (symlinks resolved) is still inside the root.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message as AxMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use meshfox_core::canvas_index::{IndexCache, ScanOptions, DEFAULT_MIN_RESCAN};
use meshfox_core::gateway_header::{encode_root, GATEWAY_HEADER_PREFIX, GATEWAY_ROOT_HEADER};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TunCloseFrame;
use tokio_tungstenite::tungstenite::Message as TunMessage;

use crate::gateway_index::{self, Row};

const SESSION_COOKIE: &str = "mfx_session";
const SESSION_CONTEXT: &str = "meshfox gateway session v1";
const LOGIN_BODY_MAX: usize = 8 * 1024;
const FAILED_AUTH_LIMIT: u32 = 10;
const FAILED_AUTH_WINDOW: Duration = Duration::from_secs(60);

pub type PortFuture<'a> = Pin<Box<dyn Future<Output = Result<u16, String>> + Send + 'a>>;

/// What the gateway needs from a coordinator.
pub trait CanvasResolver: Send + Sync + 'static {
    /// Get-or-spawn the worker for this canonical canvas path.
    fn port_for(&self, canvas: PathBuf) -> PortFuture<'_>;
    /// Canonical paths of the canvases that currently have a worker.
    fn running(&self) -> Vec<PathBuf>;
}

pub struct GatewayConfig {
    /// Canonical directory whose canvases are served.
    pub root: PathBuf,
    /// `None`: no authentication (loopback only; enforced by the caller).
    pub token: Option<String>,
    /// Extra `Host` names to accept (without port).
    pub allowed_hosts: Vec<String>,
    /// The listener is bound to a loopback address.
    pub loopback: bool,
    /// A reverse proxy in front terminates TLS and forwards: believe its
    /// `X-Forwarded-For` (the client for rate limiting) and
    /// `X-Forwarded-Proto` (whether the session cookie gets `Secure`).
    /// Off by default, since otherwise any client could claim anything.
    pub trust_proxy: bool,
}

pub struct Gateway {
    cfg: GatewayConfig,
    resolver: Arc<dyn CanvasResolver>,
    index: Arc<IndexCache>,
    client: reqwest::Client,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    session_value: Option<String>,
}

impl Gateway {
    pub fn new(cfg: GatewayConfig, resolver: Arc<dyn CanvasResolver>) -> io::Result<Arc<Self>> {
        let index = Arc::new(IndexCache::new(
            cfg.root.clone(),
            ScanOptions::default(),
            DEFAULT_MIN_RESCAN,
        )?);
        // `no_proxy`: the upstream is always loopback and must never be sent
        // through an HTTP(S)_PROXY from the environment.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .map_err(io::Error::other)?;
        let session_value = cfg.token.as_deref().map(session_value);
        Ok(Arc::new(Self {
            cfg,
            resolver,
            index,
            client,
            failures: Mutex::new(HashMap::new()),
            session_value,
        }))
    }

    pub fn router(self: Arc<Self>) -> Router {
        Router::new().fallback(handle).with_state(self)
    }
}

fn session_value(token: &str) -> String {
    blake3::Hash::from(blake3::derive_key(SESSION_CONTEXT, token.as_bytes()))
        .to_hex()
        .to_string()
}

async fn handle(State(gw): State<Arc<Gateway>>, req: Request) -> Response {
    let socket_peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let peer = client_ip(gw.cfg.trust_proxy, req.headers(), socket_peer);
    gw.dispatch(req, peer).await
}

/// The address failed logins are counted against: the socket peer, unless a
/// trusted proxy is in front, in which case the *last* `X-Forwarded-For`
/// entry — the one that proxy appended itself; earlier ones are whatever the
/// client claimed. Behind a proxy the peer is always the proxy, so counting
/// by it would let anyone's bad guesses lock everybody out.
fn client_ip(
    trust_proxy: bool,
    headers: &HeaderMap,
    socket_peer: Option<IpAddr>,
) -> Option<IpAddr> {
    if trust_proxy {
        let forwarded = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .next_back()
            .and_then(|last| last.trim().parse::<IpAddr>().ok());
        if forwarded.is_some() {
            return forwarded;
        }
    }
    socket_peer
}

enum Auth {
    Ok,
    Missing,
    Bad,
    Limited,
}

impl Gateway {
    async fn dispatch(&self, req: Request, peer: Option<IpAddr>) -> Response {
        let headers = req.headers();
        if !self.host_allowed(headers) {
            return text(StatusCode::FORBIDDEN, "host not allowed");
        }
        if !origin_matches_host(headers) {
            return text(StatusCode::FORBIDDEN, "cross-origin request refused");
        }
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        match (method.as_str(), path.as_str()) {
            ("GET" | "HEAD", "/_/ping") => return text(StatusCode::OK, "ok"),
            ("POST", "/_/login") => return self.login(req, peer).await,
            _ => {}
        }

        match self.authenticate(req.headers(), peer) {
            Auth::Ok => {}
            Auth::Limited => {
                let mut res = text(StatusCode::TOO_MANY_REQUESTS, "too many failed attempts");
                res.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
                return res;
            }
            Auth::Missing => return self.unauthorized(&req, false),
            Auth::Bad => return self.unauthorized(&req, true),
        }

        match path.as_str() {
            "/" if is_read(&method) => self.index_page().await,
            "/_/index.json" if is_read(&method) => self.index_json().await,
            _ => match path.strip_prefix("/c/") {
                Some(rest) => self.canvas(rest, req).await,
                None => text(StatusCode::NOT_FOUND, "not found"),
            },
        }
    }

    // ---- authentication -------------------------------------------------

    fn authenticate(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> Auth {
        let (Some(token), Some(session)) = (&self.cfg.token, &self.session_value) else {
            return Auth::Ok;
        };
        if self.is_limited(peer) {
            return Auth::Limited;
        }
        let presented = bearer(headers)
            .map(|b| Credential::Token(b.to_string()))
            .or_else(|| {
                cookie(headers, SESSION_COOKIE).map(|c| Credential::Session(c.to_string()))
            });
        let ok = match &presented {
            None => return Auth::Missing,
            Some(Credential::Token(t)) => ct_eq(t, token),
            Some(Credential::Session(s)) => ct_eq(s, session),
        };
        if ok {
            Auth::Ok
        } else {
            self.record_failure(peer);
            Auth::Bad
        }
    }

    fn is_limited(&self, peer: Option<IpAddr>) -> bool {
        let Some(peer) = peer else { return false };
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        match failures.get(&peer) {
            Some((count, since)) if since.elapsed() < FAILED_AUTH_WINDOW => {
                *count >= FAILED_AUTH_LIMIT
            }
            Some(_) => {
                failures.remove(&peer);
                false
            }
            None => false,
        }
    }

    fn record_failure(&self, peer: Option<IpAddr>) {
        let Some(peer) = peer else { return };
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let entry = failures.entry(peer).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= FAILED_AUTH_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    fn unauthorized(&self, req: &Request, bad: bool) -> Response {
        let wants_html = req.method() == Method::GET
            && req
                .headers()
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("text/html"));
        if !wants_html {
            let mut res = text(StatusCode::UNAUTHORIZED, "authentication required");
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return res;
        }
        let path = req.uri().path();
        let mut next = path.to_string();
        if let Some(q) = req.uri().query() {
            next.push('?');
            next.push_str(q);
        }
        let action = format!("{}_/login", rel_root(path));
        html(
            StatusCode::UNAUTHORIZED,
            gateway_index::login_html(&action, &next, bad),
        )
    }

    async fn login(&self, req: Request, peer: Option<IpAddr>) -> Response {
        let (Some(token), Some(session)) = (&self.cfg.token, &self.session_value) else {
            return redirect("../");
        };
        if self.is_limited(peer) {
            return text(StatusCode::TOO_MANY_REQUESTS, "too many failed attempts");
        }
        let secure = self.cfg.trust_proxy
            && req
                .headers()
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("https"));
        let Ok(bytes) = axum::body::to_bytes(req.into_body(), LOGIN_BODY_MAX).await else {
            return text(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
        };
        let form: HashMap<String, String> =
            serde_urlencoded::from_bytes(&bytes).unwrap_or_default();
        let next = form
            .get("next")
            .map(String::as_str)
            .filter(|n| safe_next(n))
            .unwrap_or("/");
        if !form.get("token").is_some_and(|t| ct_eq(t, token)) {
            self.record_failure(peer);
            return html(
                StatusCode::UNAUTHORIZED,
                gateway_index::login_html("login", next, true),
            );
        }
        let mut cookie = format!("{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict");
        if secure {
            cookie.push_str("; Secure");
        }
        let mut res = redirect(&format!("../{}", next.trim_start_matches('/')));
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            res.headers_mut().insert(header::SET_COOKIE, value);
        }
        res
    }

    // ---- host / origin --------------------------------------------------

    fn host_allowed(&self, headers: &HeaderMap) -> bool {
        let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let name = host_name(host).to_ascii_lowercase();
        let listed = self
            .cfg
            .allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&name));
        if self.cfg.loopback {
            listed || matches!(name.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        } else {
            listed || self.cfg.allowed_hosts.is_empty()
        }
    }

    // ---- pages ----------------------------------------------------------

    async fn rows(&self) -> Option<(Vec<Row>, bool)> {
        let index = Arc::clone(&self.index);
        let root = self.cfg.root.clone();
        let running: Vec<PathBuf> = self.resolver.running();
        tokio::task::spawn_blocking(move || {
            let snapshot = index.current().ok()?;
            let running: std::collections::HashSet<String> = running
                .iter()
                .filter_map(|p| p.strip_prefix(&root).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .collect();
            let now = SystemTime::now();
            let rows = snapshot
                .entries()
                .iter()
                .map(|e| {
                    let path = e.rel.to_string_lossy().replace('\\', "/");
                    Row {
                        running: running.contains(&path),
                        slug: e.slug.clone(),
                        age_secs: e
                            .mtime
                            .and_then(|m| now.duration_since(m).ok())
                            .map(|d| d.as_secs()),
                        path,
                    }
                })
                .collect::<Vec<_>>();
            Some((rows, snapshot.truncated))
        })
        .await
        .ok()
        .flatten()
    }

    async fn index_page(&self) -> Response {
        match self.rows().await {
            Some((rows, truncated)) => {
                html(StatusCode::OK, gateway_index::index_html(&rows, truncated))
            }
            None => text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot read the served root",
            ),
        }
    }

    async fn index_json(&self) -> Response {
        match self.rows().await {
            Some((rows, truncated)) => {
                let body = gateway_index::index_json(&rows, truncated).to_string();
                let mut res = Response::new(Body::from(body));
                res.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                res
            }
            None => text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot read the served root",
            ),
        }
    }

    // ---- proxy ----------------------------------------------------------

    /// `rest` is everything after `/c/`: `<slug>` or `<slug>/<tail…>`.
    async fn canvas(&self, rest: &str, req: Request) -> Response {
        let (slug, tail) = match rest.split_once('/') {
            Some((slug, tail)) => (slug, Some(tail)),
            None => (rest, None),
        };
        // `/c/<slug>` without the slash would resolve every relative URL of
        // the page against `/c/`; send the browser to the canonical form.
        let Some(tail) = tail else {
            let mut to = format!("{slug}/");
            if let Some(q) = req.uri().query() {
                to.push('?');
                to.push_str(q);
            }
            let mut res = redirect(&to);
            *res.status_mut() = StatusCode::PERMANENT_REDIRECT;
            return res;
        };
        let port = match self.port_for_slug(slug).await {
            Ok(port) => port,
            Err(res) => return res,
        };
        let upstream_path = format!("/{tail}");
        if is_upgrade(req.headers()) {
            proxy_ws(port, upstream_path, &self.cfg.root, req).await
        } else {
            proxy_http(&self.client, port, upstream_path, &self.cfg.root, req).await
        }
    }

    async fn port_for_slug(&self, slug: &str) -> Result<u16, Response> {
        let index = Arc::clone(&self.index);
        let owned = slug.to_string();
        let entry = tokio::task::spawn_blocking(move || index.resolve(&owned))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| text(StatusCode::NOT_FOUND, "no such canvas"))?;
        let path = self.cfg.root.join(&entry.rel);
        // The file may have been swapped for a symlink since the scan.
        let real = path
            .canonicalize()
            .map_err(|_| text(StatusCode::NOT_FOUND, "no such canvas"))?;
        if !real.starts_with(&self.cfg.root) {
            return Err(text(StatusCode::FORBIDDEN, "outside the served root"));
        }
        self.resolver.port_for(real).await.map_err(|e| {
            text(
                StatusCode::BAD_GATEWAY,
                &format!("cannot start the canvas: {e}"),
            )
        })
    }
}

enum Credential {
    Token(String),
    Session(String),
}

fn is_read(m: &Method) -> bool {
    m == Method::GET || m == Method::HEAD
}

fn ct_eq(a: &str, b: &str) -> bool {
    // Hash both sides first so length differences are not observable either.
    blake3::hash(a.as_bytes()) == blake3::hash(b.as_bytes())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(k, v)| (k == name).then_some(v))
}

/// `Host` without its port: `example.com:8080` → `example.com`, `[::1]:80` →
/// `[::1]`.
fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return host.find(']').map_or(host, |end| &host[..=end]);
    }
    host.rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(name, _)| name)
}

/// An `Origin` header, when present, must be this page's own authority.
fn origin_matches_host(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://").map(|(_, authority)| authority))
        .zip(host)
        .is_some_and(|(authority, host)| authority.eq_ignore_ascii_case(host))
}

/// `../` repeated enough to climb from `path` (as requested) to the gateway
/// root, so the pages need no absolute URLs.
fn rel_root(path: &str) -> String {
    let ups = path.matches('/').count().saturating_sub(1);
    if ups == 0 {
        "./".to_string()
    } else {
        "../".repeat(ups)
    }
}

/// A post-login target: an absolute path of this gateway, nothing that could
/// become another origin or smuggle a header.
fn safe_next(next: &str) -> bool {
    next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.chars().any(char::is_control)
}

fn text(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg.to_string(),
    )
        .into_response()
}

fn html(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

fn redirect(to: &str) -> Response {
    let mut res = Response::new(Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(value) = HeaderValue::from_str(to) {
        res.headers_mut().insert(header::LOCATION, value);
    }
    res
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Request headers worth forwarding to a worker. `Host` is set from the
/// upstream URL; `Origin` is rewritten by the caller; credentials and the
/// referrer (which names the gateway) stay behind.
fn upstream_headers(headers: &HeaderMap, port: u16, root: &std::path::Path) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if is_hop_by_hop(name)
            || matches!(
                name.as_str(),
                "host" | "origin" | "cookie" | "authorization" | "referer"
            )
            || name.as_str().starts_with("sec-websocket-")
            // Ours alone: a client must not be able to pose as the gateway.
            || name.as_str().starts_with(GATEWAY_HEADER_PREFIX)
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    if headers.contains_key(header::ORIGIN) {
        if let Ok(value) = HeaderValue::from_str(&format!("http://127.0.0.1:{port}")) {
            out.insert(header::ORIGIN, value);
        }
    }
    set_gateway_root(&mut out, root);
    out
}

/// Tells the worker it is being reached through this gateway and which root
/// it serves (see `meshfox_core::gateway_header`).
fn set_gateway_root(headers: &mut HeaderMap, root: &std::path::Path) {
    if let Ok(value) = HeaderValue::from_str(&encode_root(root)) {
        headers.insert(HeaderName::from_static(GATEWAY_ROOT_HEADER), value);
    }
}

fn upstream_target(port: u16, scheme: &str, path: &str, uri: &Uri) -> String {
    let mut url = format!("{scheme}://127.0.0.1:{port}{path}");
    if let Some(q) = uri.query() {
        url.push('?');
        url.push_str(q);
    }
    url
}

async fn proxy_http(
    client: &reqwest::Client,
    port: u16,
    path: String,
    root: &std::path::Path,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let url = upstream_target(port, "http", &path, &parts.uri);
    let upstream = client
        .request(parts.method.clone(), url)
        .headers(upstream_headers(&parts.headers, port, root));
    // A bodiless GET must not turn into a chunked upload.
    let has_body = parts.headers.contains_key(header::TRANSFER_ENCODING)
        || parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v != "0");
    let upstream = if has_body {
        upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()))
    } else {
        upstream
    }
    .send()
    .await;
    let upstream = match upstream {
        Ok(res) => res,
        Err(e) => return text(StatusCode::BAD_GATEWAY, &format!("worker unreachable: {e}")),
    };
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let mut res = Response::new(Body::from_stream(upstream.bytes_stream()));
    *res.status_mut() = status;
    for (name, value) in &headers {
        if !is_hop_by_hop(name) {
            res.headers_mut().append(name.clone(), value.clone());
        }
    }
    res
}

async fn proxy_ws(port: u16, path: String, root: &std::path::Path, req: Request) -> Response {
    let (mut parts, _body) = req.into_parts();
    let url = upstream_target(port, "ws", &path, &parts.uri);
    let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
        return text(StatusCode::BAD_REQUEST, "bad websocket upgrade");
    };
    let Ok(mut request) = url.into_client_request() else {
        return text(StatusCode::BAD_REQUEST, "bad websocket target");
    };
    if parts.headers.contains_key(header::ORIGIN) {
        if let Ok(value) = HeaderValue::from_str(&format!("http://127.0.0.1:{port}")) {
            request.headers_mut().insert(header::ORIGIN, value);
        }
    }
    set_gateway_root(request.headers_mut(), root);
    // Connect first so a dead worker is a 502, not an upgrade that dies.
    let upstream = match tokio_tungstenite::connect_async(request).await {
        Ok((stream, _)) => stream,
        Err(e) => return text(StatusCode::BAD_GATEWAY, &format!("worker unreachable: {e}")),
    };
    upgrade.on_upgrade(move |client| pump(client, upstream))
}

type Upstream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Copies frames both ways until either side ends, then drops both: closing
/// the browser side must close the worker side too, or the worker never sees
/// its tab go away.
async fn pump(client: WebSocket, upstream: Upstream) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();
    let to_upstream = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let closing = matches!(msg, AxMessage::Close(_));
            if up_tx.send(to_tungstenite(msg)).await.is_err() || closing {
                break;
            }
        }
        let _ = up_tx.close().await;
    };
    let to_client = async {
        while let Some(Ok(msg)) = up_rx.next().await {
            let Some(msg) = from_tungstenite(msg) else {
                continue;
            };
            let closing = matches!(msg, AxMessage::Close(_));
            if client_tx.send(msg).await.is_err() || closing {
                break;
            }
        }
        let _ = client_tx.close().await;
    };
    tokio::select! {
        _ = to_upstream => {}
        _ = to_client => {}
    }
}

fn to_tungstenite(msg: AxMessage) -> TunMessage {
    match msg {
        AxMessage::Text(t) => TunMessage::Text(t.into()),
        AxMessage::Binary(b) => TunMessage::Binary(b.into()),
        AxMessage::Ping(b) => TunMessage::Ping(b.into()),
        AxMessage::Pong(b) => TunMessage::Pong(b.into()),
        AxMessage::Close(frame) => TunMessage::Close(frame.map(|f| TunCloseFrame {
            code: CloseCode::from(f.code),
            reason: f.reason.to_string().into(),
        })),
    }
}

fn from_tungstenite(msg: TunMessage) -> Option<AxMessage> {
    Some(match msg {
        TunMessage::Text(t) => AxMessage::Text(t.to_string()),
        TunMessage::Binary(b) => AxMessage::Binary(b.to_vec()),
        TunMessage::Ping(b) => AxMessage::Ping(b.to_vec()),
        TunMessage::Pong(b) => AxMessage::Pong(b.to_vec()),
        TunMessage::Close(frame) => AxMessage::Close(frame.map(|f| CloseFrame {
            code: u16::from(f.code),
            reason: f.reason.to_string().into(),
        })),
        TunMessage::Frame(_) => return None,
    })
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;
