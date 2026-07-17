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
  handle: tokio::task::JoinHandle<()>,
  proxy_handle: tokio::task::JoinHandle<()>,
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

    let state = Arc::new(ServerState { proxy });
    let mut app = Router::new()
      // Literal route wins over the wildcard, so the WS upgrade
      // extractor only runs for the echo endpoint.
      .route("/fx/ws", any(ws_upgrade))
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
      handle,
      proxy_handle,
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

  /// Serve until aborted.
  pub async fn run_forever(self) {
    let _ = tokio::join!(self.handle, self.proxy_handle);
  }

  pub async fn stop(self) {
    self.handle.abort();
    self.proxy_handle.abort();
    let _ = self.handle.await;
    let _ = self.proxy_handle.await;
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
  body: axum::body::Bytes,
) -> Response<Body> {
  let body_text = String::from_utf8_lossy(&body).to_string();
  let parsed_json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
  fx_json(&serde_json::json!({
    "url": format!("/_api/{path}"),
    "method": method.to_string(),
    "headers": headers_json(&headers),
    "data": body_text,
    "json": parsed_json,
  }))
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
    "api/users" => fx_json(&serde_json::json!({"users": ["alice", "bob"]})),
    "api/posts" => fx_json(&serde_json::json!({"posts": ["first"]})),
    "echo" => fx_build(200, "text/plain", body.to_vec(), &[]),
    "echo-headers" => fx_json(&serde_json::Value::Object(headers_json(&headers))),
    "multi-cookie" => fx_build(
      200,
      "text/plain",
      b"cookies-set".to_vec(),
      &[
        ("set-cookie", "a=1; Path=/".to_string()),
        ("set-cookie", "b=2; Path=/".to_string()),
      ],
    ),
    // Each `c` query param becomes one Set-Cookie header verbatim:
    // `/fx/set-cookie?c=name%3Dvalue%3B%20Path%3D%2F`.
    "set-cookie" => {
      let cookies: Vec<(&str, String)> = query_values(query.as_deref(), "c")
        .into_iter()
        .map(|c| ("set-cookie", c))
        .collect();
      fx_build(200, "text/plain", b"cookie-set".to_vec(), &cookies)
    },
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
    "proxy-log" => {
      let mut lines = state
        .proxy
        .lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if method == axum::http::Method::DELETE {
        lines.clear();
      }
      fx_json(&serde_json::json!({"hits": lines.len(), "lines": *lines}))
    },
    _ => fx_build(404, "text/plain", b"unknown fixture route".to_vec(), &[]),
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
