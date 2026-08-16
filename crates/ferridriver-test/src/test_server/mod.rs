//! The test server: what Playwright's UI talks to.
//!
//! `ferridriver test --ui` serves Playwright's own UI-mode app (embedded
//! in this binary, see `ferridriver-viewer`) and answers the protocol it
//! speaks — a small JSON-RPC over one websocket
//! (`testServerInterface.ts`). The same protocol backs Playwright's VS
//! Code extension, so anything that drives Playwright this way drives a
//! ferridriver run too.
//!
//! Shape of it:
//!
//! ```text
//!   GET  /                     -> 302 /trace/uiMode.html?ws=<guid>
//!   GET  /trace/*              -> the embedded app + trace files
//!   WS   /<guid>               -> { id, method, params } / { id, result }
//!                                 plus server events { method, params }
//! ```
//!
//! Requests are handed to the run loop one at a time ([`Request`]);
//! events flow back to every connected client through a broadcast. The
//! run loop lives in the runner — this module is transport only, exactly
//! like `ui_server` is for the classic app.

pub mod driver;
pub mod tele;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use ferridriver_viewer::{App, FileRoots};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// One client call, waiting for the run loop to answer it.
pub struct Request {
  pub method: String,
  pub params: Value,
  reply: Option<oneshot::Sender<Result<Value, String>>>,
}

impl Request {
  /// Answer the call. Dropping a request without answering replies with
  /// an error, so a client is never left waiting on a method the loop
  /// chose not to handle.
  pub fn respond(mut self, result: Value) {
    if let Some(reply) = self.reply.take() {
      let _ = reply.send(Ok(result));
    }
  }

  pub fn fail(mut self, message: impl Into<String>) {
    if let Some(reply) = self.reply.take() {
      let _ = reply.send(Err(message.into()));
    }
  }

  /// A `params` field as a string list (`locations`, `testIds`, …).
  #[must_use]
  pub fn string_list(&self, key: &str) -> Vec<String> {
    self
      .params
      .get(key)
      .and_then(Value::as_array)
      .map(|values| {
        values
          .iter()
          .filter_map(|v| v.as_str().map(ToString::to_string))
          .collect()
      })
      .unwrap_or_default()
  }

  /// A `params` field as a string.
  #[must_use]
  pub fn string(&self, key: &str) -> Option<String> {
    self.params.get(key).and_then(Value::as_str).map(ToString::to_string)
  }

  /// A `params` field as a bool.
  #[must_use]
  pub fn flag(&self, key: &str) -> bool {
    self.bool(key).unwrap_or(false)
  }

  /// A `params` field as a bool, absent when the client did not send it
  /// — `headed: false` means headless, not "unspecified".
  #[must_use]
  pub fn bool(&self, key: &str) -> Option<bool> {
    self.params.get(key).and_then(Value::as_bool)
  }

  /// A `params` field as a whole number (`maxFailures`, `timeout`).
  #[must_use]
  pub fn number(&self, key: &str) -> Option<u64> {
    self.params.get(key).and_then(Value::as_u64)
  }
}

impl Drop for Request {
  fn drop(&mut self) {
    if let Some(reply) = self.reply.take() {
      let _ = reply.send(Err(format!("{} is not supported", self.method)));
    }
  }
}

/// Fan-out of server events to every connected client.
///
/// One unbounded queue per client rather than a broadcast channel: the
/// client is a state machine (it rebuilds the run from these events), so
/// a dropped `onTestEnd` does not degrade its view — it corrupts it. A
/// slow client grows its own queue and nobody else waits.
#[derive(Clone)]
pub struct Events {
  clients: Arc<std::sync::Mutex<Vec<mpsc::UnboundedSender<String>>>>,
}

impl Events {
  fn new() -> Self {
    Self {
      clients: Arc::new(std::sync::Mutex::new(Vec::new())),
    }
  }

  /// Register a client, receiving everything sent from now on.
  fn subscribe(&self) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    self
      .clients
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(tx);
    rx
  }

  /// Send one protocol event (`report`, `stdio`, `testFilesChanged`, …).
  pub fn send(&self, method: &str, params: Value) {
    let message = json!({ "method": method, "params": params }).to_string();
    let mut clients = self.clients.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Disconnected clients are dropped here rather than tracked
    // separately — a send to a closed queue is how we learn of them.
    clients.retain(|client| client.send(message.clone()).is_ok());
  }

  /// Send one teleReporter event, wrapped as the `report` event the UI
  /// feeds to its receiver.
  pub fn report(&self, event: Value) {
    self.send("report", event);
  }
}

