//! Fixture web server for ferridriver's own test suites.
//!
//! Dev-only infrastructure (`publish = false`): the repo's BDD features
//! and TypeScript e2e tests need deterministic HTTP behaviours —
//! redirect chains, echo endpoints, cookies, Basic auth, CSP, download
//! attachments, a WebSocket echo, and an observable HTTP proxy. This
//! crate serves all of them from one binary that `ferridriver.toml`
//! wires in as a `command` web server, keeping the routes out of the
//! shipped `ferridriver-test` runner.
//!
//! Two listeners:
//! - the main server: static files from `--static` plus the dynamic
//!   `/fx/*` routes and the httpbin-shaped `/_api/*` echo;
//! - the proxy: a minimal HTTP proxy that answers every absolute-form
//!   request with a canned page and records the request line, exposed
//!   through `/fx/proxy-info` and `/fx/proxy-log` on the main server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, Response};
use axum::routing::any;
use base64::Engine as _;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

#[derive(Debug, Clone, Default)]
pub struct FixtureServerOptions {
  /// Main listener port; `0` binds an ephemeral port.
  pub port: u16,
  /// Proxy listener port; `0` binds an ephemeral port (discoverable via
  /// `/fx/proxy-info`).
  pub proxy_port: u16,
  /// Optional directory served for paths outside `/fx/` and `/_api/`.
  pub static_dir: Option<PathBuf>,
}

struct ServerState {
  proxy: Arc<ProxyState>,
  reset: Arc<ResetState>,
  reset_addr: SocketAddr,
  tls_addr: SocketAddr,
  held: Mutex<rustc_hash::FxHashMap<String, tokio::sync::watch::Sender<(bool, bool)>>>,
}

/// Per-key budget of connections the reset listener still has to abort.
/// `/fx/reset-arm?key=K&times=N` seeds it; each aborted connection spends
/// one. Keyed so concurrent tests (and the four backend projects) never
/// consume each other's budget.
#[derive(Default)]
struct ResetState {
  remaining: Mutex<rustc_hash::FxHashMap<String, u32>>,
}

/// Request log for the proxy listener. `lines` records the raw request
/// line of every request the proxy served; `/fx/proxy-log` exposes it
/// (hits = line count) so browser-side tests can prove traffic actually
/// traversed the proxy.
struct ProxyState {
  addr: SocketAddr,
  lines: Mutex<Vec<String>>,
}

pub struct FixtureServer {
  addr: SocketAddr,
  proxy_addr: SocketAddr,
  reset_addr: SocketAddr,
  tls_addr: SocketAddr,
  handle: tokio::task::JoinHandle<()>,
  proxy_handle: tokio::task::JoinHandle<()>,
  reset_handle: tokio::task::JoinHandle<()>,
  tls_handle: tokio::task::JoinHandle<()>,
}

