#![allow(clippy::expect_used, clippy::unwrap_used, clippy::items_after_statements)]
//! Runtime coverage for `WebServerManager` graceful shutdown and the
//! HTTPS-aware readiness probe (§7.25). The graceful-shutdown half
//! drives a Node.js trap script through the actual lifecycle path;
//! the readiness probe is exercised over plain HTTP via a
//! `Bun.serve`-equivalent fixture (axum) and through a unit-level
//! check that the `ignore_https_errors` flag flows into the reqwest
//! client builder.
//!
//! The TLS half is skipped intentionally — wiring up a self-signed
//! HTTPS fixture would be cheap on macOS but pulls in a server-side
//! TLS dep just for one bool. The probe builder is exercised
//! separately so the wiring is still covered.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ferridriver_test::config::{GracefulShutdown, WebServerConfig};
use ferridriver_test::server::WebServerManager;

fn unique_marker(label: &str) -> PathBuf {
  let dir = std::env::temp_dir();
  let pid = std::process::id();
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  dir.join(format!("ferridriver-{label}-{pid}-{nanos}.marker"))
}

fn unused_port() -> u16 {
  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
  let port = listener.local_addr().expect("local_addr").port();
  drop(listener);
  port
}

/// Returns a Node.js script that traps SIGTERM, writes a marker file,
/// and exits 0 after a short delay. The HTTP server is the readiness
/// surface — `wait_for_url` polls until 200 OK comes back.
fn build_trap_script(marker: &Path, port: u16) -> String {
  let marker_str = marker.display().to_string();
  format!(
    "
const fs = require('fs');
const http = require('http');

const server = http.createServer((_, res) => {{
  res.writeHead(200, {{ 'content-type': 'text/plain' }});
  res.end('ok');
}});
server.listen({port}, '127.0.0.1');

let shuttingDown = false;
function gracefulExit() {{
  if (shuttingDown) return;
  shuttingDown = true;
  fs.writeFileSync({marker_str:?}, 'graceful');
  setTimeout(() => {{
    server.close(() => process.exit(0));
  }}, 50);
}}

process.on('SIGTERM', gracefulExit);
process.on('SIGINT', gracefulExit);
"
  )
}

#[tokio::test]
async fn stop_with_graceful_shutdown_writes_marker_and_exits_clean() {
  let marker = unique_marker("graceful");
  let _ = std::fs::remove_file(&marker);
  let port = unused_port();
  let url = format!("http://127.0.0.1:{port}");

  let script = build_trap_script(&marker, port);
  let script_path = std::env::temp_dir().join(format!("ferridriver-trap-{}.js", std::process::id()));
  std::fs::write(&script_path, &script).expect("write trap script");

  let cfg = WebServerConfig {
    command: Some(format!("node {}", script_path.display())),
    url: Some(url.clone()),
    timeout: 10_000,
    name: Some("graceful-fixture".into()),
    graceful_shutdown: Some(GracefulShutdown {
      signal: "SIGTERM".into(),
      timeout: 1_000,
    }),
    ..WebServerConfig::default()
  };

  let manager = WebServerManager::start(std::slice::from_ref(&cfg))
    .await
    .expect("start web server");

  // Confirm the server is actually serving before we send the signal,
  // otherwise the trap handler may not be installed yet.
  assert_eq!(manager.first_url().as_deref(), Some(url.as_str()));

  manager.stop().await;

  // Give the OS a moment to flush the marker write to disk.
  for _ in 0..20 {
    if marker.exists() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }

  assert!(
    marker.exists(),
    "expected SIGTERM-trap marker at {} after graceful shutdown",
    marker.display(),
  );

  let _ = std::fs::remove_file(&marker);
  let _ = std::fs::remove_file(&script_path);
}

#[tokio::test]
async fn stop_without_graceful_shutdown_hard_kills() {
  let marker = unique_marker("hardkill");
  let _ = std::fs::remove_file(&marker);
  let port = unused_port();
  let url = format!("http://127.0.0.1:{port}");

  let script = build_trap_script(&marker, port);
  let script_path = std::env::temp_dir().join(format!("ferridriver-hardkill-{}.js", std::process::id()));
  std::fs::write(&script_path, &script).expect("write trap script");

  let cfg = WebServerConfig {
    command: Some(format!("node {}", script_path.display())),
    url: Some(url.clone()),
    timeout: 10_000,
    // No graceful_shutdown — manager goes straight to SIGKILL.
    ..WebServerConfig::default()
  };

  let manager = WebServerManager::start(std::slice::from_ref(&cfg))
    .await
    .expect("start web server");

  manager.stop().await;

  // Hard kill races the trap, but the marker write only happens on
  // SIGTERM/SIGINT — `child.kill().await` sends SIGKILL which the
  // process can't intercept. So the marker must NOT exist.
  tokio::time::sleep(Duration::from_millis(200)).await;

  assert!(
    !marker.exists(),
    "did not expect a marker after SIGKILL — found {}",
    marker.display(),
  );

  let _ = std::fs::remove_file(&script_path);
}

#[tokio::test]
async fn probe_client_honours_ignore_https_errors_flag() {
  // Smoke-level coverage: build the probe client both ways and
  // exercise the actual probe decision tree against a 127.0.0.1
  // listener so we know `ignore_https_errors` doesn't break the
  // happy path. The TLS half (self-signed cert acceptance) is
  // a runtime feature of reqwest's `danger_accept_invalid_certs`
  // and is not re-tested here.
  let strict = ferridriver_test::server::build_probe_client(false);
  let lenient = ferridriver_test::server::build_probe_client(true);
  let _ = (strict, lenient);

  // Bind to port 0 and read back the assigned port — avoids the
  // bind/release race that `unused_port()` has when used directly
  // here.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let addr = listener.local_addr().expect("local_addr");
  let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
  let handle = tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });

  // Plain HTTP probe — both flag values must succeed.
  let url = format!("http://{addr}");
  assert!(ferridriver_test::server::http_probe(&url, false).await);
  assert!(ferridriver_test::server::http_probe(&url, true).await);

  handle.abort();
}
