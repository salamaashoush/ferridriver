//! WebSocket lifecycle test for `page.waitForEvent('websocket')` +
//! frame events. The rest of the Request/Response/route surface lives
//! in `tests/e2e/network.test.ts`, run by `ferridriver test`.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;
use std::net::TcpListener;
use std::thread;

/// Open a tungstenite-backed WebSocket echo server, navigate to a page
/// that connects to it, send a frame, and assert
/// `webSocket.waitForEvent('framereceived')` delivers the echoed payload.
///
/// CDP exposes `Network.webSocketFrameSent` / `Received`, so this test
/// runs end-to-end on cdp-pipe / cdp-raw. BiDi and WebKit do not yet
/// surface WebSocket frame events in their respective protocols /
/// public APIs — Playwright's own backends mirror the same gap; we
/// assert that `page.waitForEvent('websocket', { timeoutMs })` rejects
/// with a typed Timeout there rather than silently dangling.
pub fn test_network_websocket(c: &mut McpClient) {
  if c.backend == "bidi" || c.backend == "webkit" {
    c.nav_url("about:blank");
    let script = "const r = await page.waitForEvent('websocket', 500).catch(e => ({ error: String(e) }));\
                  return r && r.error ? { error: r.error } : { ok: true };";
    let v = c.script_value(script);
    assert!(
      v["error"].as_str().is_some_and(|s| {
        let lc = s.to_ascii_lowercase();
        lc.contains("timeout") || lc.contains("waiting for event")
      }),
      "BiDi/WebKit WebSocket should reject with typed timeout: {v}",
    );
    return;
  }

  let (ws_url, ws_stop) = spawn_ws_echo();
  let (http_url, http_stop) = spawn_http_blank();
  // Navigate to a real http origin first — Chromium 100+ blocks
  // loopback connections from opaque-origin pages (about:blank,
  // data:) under Private Network Access. Under CI sandboxing the
  // block manifests as a hung CDP Runtime.evaluate instead of a
  // fast error. A same-origin http://127.0.0.1 page satisfies the
  // PNA same-origin path.
  c.nav_url(&http_url);
  let script = format!(
    r#"
    const wsPromise = page.waitForEvent('websocket', 10000);
    await page.evaluate(`
      window.__ws = new WebSocket({ws_url:?});
      window.__opened = new Promise((res) => {{ window.__ws.onopen = () => res(); }});
      null
    `);
    const ws = await wsPromise;
    const recvPromise = ws.waitForEvent('framereceived', 10000);
    await page.evaluate("window.__opened.then(() => window.__ws.send('hello-ws')); null");
    const frame = await recvPromise;
    return {{
      url: ws.url(),
      payload: frame.payload,
      isClosed: ws.isClosed(),
    }};
    "#,
    ws_url = ws_url,
  );
  let v = c.script_value(&script);
  assert!(v["url"].as_str().is_some_and(|s| s.starts_with("ws://")), "ws url: {v}");
  assert_eq!(v["payload"].as_str(), Some("hello-ws"), "echoed payload: {v}");
  let _ = ws_stop.send(());
  let _ = http_stop.send(());
}

/// Spawn a tiny HTTP server that responds 200 OK to any request.
/// Used to give the test page a real http origin so Private Network
/// Access doesn't block subsequent loopback WebSocket connections.
fn spawn_http_blank() -> (String, std::sync::mpsc::Sender<()>) {
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener as TokioListener;

  let std_listener = TcpListener::bind("127.0.0.1:0").expect("http bind");
  let addr = std_listener.local_addr().expect("addr");
  let url = format!("http://{addr}/");
  std_listener.set_nonblocking(true).expect("nonblocking");
  let (tx, rx) = std::sync::mpsc::channel::<()>();

  thread::spawn(move || {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("http runtime");
    runtime.block_on(async move {
      let listener = TokioListener::from_std(std_listener).expect("from_std");
      loop {
        if rx.try_recv().is_ok() {
          break;
        }
        tokio::select! {
          accept = listener.accept() => {
            if let Ok((mut stream, _)) = accept {
              tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let body = b"<!doctype html><title>ferri</title>";
                let resp = format!(
                  "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n",
                  body.len(),
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.shutdown().await;
              });
            }
          },
          () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
        }
      }
    });
  });

  (url, tx)
}

/// Spawn a tokio-tungstenite echo server. Returns `(ws_url, stop_sender)`;
/// sending on the stop channel ends the listener task.
fn spawn_ws_echo() -> (String, std::sync::mpsc::Sender<()>) {
  use futures::{SinkExt, StreamExt};
  use tokio::net::TcpListener as TokioListener;

  // Bind synchronously to grab the port, then transfer the std listener
  // into the tokio runtime spawned on a worker thread.
  let std_listener = TcpListener::bind("127.0.0.1:0").expect("ws bind");
  let addr = std_listener.local_addr().expect("addr");
  let url = format!("ws://{addr}/");
  std_listener.set_nonblocking(true).expect("nonblocking");
  let (tx, rx) = std::sync::mpsc::channel::<()>();

  thread::spawn(move || {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("ws runtime");
    runtime.block_on(async move {
      let listener = TokioListener::from_std(std_listener).expect("from_std");
      loop {
        if rx.try_recv().is_ok() {
          break;
        }
        tokio::select! {
          accept = listener.accept() => {
            if let Ok((stream, _)) = accept {
              tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                while let Some(Ok(msg)) = ws.next().await {
                  if msg.is_text() || msg.is_binary() {
                    let _ = ws.send(msg).await;
                  }
                  if ws.send(tokio_tungstenite::tungstenite::Message::Close(None)).await.is_ok() {
                    break;
                  }
                }
              });
            }
          },
          () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
        }
      }
    });
  });

  (url, tx)
}
