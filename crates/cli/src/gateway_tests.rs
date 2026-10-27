use super::*;
use axum::extract::ws::{Message as WsMsg, WebSocket as AxWs};
use axum::routing::{any, get};
use axum::Json;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

// ---- pure helpers -----------------------------------------------------

#[test]
fn host_name_strips_ports_and_keeps_ipv6_brackets() {
    assert_eq!(host_name("example.com"), "example.com");
    assert_eq!(host_name("example.com:8080"), "example.com");
    assert_eq!(host_name("[::1]:4590"), "[::1]");
    assert_eq!(host_name("[::1]"), "[::1]");
    assert_eq!(host_name("127.0.0.1:1"), "127.0.0.1");
}

#[test]
fn rel_root_climbs_to_the_gateway_root() {
    assert_eq!(rel_root("/"), "./");
    assert_eq!(rel_root("/_/login"), "../");
    assert_eq!(rel_root("/c/todo-abcdefgh/"), "../../");
    assert_eq!(rel_root("/c/todo-abcdefgh/api/x"), "../../../");
}

#[test]
fn next_targets_must_stay_on_this_gateway() {
    assert!(safe_next("/"));
    assert!(safe_next("/c/x/?a=1"));
    assert!(!safe_next("//evil.example/"));
    assert!(!safe_next("https://evil.example/"));
    assert!(!safe_next("/\\evil"));
    assert!(!safe_next("/a\r\nSet-Cookie: x=1"));
    assert!(!safe_next(""));
}

#[test]
fn cookie_lookup_handles_several_cookies_and_headers() {
    let mut headers = HeaderMap::new();
    headers.append(
        header::COOKIE,
        HeaderValue::from_static("a=1; mfx_session=abc; b=2"),
    );
    assert_eq!(cookie(&headers, SESSION_COOKIE), Some("abc"));
    assert_eq!(cookie(&headers, "missing"), None);
    let mut split = HeaderMap::new();
    split.append(header::COOKIE, HeaderValue::from_static("a=1"));
    split.append(header::COOKIE, HeaderValue::from_static("mfx_session=zzz"));
    assert_eq!(cookie(&split, SESSION_COOKIE), Some("zzz"));
}

#[test]
fn upstream_headers_drop_credentials_and_rewrite_origin() {
    let mut headers = HeaderMap::new();
    for (k, v) in [
        ("host", "gateway.example:4590"),
        ("origin", "http://gateway.example:4590"),
        ("cookie", "mfx_session=s"),
        ("authorization", "Bearer t"),
        ("referer", "http://gateway.example:4590/c/x/"),
        ("connection", "keep-alive"),
        ("sec-websocket-key", "k"),
        ("content-type", "application/json"),
        ("x-custom", "kept"),
        ("x-meshfox-gateway-root", "/spoofed"),
        ("x-meshfox-other", "1"),
    ] {
        headers.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    let out = upstream_headers(&headers, 9, std::path::Path::new("/srv/notes dir"));
    let names: Vec<&str> = out.keys().map(HeaderName::as_str).collect();
    assert!(names.contains(&"content-type") && names.contains(&"x-custom"));
    for dropped in [
        "host",
        "cookie",
        "authorization",
        "referer",
        "connection",
        "sec-websocket-key",
    ] {
        assert!(!names.contains(&dropped), "{dropped} must not be forwarded");
    }
    assert_eq!(out.get("origin").unwrap(), "http://127.0.0.1:9");
    // The client's claims are gone; the gateway's own header replaces them.
    assert_eq!(out.get(GATEWAY_ROOT_HEADER).unwrap(), "/srv/notes%20dir");
    assert!(!names.contains(&"x-meshfox-other"));
    assert!(
        !upstream_headers(&HeaderMap::new(), 9, std::path::Path::new("/r")).contains_key("origin")
    );
}

// ---- end to end against a fake worker ---------------------------------

struct Fake {
    port: u16,
    seen: Mutex<Vec<PathBuf>>,
    running: Vec<PathBuf>,
}

impl CanvasResolver for Fake {
    fn port_for(&self, canvas: PathBuf) -> PortFuture<'_> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(canvas);
            Ok(self.port)
        })
    }
    fn running(&self) -> Vec<PathBuf> {
        self.running.clone()
    }
}