impl FixtureServer {
  /// Bind both listeners and start serving.
  ///
  /// # Errors
  ///
  /// Returns an error when either listener fails to bind.
  pub async fn start(options: FixtureServerOptions) -> std::io::Result<Self> {
    let proxy_listener = tokio::net::TcpListener::bind(("127.0.0.1", options.proxy_port)).await?;
    let proxy = Arc::new(ProxyState {
      addr: proxy_listener.local_addr()?,
      lines: Mutex::new(Vec::new()),
    });
    let proxy_addr = proxy.addr;
    let proxy_handle = tokio::spawn(run_proxy(proxy_listener, Arc::clone(&proxy)));

    let reset = Arc::new(ResetState::default());
    let reset_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let reset_addr = reset_listener.local_addr()?;
    let reset_handle = tokio::spawn(run_reset(reset_listener, Arc::clone(&reset)));

    let tls_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let tls_addr = tls_listener.local_addr()?;
    let tls_handle = tokio::spawn(run_tls(tls_listener));

    let state = Arc::new(ServerState {
      proxy,
      reset,
      reset_addr,
      tls_addr,
      held: Mutex::default(),
    });
    let mut app = Router::new()
      // Literal route wins over the wildcard, so the WS upgrade
      // extractor only runs for the echo endpoint.
      .route("/fx/ws", any(ws_upgrade))
      .route("/fx/control/{action}/{key}", any(handle_control))
      .route("/fx/{*path}", any(handle_fx))
      .route("/_api/{*path}", any(handle_api_echo));
    app = match &options.static_dir {
      Some(dir) => app.fallback_service(ServeDir::new(dir).append_index_html_on_directories(true)),
      None => app.fallback(any(|| async { fx_text("ferridriver-fixtures") })),
    };
    let app = app.with_state(state).layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port)).await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
      axum::serve(listener, app).await.ok();
    });

    Ok(Self {
      addr,
      proxy_addr,
      reset_addr,
      tls_addr,
      handle,
      proxy_handle,
      reset_handle,
      tls_handle,
    })
  }

  #[must_use]
  pub fn url(&self) -> String {
    format!("http://{}", self.addr)
  }

  #[must_use]
  pub fn proxy_url(&self) -> String {
    format!("http://{}", self.proxy_addr)
  }

  /// Origin of the listener that aborts connections (`/fx/endpoints`).
  #[must_use]
  pub fn reset_url(&self) -> String {
    format!("http://{}", self.reset_addr)
  }

  /// Origin of the HTTPS listener holding a self-signed certificate.
  #[must_use]
  pub fn tls_url(&self) -> String {
    format!("https://{}", self.tls_addr)
  }

  /// Serve until aborted.
  pub async fn run_forever(self) {
    let _ = tokio::join!(self.handle, self.proxy_handle, self.reset_handle, self.tls_handle);
  }

  pub async fn stop(self) {
    self.handle.abort();
    self.proxy_handle.abort();
    self.reset_handle.abort();
    self.tls_handle.abort();
    let _ = self.handle.await;
    let _ = self.proxy_handle.await;
    let _ = self.reset_handle.await;
    let _ = self.tls_handle.await;
  }
}

// ── Response helpers ────────────────────────────────────────────────────────