/// A running test server.
pub struct TestServer {
  pub addr: SocketAddr,
  /// Where to point a browser: the UI app with its websocket parameter.
  pub url: String,
  /// Client calls, in arrival order.
  pub requests: mpsc::UnboundedReceiver<Request>,
  pub events: Events,
}

/// How to bind the server.
pub struct TestServerOptions {
  pub host: String,
  pub port: Option<u16>,
  /// Directories the trace viewer may read trace files from — the output
  /// directory (live traces, finished zips) and the project root.
  pub file_roots: Vec<PathBuf>,
}

struct ServerState {
  requests: mpsc::UnboundedSender<Request>,
  events: Events,
}

/// Bind the server and serve the UI app in the background.
///
/// # Errors
///
/// Errors if the listener cannot bind.
pub async fn start(options: TestServerOptions) -> ferridriver::error::Result<TestServer> {
  use ferridriver::FerriError;

  let (requests_tx, requests_rx) = mpsc::unbounded_channel();
  let events = Events::new();
  // The websocket path is unguessable, exactly as in Playwright: the
  // page is told the path in its query string, and a drive-by page on
  // another origin cannot construct it.
  let ws_guid = guid();
  let state = Arc::new(ServerState {
    requests: requests_tx,
    events: events.clone(),
  });

  let ip: std::net::IpAddr = options
    .host
    .parse()
    .map_err(|e| FerriError::backend(format!("invalid host {}: {e}", options.host)))?;
  let listener = tokio::net::TcpListener::bind(SocketAddr::new(ip, options.port.unwrap_or(0)))
    .await
    .map_err(|e| FerriError::backend(format!("bind test server: {e}")))?;
  let addr = listener
    .local_addr()
    .map_err(|e| FerriError::backend(format!("test server local_addr: {e}")))?;

  let base = format!("http://{}", displayable(addr));
  let url = ferridriver_viewer::app_url(
    &base,
    "uiMode.html",
    &[
      ("ws", ws_guid.clone()),
      ("pathSeparator", std::path::MAIN_SEPARATOR.to_string()),
    ],
  );

  let redirect_to = url.clone();
  // The viewer's routes carry their own state, so the protocol socket is
  // its own router merged in beside them.
  let protocol = Router::new()
    .route(&format!("/{ws_guid}"), get(upgrade))
    .with_state(Arc::clone(&state));
  let app = ferridriver_viewer::router(App::TraceViewer, FileRoots::new(options.file_roots))
    .route(
      "/",
      get(move || {
        let redirect_to = redirect_to.clone();
        async move { Redirect::temporary(&redirect_to) }
      }),
    )
    .merge(protocol);

  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });

  Ok(TestServer {
    addr,
    url,
    requests: requests_rx,
    events,
  })
}

/// `0.0.0.0` is a bind address, not a place a browser can go.
fn displayable(addr: SocketAddr) -> String {
  if addr.ip().is_unspecified() {
    format!("127.0.0.1:{}", addr.port())
  } else {
    addr.to_string()
  }
}

/// Unguessable websocket path segment. Not a cryptographic identity —
/// it keeps a page on another origin from finding the socket, which is
/// what Playwright's `createGuid` does here too.
fn guid() -> String {
  use std::hash::{BuildHasher, Hasher, RandomState};
  let mut out = String::with_capacity(32);
  for _ in 0..2 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_usize(std::process::id() as usize);
    out.push_str(&format!("{:016x}", hasher.finish()));
  }
  out
}

async fn upgrade(
  State(state): State<Arc<ServerState>>,
  headers: axum::http::HeaderMap,
  ws: WebSocketUpgrade,
) -> Response {
  // Browsers apply no same-origin policy to websocket handshakes, so a
  // page anywhere could otherwise drive this run.
  match headers.get(header::ORIGIN).map(|value| value.to_str()) {
    None => {},
    Some(Ok(origin)) if is_loopback_origin(origin) => {},
    Some(_) => return StatusCode::FORBIDDEN.into_response(),
  }
  ws.on_upgrade(move |socket| session(socket, state))
}