/// A stand-in worker: serves a page, echoes request details, echoes
/// WebSocket text and records when its socket closes.
async fn start_worker(ws_closed: Arc<AtomicBool>) -> u16 {
    async fn echo(req: Request) -> Json<serde_json::Value> {
        let (parts, body) = req.into_parts();
        let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
        let h = |name: &str| {
            parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        Json(serde_json::json!({
            "method": parts.method.as_str(),
            "path": parts.uri.path(),
            "query": parts.uri.query(),
            "host": h("host"), "origin": h("origin"),
            "gatewayRoot": h("x-meshfox-gateway-root"),
            "cookie": h("cookie"), "authorization": h("authorization"),
            "body": String::from_utf8_lossy(&body),
        }))
    }
    let app = Router::new()
        .route("/", get(|| async { "worker-root" }))
        .route("/api/echo", any(echo))
        .route(
            "/ws",
            get(move |ws: WebSocketUpgrade| {
                let closed = Arc::clone(&ws_closed);
                async move {
                    ws.on_upgrade(move |mut socket: AxWs| async move {
                        while let Some(Ok(msg)) = socket.recv().await {
                            if let WsMsg::Text(t) = msg {
                                let _ = socket.send(WsMsg::Text(format!("echo:{t}"))).await;
                            }
                        }
                        closed.store(true, Ordering::SeqCst);
                    })
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

struct Harness {
    base: String,
    addr: SocketAddr,
    root: PathBuf,
    worker_port: u16,
    resolver: Arc<Fake>,
    slug: String,
    ws_closed: Arc<AtomicBool>,
    client: reqwest::Client,
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mfx-gw-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("notes")).unwrap();
    fs::write(dir.join("notes/todo.canvas.md"), "# todo\n").unwrap();
    dir.canonicalize().unwrap()
}

/// The binary installs the process-wide rustls provider in `main`; tests build
/// reqwest clients without going through it.
fn ensure_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn harness(name: &str, token: Option<&str>, loopback: bool, allowed: &[&str]) -> Harness {
    harness_with(name, token, loopback, allowed, false).await
}

async fn harness_with(
    name: &str,
    token: Option<&str>,
    loopback: bool,
    allowed: &[&str],
    trust_proxy: bool,
) -> Harness {
    ensure_crypto();
    let root = temp_root(name);
    let ws_closed = Arc::new(AtomicBool::new(false));
    let worker_port = start_worker(Arc::clone(&ws_closed)).await;
    let resolver = Arc::new(Fake {
        port: worker_port,
        seen: Mutex::new(Vec::new()),
        running: vec![root.join("notes/todo.canvas.md")],
    });
    let gateway = Gateway::new(
        GatewayConfig {
            root: root.clone(),
            token: token.map(str::to_string),
            allowed_hosts: allowed.iter().map(|s| s.to_string()).collect(),
            loopback,
            trust_proxy,
        },
        resolver.clone(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = gateway.router();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let slug =
        meshfox_core::slug::slug_for_rel(std::path::Path::new("notes/todo.canvas.md")).unwrap();
    Harness {
        base: format!("http://{addr}"),
        addr,
        root,
        worker_port,
        resolver,
        slug,
        ws_closed,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap(),
    }
}

impl Harness {
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(format!("{}{path}", self.base))
    }
}

#[tokio::test]
async fn open_gateway_serves_ping_index_and_json() {
    let h = harness("index", None, true, &[]).await;
    assert_eq!(
        h.get("/_/ping").send().await.unwrap().text().await.unwrap(),
        "ok"
    );

    let page = h.get("/").send().await.unwrap().text().await.unwrap();
    assert!(page.contains("todo.canvas.md"), "{page}");
    assert!(page.contains(&format!("href=\"c/{}/\"", h.slug)));
    assert!(page.contains("● running"));

    let json: serde_json::Value = h
        .get("/_/index.json")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(json["canvases"][0]["path"], "notes/todo.canvas.md");
    assert_eq!(json["canvases"][0]["slug"], h.slug.as_str());
}

#[tokio::test]
async fn proxies_http_with_path_query_and_rewritten_headers() {
    let h = harness("proxy", None, true, &[]).await;
    let root = h.get(&format!("/c/{}/", h.slug)).send().await.unwrap();
    assert_eq!(root.text().await.unwrap(), "worker-root");
    assert_eq!(
        h.resolver.seen.lock().unwrap().last().unwrap(),
        &h.root.join("notes/todo.canvas.md")
    );

    let echoed: serde_json::Value = h
        .client
        .post(format!("{}/c/{}/api/echo?x=1&y=a%20b", h.base, h.slug))
        .header("origin", format!("http://{}", h.addr))
        .header("cookie", "other=1")
        .header("authorization", "Bearer not-for-the-worker")
        .header("x-meshfox-gateway-root", "/spoofed")
        .body("payload")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(echoed["method"], "POST");
    assert_eq!(echoed["path"], "/api/echo");
    assert_eq!(echoed["query"], "x=1&y=a%20b");
    assert_eq!(echoed["body"], "payload");
    assert_eq!(echoed["host"], format!("127.0.0.1:{}", h.worker_port));
    assert_eq!(
        echoed["origin"],
        format!("http://127.0.0.1:{}", h.worker_port)
    );
    assert!(echoed["cookie"].is_null() && echoed["authorization"].is_null());
    // The worker is told it is behind this gateway, with the real root.
    assert_eq!(
        echoed["gatewayRoot"],
        meshfox_core::gateway_header::encode_root(&h.root).as_str()
    );
}

#[tokio::test]
async fn a_slug_without_trailing_slash_redirects_with_its_query() {
    let h = harness("slash", None, true, &[]).await;
    let res = h.get(&format!("/c/{}?a=1", h.slug)).send().await.unwrap();
    assert_eq!(res.status(), 308);
    assert_eq!(
        res.headers()["location"],
        format!("{}/?a=1", h.slug).as_str()
    );
}

#[tokio::test]
async fn unknown_or_tampered_slugs_are_404() {
    let h = harness("slugs", None, true, &[]).await;
    let hash = meshfox_core::slug::hash_of(&h.slug).unwrap().to_string();
    for bad in ["nope", "todo-aaaaaaaa", &format!("other-{hash}")] {
        let res = h.get(&format!("/c/{bad}/")).send().await.unwrap();
        assert_eq!(res.status(), 404, "{bad}");
    }
    assert_eq!(h.get("/elsewhere").send().await.unwrap().status(), 404);
    assert!(h.resolver.seen.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_canvas_swapped_for_a_symlink_out_of_the_root_is_forbidden() {
    let h = harness("swap", None, true, &[]).await;
    let outside = temp_root("swap-outside");
    let target = h.root.join("notes/todo.canvas.md");
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(outside.join("notes/todo.canvas.md"), &target).unwrap();
    let res = h.get(&format!("/c/{}/", h.slug)).send().await.unwrap();
    assert_eq!(res.status(), 403);
    assert!(h.resolver.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn host_header_must_be_loopback_or_allowed() {
    let h = harness("host", None, true, &["gw.example"]).await;
    let with_host = |host: &str| h.get("/_/ping").header("host", host).send();
    assert_eq!(with_host("evil.example").await.unwrap().status(), 403);
    assert_eq!(with_host("localhost:1").await.unwrap().status(), 200);
    assert_eq!(with_host("GW.example:4590").await.unwrap().status(), 200);

    // A non-loopback bind with no list accepts any name (the token is the gate).
    let open = harness("host2", None, false, &[]).await;
    let res = open
        .get("/_/ping")
        .header("host", "anything.example")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    // ...but with a list, only those.
    let listed = harness("host3", None, false, &["only.example"]).await;
    let res = listed
        .get("/_/ping")
        .header("host", "anything.example")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn a_foreign_origin_is_refused() {
    let h = harness("origin", None, true, &[]).await;
    let res = h
        .get("/_/ping")
        .header("origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
    let res = h
        .get("/_/ping")
        .header("origin", format!("http://{}", h.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn token_gate_login_and_cookie_flow() {
    let h = harness("auth", Some("s3cret"), true, &[]).await;
    let canvas = format!("/c/{}/", h.slug);

    // No credentials: 401 for API clients, a form for browsers.
    let res = h.get(&canvas).send().await.unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(res.headers()["www-authenticate"], "Bearer");
    let page = h
        .get(&canvas)
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 401);
    let form = page.text().await.unwrap();
    assert!(
        form.contains("action=\"../../_/login\"") && form.contains(&format!("value=\"{canvas}\""))
    );
    assert!(h.resolver.seen.lock().unwrap().is_empty());

    // Bearer works; a wrong one does not.
    let ok = h.get(&canvas).bearer_auth("s3cret").send().await.unwrap();
    assert_eq!(ok.text().await.unwrap(), "worker-root");
    assert_eq!(
        h.get(&canvas)
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    // Wrong token on the form: 401, no cookie.
    let bad = h
        .client
        .post(format!("{}/_/login", h.base))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(serde_urlencoded::to_string([("token", "nope"), ("next", canvas.as_str())]).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    assert!(bad.headers().get("set-cookie").is_none());

    // Right token: cookie + redirect (relative, to the sanitised target).
    let login = h
        .client
        .post(format!("{}/_/login", h.base))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(
            serde_urlencoded::to_string([("token", "s3cret"), ("next", canvas.as_str())]).unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 303);
    assert_eq!(login.headers()["location"], format!("..{canvas}").as_str());
    let set = login.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Path=/"));
    assert!(
        !set.contains("s3cret"),
        "the cookie must not carry the raw token"
    );
    let session = set.split(';').next().unwrap().to_string();
    let via_cookie = h
        .get(&canvas)
        .header("cookie", session)
        .send()
        .await
        .unwrap();
    assert_eq!(via_cookie.text().await.unwrap(), "worker-root");

    // An off-site `next` falls back to the root.
    let evil = h
        .client
        .post(format!("{}/_/login", h.base))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(
            serde_urlencoded::to_string([("token", "s3cret"), ("next", "//evil.example/")])
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(evil.headers()["location"], "../");

    // The index and ping: ping is open, the index is not.
    assert_eq!(h.get("/_/ping").send().await.unwrap().status(), 200);
    assert_eq!(h.get("/").send().await.unwrap().status(), 401);
}

#[tokio::test]
async fn repeated_bad_tokens_get_rate_limited_even_for_the_right_one() {
    let h = harness("limit", Some("s3cret"), true, &[]).await;
    for _ in 0..FAILED_AUTH_LIMIT {
        let res = h.get("/").bearer_auth("wrong").send().await.unwrap();
        assert_eq!(res.status(), 401);
    }
    let res = h.get("/").bearer_auth("s3cret").send().await.unwrap();
    assert_eq!(res.status(), 429);
    assert_eq!(res.headers()["retry-after"], "60");
}

#[tokio::test]
async fn websocket_is_proxied_both_ways_and_closing_the_client_closes_the_worker() {
    let h = harness("ws", Some("s3cret"), true, &[]).await;
    let url = format!("ws://{}/c/{}/ws", h.addr, h.slug);
    // Without credentials the upgrade is refused.
    assert!(tokio_tungstenite::connect_async(url.clone()).await.is_err());

    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer s3cret"),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket.send(TunMessage::Text("hi".into())).await.unwrap();
    let reply = socket.next().await.unwrap().unwrap();
    assert_eq!(reply.into_text().unwrap().as_str(), "echo:hi");

    socket.close(None).await.unwrap();
    drop(socket);
    for _ in 0..50 {
        if h.ws_closed.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the worker never saw its websocket close");
}

#[tokio::test]
async fn a_dead_worker_is_a_502_not_a_hang() {
    let h = harness("dead", None, true, &[]).await;
    // Point the fake at a port nobody listens on.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let gateway = Gateway::new(
        GatewayConfig {
            root: h.root.clone(),
            token: None,
            allowed_hosts: vec![],
            loopback: true,
            trust_proxy: false,
        },
        Arc::new(Fake {
            port: dead,
            seen: Mutex::new(Vec::new()),
            running: vec![],
        }),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = gateway.router();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let res = h
        .client
        .get(format!("http://{addr}/c/{}/", h.slug))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
}

#[test]
fn client_ip_trusts_only_the_last_forwarded_entry_and_only_behind_a_proxy() {
    let peer: Option<IpAddr> = Some("10.0.0.1".parse().unwrap());
    let mut headers = HeaderMap::new();
    headers.append(
        "x-forwarded-for",
        HeaderValue::from_static("6.6.6.6, 203.0.113.9"),
    );
    // Not trusted: the header is ignored, the socket peer counts.
    assert_eq!(client_ip(false, &headers, peer), peer);
    // Trusted: the entry the proxy appended, not the one the client claimed.
    assert_eq!(
        client_ip(true, &headers, peer),
        Some("203.0.113.9".parse().unwrap())
    );
    // Several header lines count as one list.
    headers.append("x-forwarded-for", HeaderValue::from_static("198.51.100.2"));
    assert_eq!(
        client_ip(true, &headers, peer),
        Some("198.51.100.2".parse().unwrap())
    );
    // Garbage or absent: fall back to the peer.
    let mut bad = HeaderMap::new();
    bad.append("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
    assert_eq!(client_ip(true, &bad, peer), peer);
    assert_eq!(client_ip(true, &HeaderMap::new(), peer), peer);
}

async fn bad_guess(h: &Harness, forwarded_for: &str) -> reqwest::StatusCode {
    h.get("/")
        .bearer_auth("wrong")
        .header("x-forwarded-for", forwarded_for)
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn behind_a_trusted_proxy_one_clients_bad_guesses_do_not_lock_out_the_others() {
    let h = harness_with("proxy-limit", Some("s3cret"), true, &[], true).await;
    for _ in 0..FAILED_AUTH_LIMIT {
        assert_eq!(bad_guess(&h, "203.0.113.1").await, 401);
    }
    assert_eq!(bad_guess(&h, "203.0.113.1").await, 429);
    // A different client behind the same proxy is unaffected, right token included.
    let ok = h
        .get("/_/index.json")
        .bearer_auth("s3cret")
        .header("x-forwarded-for", "203.0.113.2")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    // A client cannot dodge its own count by prepending a fake address.
    assert_eq!(bad_guess(&h, "9.9.9.9, 203.0.113.1").await, 429);
}

#[tokio::test]
async fn without_trust_proxy_forwarded_for_is_ignored() {
    let h = harness("no-trust", Some("s3cret"), true, &[]).await;
    for i in 0..FAILED_AUTH_LIMIT {
        assert_eq!(bad_guess(&h, &format!("203.0.113.{i}")).await, 401);
    }
    // Varying the claimed address changed nothing: the real peer is limited.
    assert_eq!(bad_guess(&h, "203.0.113.200").await, 429);
}

#[tokio::test]
async fn the_session_cookie_is_secure_only_when_a_trusted_proxy_says_https() {
    for (trust, expect_secure) in [(true, true), (false, false)] {
        let h = harness_with("secure", Some("s3cret"), true, &[], trust).await;
        let login = h
            .client
            .post(format!("{}/_/login", h.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-proto", "https")
            .body("token=s3cret&next=%2F")
            .send()
            .await
            .unwrap();
        let cookie = login.headers()["set-cookie"].to_str().unwrap().to_string();
        assert_eq!(
            cookie.contains("Secure"),
            expect_secure,
            "trust={trust}: {cookie}"
        );
    }
}

// ---- a scan that takes forever must not take the gateway with it --------

/// Stands in for a walk of a huge tree on a slow mount: blocks until released.
struct Stuck {
    released: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Stuck {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

#[tokio::test]
async fn a_scan_that_never_finishes_does_not_block_the_index_the_pings_or_other_canvases() {
    use meshfox_core::canvas_index::{CanvasIndex, IndexCache, Lookup, ScanOptions};

    ensure_crypto();
    let root = temp_root("stuck");
    let worker_port = start_worker(Arc::new(AtomicBool::new(false))).await;
    let stuck = Arc::new(Stuck {
        released: Mutex::new(false),
        cv: std::sync::Condvar::new(),
    });
    let scanner = {
        let (stuck, root) = (Arc::clone(&stuck), root.clone());
        Box::new(move || {
            let mut released = stuck.released.lock().unwrap();
            while !*released {
                released = stuck.cv.wait(released).unwrap();
            }
            CanvasIndex::scan(&root, &ScanOptions::default())
        })
    };
    let index = IndexCache::with_scanner(
        scanner,
        Duration::ZERO,
        Duration::from_secs(3600),
        Duration::from_millis(300),
    );
    let gateway = Gateway::with_index(
        GatewayConfig {
            root: root.clone(),
            token: None,
            allowed_hosts: vec![],
            loopback: true,
            trust_proxy: false,
        },
        Arc::new(Fake {
            port: worker_port,
            seen: Mutex::new(Vec::new()),
            running: vec![],
        }),
        Arc::clone(&index),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = gateway.router();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let slug =
        meshfox_core::slug::slug_for_rel(std::path::Path::new("notes/todo.canvas.md")).unwrap();

    // While the first scan is stuck: everything that does not need the index
    // answers, the index page says it is scanning, and a canvas is a quick
    // 503, not a hang.
    let started = Instant::now();
    assert_eq!(
        client
            .get(format!("{base}/_/ping"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let page = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    let page = page.text().await.unwrap();
    assert!(
        page.contains("Scanning the served root") && page.contains("http-equiv"),
        "{page}"
    );
    let json: serde_json::Value = client
        .get(format!("{base}/_/index.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(json["indexing"], true);
    assert_eq!(json["canvases"].as_array().unwrap().len(), 0);
    let canvas = client
        .get(format!("{base}/c/{slug}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(canvas.status(), 503);
    assert_eq!(canvas.headers()["retry-after"], "2");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    // Many viewers at once share the one scan that is running.
    let views: Vec<_> = (0..30)
        .map(|_| {
            let (client, url) = (client.clone(), format!("{base}/"));
            tokio::spawn(async move { client.get(url).send().await.unwrap().status() })
        })
        .collect();
    for view in views {
        assert_eq!(view.await.unwrap(), 200);
    }
    assert_eq!(index.scans_started(), 1, "one scan, however many requests");

    // Released: the index fills in and the canvas opens.
    stuck.release();
    let mut listed = false;
    for _ in 0..100 {
        let json: serde_json::Value = client
            .get(format!("{base}/_/index.json"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if json["indexing"] == false && json["canvases"].as_array().unwrap().len() == 1 {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(listed, "the index never filled in");
    let open = client
        .get(format!("{base}/c/{slug}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), 200);
    assert_eq!(open.text().await.unwrap(), "worker-root");
    assert!(matches!(index.resolve(&slug), Lookup::Found(_)));
}