fn fx_build(status: u16, content_type: &str, body: Vec<u8>, extra: &[(&str, String)]) -> Response<Body> {
  let mut builder = Response::builder().status(status).header("content-type", content_type);
  for (k, v) in extra {
    builder = builder.header(*k, v.as_str());
  }
  builder
    .body(Body::from(body))
    .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn fx_text(body: &str) -> Response<Body> {
  fx_build(200, "text/plain", body.as_bytes().to_vec(), &[])
}

fn fx_html(body: &str) -> Response<Body> {
  fx_build(200, "text/html", body.as_bytes().to_vec(), &[])
}

fn fx_json(body: &serde_json::Value) -> Response<Body> {
  fx_build(200, "application/json", body.to_string().into_bytes(), &[])
}

fn fx_redirect(location: &str) -> Response<Body> {
  fx_build(302, "text/plain", Vec::new(), &[("location", location.to_string())])
}

/// A page sound enough that `checkPageQuality()` fails it on nothing
/// except whatever the route serving it did to the response.
fn fx_auditable(heading: &str) -> String {
  format!(
    "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
     <title>{heading}</title><meta name=\"description\" content=\"A page the quality audits pass.\">\
     </head><body><main><h1>{heading}</h1></main></body></html>"
  )
}

/// Body compressed with one of the four HTTP content codings, served
/// with the matching `Content-Encoding`. The plaintext embeds the
/// `Accept-Encoding` the request carried, so one assertion covers both
/// halves of transparent decompression: that the client advertised the
/// codings, and that it decoded the reply.
///
/// The payload is deliberately repetitive so the encoded bytes are much
/// shorter than the plaintext — a client that fails to decode cannot
/// accidentally still parse it.
fn fx_compressed(algo: &str, headers: &HeaderMap) -> Response<Body> {
  use std::io::Write as _;

  let accept_encoding = headers
    .get("accept-encoding")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("")
    .to_string();
  let plain = serde_json::json!({
    "algo": algo,
    "acceptEncoding": accept_encoding,
    "payload": "ferridriver-compression-probe ".repeat(64),
  })
  .to_string();

  let encoded = match algo {
    "gzip" => {
      let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
      e.write_all(plain.as_bytes()).and_then(|()| e.finish())
    },
    // HTTP `deflate` is the zlib wrapper (RFC 1950), not a raw stream.
    "deflate" => {
      let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
      e.write_all(plain.as_bytes()).and_then(|()| e.finish())
    },
    "br" => {
      let mut out = Vec::new();
      let mut e = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
      e.write_all(plain.as_bytes()).map(|()| drop(e)).map(|()| out)
    },
    "zstd" => zstd::stream::encode_all(plain.as_bytes(), 3),
    _ => {
      return fx_build(
        400,
        "text/plain",
        format!("unknown content coding: {algo}").into_bytes(),
        &[],
      );
    },
  };

  match encoded {
    Ok(bytes) => fx_build(
      200,
      "application/json",
      bytes,
      &[("content-encoding", algo.to_string())],
    ),
    Err(e) => fx_build(500, "text/plain", format!("encode failed: {e}").into_bytes(), &[]),
  }
}

/// Attachment that never completes: declares a large Content-Length and
/// dribbles zero bytes until the client tears the connection down
/// (which browsers do on `download.cancel()`). Keeps a download
/// deterministically in-flight so cancel-vs-complete races always
/// resolve as canceled. Bounded at ~30s as a safety cap.
fn fx_download_hang() -> Response<Body> {
  let stream = futures::stream::unfold(0u32, |i| async move {
    if i >= 600 {
      return None;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Some((Ok::<_, std::io::Error>(vec![0u8; 1024]), i + 1))
  });
  Response::builder()
    .status(200)
    .header("content-type", "application/octet-stream")
    .header("content-disposition", "attachment; filename=\"greeting.txt\"")
    .header("content-length", "1048576")
    .body(Body::from_stream(stream))
    .unwrap_or_else(|_| fx_build(500, "text/plain", b"stream error".to_vec(), &[]))
}

fn percent_decode(s: &str) -> String {
  fn hex_val(b: u8) -> Option<u8> {
    match b {
      b'0'..=b'9' => Some(b - b'0'),
      b'a'..=b'f' => Some(b - b'a' + 10),
      b'A'..=b'F' => Some(b - b'A' + 10),
      _ => None,
    }
  }
  let bytes = s.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    match bytes[i] {
      b'%' if i + 2 < bytes.len() => {
        if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
          out.push(hi * 16 + lo);
          i += 3;
        } else {
          out.push(b'%');
          i += 1;
        }
      },
      b'+' => {
        out.push(b' ');
        i += 1;
      },
      b => {
        out.push(b);
        i += 1;
      },
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// Query params with the given key, percent-decoded, in order.
fn query_values(query: Option<&str>, key: &str) -> Vec<String> {
  let Some(query) = query else { return Vec::new() };
  query
    .split('&')
    .filter_map(|pair| {
      let (k, v) = pair.split_once('=')?;
      (percent_decode(k) == key).then(|| percent_decode(v))
    })
    .collect()
}

fn headers_json(headers: &HeaderMap) -> serde_json::Map<String, serde_json::Value> {
  let mut map = serde_json::Map::new();
  for (name, value) in headers {
    if let Ok(v) = value.to_str() {
      let key = name.as_str().to_ascii_lowercase();
      match map.get_mut(&key) {
        Some(serde_json::Value::String(existing)) => {
          existing.push_str(", ");
          existing.push_str(v);
        },
        _ => {
          map.insert(key, serde_json::Value::String(v.to_string()));
        },
      }
    }
  }
  map
}

// ── Route handlers ──────────────────────────────────────────────────────────

/// httpbin-shaped JSON echo of the request (url, method, headers, raw
/// body, parsed JSON body) — the BDD suite's HTTP-client features
/// assert round-trips against it.
async fn handle_api_echo(
  Path(path): Path<String>,
  headers: HeaderMap,
  method: axum::http::Method,
  RawQuery(query): RawQuery,
  body: axum::body::Bytes,
) -> Response<Body> {
  let body_text = String::from_utf8_lossy(&body).to_string();
  let parsed_json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
  let url = match query {
    Some(q) => format!("/_api/{path}?{q}"),
    None => format!("/_api/{path}"),
  };
  fx_json(&serde_json::json!({
    "url": url,
    "method": method.to_string(),
    "headers": headers_json(&headers),
    "data": body_text,
    "json": parsed_json,
  }))
}

/// Method + headers + body in one reply, so a test can assert what a
/// request actually carried without issuing it twice.
fn fx_echo_request(method: &axum::http::Method, headers: &HeaderMap, body: &axum::body::Bytes) -> Response<Body> {
  fx_json(&serde_json::json!({
    "method": method.as_str(),
    "headers": serde_json::Value::Object(headers_json(headers)),
    "body": String::from_utf8_lossy(body),
  }))
}

/// Grant `key` a budget of `times` connection resets on the reset
/// listener; arming itself never resets.
fn fx_reset_arm(state: &ResetState, query: Option<&str>) -> Response<Body> {
  let key = query_values(query, "key").into_iter().next().unwrap_or_default();
  let times: u32 = query_values(query, "times")
    .into_iter()
    .next()
    .and_then(|t| t.parse().ok())
    .unwrap_or(1);
  state
    .remaining
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .insert(key.clone(), times);
  fx_json(&serde_json::json!({ "key": key, "times": times }))
}

/// Each `c` query param becomes one Set-Cookie header verbatim:
/// `/fx/set-cookie?c=name%3Dvalue%3B%20Path%3D%2F`.
fn fx_set_cookie(query: Option<&str>) -> Response<Body> {
  let cookies: Vec<(&str, String)> = query_values(query, "c")
    .into_iter()
    .map(|c| ("set-cookie", c))
    .collect();
  fx_build(200, "text/plain", b"cookie-set".to_vec(), &cookies)
}

/// Sets cookies (each `c` param, verbatim) AND 302-redirects to `loc`
/// (default `/fx/landed`) — proves redirect-hop Set-Cookie capture.
fn fx_set_cookie_redirect(query: Option<&str>) -> Response<Body> {
  let mut extra: Vec<(&str, String)> = query_values(query, "c")
    .into_iter()
    .map(|c| ("set-cookie", c))
    .collect();
  let loc = query_values(query, "loc")
    .into_iter()
    .next()
    .unwrap_or_else(|| "/fx/landed".to_string());
  extra.push(("location", loc));
  fx_build(302, "text/plain", Vec::new(), &extra)
}

async fn handle_control(
  State(state): State<Arc<ServerState>>,
  Path((action, key)): Path<(String, String)>,
) -> Response<Body> {
  if action == "page" {
    return fx_html(&format!("<h1>Loading</h1><img src=\"/fx/control/hold/{key}\">"));
  }
  let signal = {
    let mut held = state.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held
      .entry(key)
      .or_insert_with(|| tokio::sync::watch::channel((false, false)).0)
      .clone()
  };
  let mut receiver = signal.subscribe();
  match action.as_str() {
    "hold" => {
      signal.send_modify(|s| s.0 = true);
      let _ = receiver.wait_for(|s| s.1).await;
    },
    "held" => {
      let _ = receiver.wait_for(|s| s.0).await;
    },
    "release" => signal.send_modify(|s| s.1 = true),
    _ => return fx_build(404, "text/plain", b"unknown control".to_vec(), &[]),
  }
  fx_text(&action)
}

async fn fx_slow(query: Option<&str>) -> Response<Body> {
  let ms: u64 = query
    .and_then(|q| {
      q.split('&')
        .find_map(|pair| pair.strip_prefix("ms=").and_then(|v| v.parse().ok()))
    })
    .unwrap_or(1000);
  tokio::time::sleep(std::time::Duration::from_millis(ms.min(30_000))).await;
  fx_text("slow")
}

fn fx_http_client(path: &str, headers: &HeaderMap, body: &axum::body::Bytes) -> Response<Body> {
  if let Some(count) = path.strip_prefix("redirect/") {
    let count = count.parse::<u32>().unwrap_or(0);
    return if count == 0 {
      fx_text("done")
    } else {
      fx_redirect(&format!("/fx/http-client/redirect/{}", count - 1))
    };
  }
  match path {
    "landed" => fx_text("LANDED"),
    "hop-offhost" => {
      let port = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .and_then(|host| host.rsplit(':').next())
        .and_then(|port| port.parse::<u16>().ok());
      match port {
        Some(port) => fx_redirect(&format!("http://localhost:{port}/fx/http-client/landed")),
        None => fx_build(400, "text/plain", b"missing port".to_vec(), &[]),
      }
    },
    "hop-metadata" => fx_redirect("http://169.254.169.254/latest"),
    "set" => fx_build(
      200,
      "text/plain",
      b"set".to_vec(),
      &[("set-cookie", "sid=abc; Path=/".into())],
    ),
    "echo" => fx_text(
      headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("none"),
    ),
    "body-echo" => fx_build(200, "text/plain", body.to_vec(), &[]),
    "ct-echo" => fx_text(
      headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("none"),
    ),
    _ => fx_text("ok"),
  }
}

async fn handle_fx(
  State(state): State<Arc<ServerState>>,
  Path(path): Path<String>,
  headers: HeaderMap,
  method: axum::http::Method,
  RawQuery(query): RawQuery,
  body: axum::body::Bytes,
) -> Response<Body> {
  match path.as_str() {
    _ if path.starts_with("http-client/") => fx_http_client(&path[12..], &headers, &body),
    "redirect" => fx_redirect("/fx/landed"),
    _ if path.starts_with("redirect/") => {
      let n: u32 = path
        .strip_prefix("redirect/")
        .and_then(|rest| rest.parse().ok())
        .unwrap_or(1);
      if n > 1 {
        fx_redirect(&format!("/fx/redirect/{}", n - 1))
      } else {
        fx_redirect("/fx/landed")
      }
    },
    "landed" => fx_text("landed"),
    "slow" => fx_slow(query.as_deref()).await,
    "api/users" => fx_json(&serde_json::json!({"users": ["alice", "bob"]})),
    "api/posts" => fx_json(&serde_json::json!({"posts": ["first"]})),
    "echo" => fx_build(200, "text/plain", body.to_vec(), &[]),
    "echo-headers" => fx_json(&serde_json::Value::Object(headers_json(&headers))),
    "echo-request" => fx_echo_request(&method, &headers, &body),
    _ if path.starts_with("compressed/") => fx_compressed(path.trim_start_matches("compressed/"), &headers),
    "multi-cookie" => fx_build(
      200,
      "text/plain",
      b"cookies-set".to_vec(),
      &[
        ("set-cookie", "a=1; Path=/".to_string()),
        ("set-cookie", "b=2; Path=/".to_string()),
      ],
    ),
    "har" => fx_build(
      200,
      "text/html",
      b"<!doctype html><title>HAR Fixture Title</title><body>backend-test</body>".to_vec(),
      &[("set-cookie", "harcookie=harvalue; Path=/".to_string())],
    ),
    "set-cookie" => fx_set_cookie(query.as_deref()),
    "set-cookie-redirect" => fx_set_cookie_redirect(query.as_deref()),
    "auth" => {
      let authed = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok())
        .is_some_and(|creds| creds == b"user:pass");
      if authed {
        fx_html("AUTHED")
      } else {
        fx_build(
          401,
          "text/html",
          b"NOAUTH".to_vec(),
          &[("www-authenticate", "Basic realm=\"fx\"".to_string())],
        )
      }
    },
    "csp" => fx_build(
      200,
      "text/html",
      b"<!doctype html><body>csp</body>".to_vec(),
      &[("content-security-policy", "script-src 'none'".to_string())],
    ),
    "download" => fx_build(
      200,
      "application/octet-stream",
      b"fx-download-payload".to_vec(),
      &[(
        "content-disposition",
        "attachment; filename=\"greeting.txt\"".to_string(),
      )],
    ),
    "download-hang" => fx_download_hang(),
    "iframe" => fx_html("<!doctype html><body>outer<iframe src=\"/fx/inner\"></iframe></body>"),
    "inner" => fx_html("<!doctype html><body>inner</body>"),
    "proxy-info" => fx_json(&serde_json::json!({"url": format!("http://{}", state.proxy.addr)})),
    // Origins of the auxiliary listeners, which bind ephemeral ports.
    "endpoints" => fx_json(&serde_json::json!({
      "proxy": format!("http://{}", state.proxy.addr),
      "reset": format!("http://{}", state.reset_addr),
      "tls": format!("https://{}", state.tls_addr),
    })),
    "reset-arm" => fx_reset_arm(&state.reset, query.as_deref()),
    "proxy-log" => fx_proxy_log(&state.proxy, &method, query.as_deref()),
    _ => fx_page_quality(&path, query.as_deref()).unwrap_or_else(fx_unknown_route),
  }
}

fn fx_proxy_log(state: &ProxyState, method: &axum::http::Method, query: Option<&str>) -> Response<Body> {
  let mut lines = state.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  if method == axum::http::Method::DELETE {
    if let Some(key) = query_values(query, "key").first() {
      lines.retain(|line| {
        line
          .split_whitespace()
          .nth(1)
          .and_then(|url| url.split_once('?'))
          .is_none_or(|(_, query)| !query_values(Some(query), "key").contains(key))
      });
    } else {
      lines.clear();
    }
  }
  fx_json(&serde_json::json!({"hits": lines.len(), "lines": *lines}))
}

fn fx_unknown_route() -> Response<Body> {
  fx_build(404, "text/plain", b"unknown fixture route".to_vec(), &[])
}

/// The routes `page.checkPageQuality()` needs, which are the ones where
/// the RESPONSE rather than the page is what an audit reads.
fn fx_page_quality(path: &str, query: Option<&str>) -> Option<Response<Body>> {
  // `http-status-code` reads the main document's status and nothing
  // else, so a page has to be servable with an arbitrary one.
  if let Some(code) = path.strip_prefix("status/") {
    let code = code.parse::<u16>().ok()?;
    return Some(fx_build(
      code,
      "text/html",
      fx_auditable("Served with a status").into_bytes(),
      &[],
    ));
  }
  match path {
    // Each `?tag=` becomes its own `X-Robots-Tag`: a repeated header is
    // one directive apiece, and `is-crawlable` reads them separately.
    "robots-header" => {
      let tags: Vec<(&str, String)> = query_values(query, "tag")
        .into_iter()
        .map(|tag| ("x-robots-tag", tag))
        .collect();
      Some(fx_build(
        200,
        "text/html",
        fx_auditable("Blocked by a header").into_bytes(),
        &tags,
      ))
    },
    // The path `tests/assets/robots.txt` disallows. Nothing on the page
    // objects to being indexed; the file does.
    "robots-blocked" => Some(fx_html(&fx_auditable("Disallowed by robots.txt"))),
    _ => None,
  }
}

async fn ws_upgrade(ws: WebSocketUpgrade) -> Response<Body> {
  ws.on_upgrade(ws_echo)
}

async fn ws_echo(mut socket: WebSocket) {
  while let Some(Ok(msg)) = socket.recv().await {
    let reply = match msg {
      WsMessage::Text(t) => WsMessage::Text(t),
      WsMessage::Binary(b) => WsMessage::Binary(b),
      WsMessage::Close(_) => break,
      // Ping/Pong are answered by the protocol layer.
      _ => continue,
    };
    if socket.send(reply).await.is_err() {
      break;
    }
  }
}

/// Accept loop for the listener that aborts connections.
///
/// While a key has budget left (armed via `/fx/reset-arm`), the
/// connection is closed with `SO_LINGER 0`, which sends a TCP RST rather
/// than a FIN — the client sees ECONNRESET, the one error class
/// Playwright's `maxRetries` retries. Once the budget is spent the same
/// URL answers 200, so a test can prove that N retries turn failure into
/// success and that N-1 do not.
async fn run_reset(listener: tokio::net::TcpListener, state: Arc<ResetState>) {
  loop {
    let Ok((mut stream, _)) = listener.accept().await else {
      break;
    };
    let state = Arc::clone(&state);
    tokio::spawn(async move {
      use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
      let mut buf = [0u8; 8192];
      let n = stream.read(&mut buf).await.unwrap_or(0);
      let request = String::from_utf8_lossy(&buf[..n]);
      let target = request.lines().next().unwrap_or("").split_whitespace().nth(1);
      let key = target
        .and_then(|t| t.split_once('?'))
        .map(|(_, q)| q)
        .and_then(|q| query_values(Some(q), "key").into_iter().next())
        .unwrap_or_default();

      let should_reset = {
        let mut remaining = state
          .remaining
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        match remaining.get_mut(&key) {
          Some(left) if *left > 0 => {
            *left -= 1;
            true
          },
          _ => false,
        }
      };

      if should_reset {
        // Zero linger turns the close into an RST instead of a FIN, which
        // is what makes the client report ECONNRESET. Set through socket2
        // on the raw socket: tokio deprecates its own `set_linger`.
        if let Ok(std_stream) = stream.into_std() {
          let socket = socket2::Socket::from(std_stream);
          let _ = socket.set_linger(Some(std::time::Duration::ZERO));
          drop(socket);
        }
        return;
      }
      let _ = stream
        .write_all(
          b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 9\r\nConnection: close\r\n\r\nsurvived!",
        )
        .await;
      let _ = stream.flush().await;
    });
  }
}

/// Accept loop for the HTTPS listener, holding a certificate generated
/// at startup for `127.0.0.1` and signed by nobody — so every client
/// rejects it unless it is explicitly told to ignore certificate errors.
async fn run_tls(listener: tokio::net::TcpListener) {
  let Ok(cert) = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string(), "localhost".to_string()]) else {
    return;
  };
  let certs = vec![cert.cert.der().clone()];
  let key = tokio_rustls::rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der());
  let Ok(key) = key else { return };
  // Name the crypto provider rather than taking the process default:
  // cargo unifies rustls features across the workspace build, so both
  // `ring` (here) and `aws-lc-rs` (reqwest's) end up enabled and the
  // automatic choice panics with "Could not automatically determine the
  // process-level CryptoProvider".
  let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
  let Ok(builder) =
    tokio_rustls::rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions()
  else {
    return;
  };
  let Ok(config) = builder.with_no_client_auth().with_single_cert(certs, key) else {
    return;
  };
  let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

  loop {
    let Ok((stream, _)) = listener.accept().await else {
      break;
    };
    let acceptor = acceptor.clone();
    tokio::spawn(async move {
      use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
      let Ok(mut tls) = acceptor.accept(stream).await else {
        // Handshake rejected by the client (the untrusted-cert case).
        return;
      };
      let mut buf = [0u8; 8192];
      let _ = tls.read(&mut buf).await;
      let _ = tls
        .write_all(
          b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 9\r\nConnection: close\r\n\r\nsecured!!",
        )
        .await;
      let _ = tls.flush().await;
    });
  }
}

/// Accept loop for the proxy listener: answers every request with a
/// canned page and records the request line. One task per connection —
/// browsers (`WebKit` especially) open speculative preconnections that
/// carry no request for up to ~60s, and a serial accept loop would
/// starve real requests behind them.
async fn run_proxy(listener: tokio::net::TcpListener, state: Arc<ProxyState>) {
  loop {
    let Ok((stream, _)) = listener.accept().await else {
      break;
    };
    let state = Arc::clone(&state);
    tokio::spawn(async move {
      use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
      let mut stream = stream;
      let (read_half, mut write_half) = stream.split();
      let mut reader = BufReader::new(read_half);
      let mut request_line = String::new();
      if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
        return;
      }
      loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
          return;
        }
        if line == "\r\n" || line == "\n" {
          break;
        }
      }
      state
        .lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(request_line.trim_end().to_string());
      let body = "<!doctype html><body>PROXY:ok</body>";
      let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      let _ = write_half.write_all(resp.as_bytes()).await;
      let _ = write_half.shutdown().await;
    });
  }
}