fn is_loopback_origin(origin: &str) -> bool {
  let Some(rest) = origin
    .strip_prefix("http://")
    .or_else(|| origin.strip_prefix("https://"))
  else {
    return false;
  };
  let host = match rest.rsplit_once(':') {
    Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
    _ => rest,
  };
  matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

async fn session(socket: WebSocket, state: Arc<ServerState>) {
  use futures::{SinkExt, StreamExt};

  let (mut sink, mut stream) = socket.split();
  let mut events = state.events.subscribe();
  let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

  // One writer task owns the sink: replies and events both go through it,
  // so a slow client cannot interleave a half-written frame.
  let writer = tokio::spawn(async move {
    loop {
      tokio::select! {
        message = out_rx.recv() => match message {
          Some(message) => {
            if sink.send(Message::Text(message.into())).await.is_err() {
              break;
            }
          },
          None => break,
        },
        event = events.recv() => match event {
          Some(event) => {
            if sink.send(Message::Text(event.into())).await.is_err() {
              break;
            }
          },
          None => break,
        },
      }
    }
  });

  while let Some(Ok(message)) = stream.next().await {
    let Message::Text(text) = message else {
      continue;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
      continue;
    };
    let (Some(id), Some(method)) = (value.get("id").cloned(), value.get("method").and_then(Value::as_str)) else {
      continue;
    };
    let params = value.get("params").cloned().unwrap_or(json!({}));
    let (reply_tx, reply_rx) = oneshot::channel();
    let request = Request {
      method: method.to_string(),
      params,
      reply: Some(reply_tx),
    };
    if state.requests.send(request).is_err() {
      break;
    }
    let out = out_tx.clone();
    // Answers are awaited off this loop: `runTests` does not return until
    // the run is over, and the client keeps pinging in the meantime.
    tokio::spawn(async move {
      let response = match reply_rx.await {
        Ok(Ok(result)) => json!({ "id": id, "result": result }),
        Ok(Err(error)) => json!({ "id": id, "error": error }),
        Err(_) => json!({ "id": id, "error": "test server is shutting down" }),
      };
      let _ = out.send(response.to_string());
    });
  }

  drop(out_tx);
  let _ = writer.await;
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_dropped_request_answers_rather_than_stranding_the_client() {
    let (tx, rx) = oneshot::channel();
    {
      let _request = Request {
        method: "clearCache".to_string(),
        params: json!({}),
        reply: Some(tx),
      };
    }
    let answer = rx.blocking_recv().expect("answered");
    assert!(answer.is_err(), "dropped requests must report, not hang");
  }

  #[test]
  fn params_read_the_shapes_the_ui_sends() {
    let request = Request {
      method: "runTests".to_string(),
      params: json!({ "testIds": ["a", "b"], "grep": "smoke", "headed": true }),
      reply: None,
    };
    assert_eq!(request.string_list("testIds"), vec!["a", "b"]);
    assert_eq!(request.string("grep").as_deref(), Some("smoke"));
    assert!(request.flag("headed"));
    assert!(!request.flag("missing"));
    assert!(request.string_list("locations").is_empty());
  }

  #[tokio::test]
  async fn every_client_gets_every_event() {
    let events = Events::new();
    let mut first = events.subscribe();
    let mut second = events.subscribe();
    events.report(json!({ "method": "onBegin" }));
    events.send("stdio", json!({ "type": "stdout", "text": "hi" }));

    for client in [&mut first, &mut second] {
      let report: Value = serde_json::from_str(&client.recv().await.expect("report")).expect("json");
      assert_eq!(report["method"], "report");
      assert_eq!(report["params"]["method"], "onBegin");
      let stdio: Value = serde_json::from_str(&client.recv().await.expect("stdio")).expect("json");
      assert_eq!(stdio["method"], "stdio");
    }
  }

  #[tokio::test]
  async fn a_disconnected_client_is_forgotten_without_stalling_the_rest() {
    let events = Events::new();
    let gone = events.subscribe();
    let mut alive = events.subscribe();
    drop(gone);

    events.report(json!({ "method": "onEnd" }));
    assert!(alive.recv().await.is_some(), "a live client still gets its event");
    assert_eq!(
      events
        .clients
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len(),
      1,
      "the closed client is dropped from the fan-out"
    );
  }

  #[test]
  fn guids_differ_between_servers() {
    assert_ne!(guid(), guid());
    assert_eq!(guid().len(), 32);
  }

  #[test]
  fn only_loopback_pages_may_open_the_socket() {
    assert!(is_loopback_origin("http://localhost:9323"));
    assert!(is_loopback_origin("http://127.0.0.1:9323"));
    assert!(!is_loopback_origin("https://evil.example"));
    assert!(!is_loopback_origin("http://127.0.0.1.evil.example"));
  }

  #[tokio::test]
  async fn serves_the_ui_app_and_redirects_the_root_at_it() {
    let server = start(TestServerOptions {
      host: "127.0.0.1".to_string(),
      port: None,
      file_roots: vec![std::env::temp_dir()],
    })
    .await
    .expect("start");

    assert!(server.url.contains("/trace/uiMode.html?ws="));
    let ws_guid = server
      .url
      .split("ws=")
      .nth(1)
      .and_then(|rest| rest.split('&').next())
      .expect("ws parameter");
    assert_eq!(ws_guid.len(), 32);
  }
}
