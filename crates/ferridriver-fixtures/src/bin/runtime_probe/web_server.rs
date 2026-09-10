use std::path::Path;

use anyhow::{Context, Result};
use ferridriver_fixtures::{FixtureServer, FixtureServerOptions};
use ferridriver_test::config::{GracefulShutdown, WebServerConfig};
use ferridriver_test::server::{WebServerManager, http_probe};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Request {
  signal: Option<String>,
  #[serde(default)]
  noisy: bool,
  failure: Option<String>,
}

pub async fn shutdown(root: &Path, request: Request) -> Result<Value> {
  if request.failure.as_deref() == Some("reuse") {
    return reuse().await;
  }
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  let url = format!("http://127.0.0.1:{port}");
  let executable = std::env::current_exe()?.display().to_string().replace('\'', "'\\''");
  let mut config = WebServerConfig {
    command: Some(format!("'{executable}' --web-server-fixture")),
    url: Some(url.clone()),
    timeout: 2_000,
    env: [
      ("PROBE_PORT".into(), port.to_string()),
      (
        "PROBE_MARKER".into(),
        root.join("shutdown-marker").display().to_string(),
      ),
      ("PROBE_NOISY".into(), request.noisy.to_string()),
    ]
    .into(),
    graceful_shutdown: request.signal.map(|signal| GracefulShutdown { signal, timeout: 1_000 }),
    ..Default::default()
  };
  if let Some(failure) = &request.failure {
    if matches!(failure.as_str(), "stall" | "unavailable") {
      config.url = Some(format!("{url}/{failure}"));
    }
    if failure == "early-exit" {
      config.env.insert("PROBE_EXIT".into(), "7".into());
    }
    config.timeout = 400;
  }
  let mut configs = vec![config];
  match request.failure.as_deref() {
    Some("later-invalid") => configs.push(WebServerConfig::default()),
    Some("later-spawn") => configs.push(WebServerConfig {
      command: Some("unreachable-command".into()),
      url: Some(url.clone()),
      cwd: Some(root.join("missing-directory").display().to_string()),
      ..Default::default()
    }),
    _ => {},
  }
  drop(listener);
  let started = std::time::Instant::now();
  let result = WebServerManager::start(&configs).await;
  let elapsed_ms = started.elapsed().as_millis();
  let manager = match result {
    Ok(manager) => manager,
    Err(error) => {
      let reachable = http_probe(&url, false).await;
      let marker = read_marker(root)?;
      if reachable {
        reqwest::get(format!("{url}/shutdown")).await?.text().await?;
      }
      return Ok(json!({ "error": error.to_string(), "elapsedMs": elapsed_ms,
        "reachableAfterFailure": reachable, "marker": marker }));
    },
  };
  let first_url = manager.first_url();
  let response = reqwest::get(&url).await;
  manager.stop().await;
  let response = response?;
  let marker = read_marker(root)?;
  Ok(json!({
    "url": url,
    "firstUrl": first_url,
    "status": response.status().as_u16(),
    "body": response.text().await?,
    "marker": marker,
    "reachableAfterStop": http_probe(&url, false).await,
  }))
}

fn read_marker(root: &Path) -> Result<Option<String>> {
  match std::fs::read_to_string(root.join("shutdown-marker")) {
    Ok(marker) => Ok(Some(marker)),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error.into()),
  }
}

async fn reuse() -> Result<Value> {
  let server = FixtureServer::start(FixtureServerOptions::default()).await?;
  let url = format!("{}/secure", server.tls_url());
  let result = WebServerManager::start(&[WebServerConfig {
    command: Some("unreachable-command".into()),
    url: Some(url.clone()),
    reuse_existing_server: true,
    ignore_https_errors: true,
    ..Default::default()
  }])
  .await;
  let outcome = match result {
    Ok(manager) => {
      let first = manager.first_url();
      manager.stop().await;
      Ok(json!({ "url": url, "firstUrl": first, "reachableAfterStop": http_probe(&url, true).await }))
    },
    Err(error) => Err(error.into()),
  };
  server.stop().await;
  outcome
}

pub async fn probes() -> Result<Value> {
  let server = FixtureServer::start(FixtureServerOptions::default()).await?;
  let http = format!("{}/fx/landed", server.url());
  let https = format!("{}/secure", server.tls_url());
  let result = json!({
    "httpStrict": http_probe(&http, false).await,
    "httpLenient": http_probe(&http, true).await,
    "httpsStrict": http_probe(&https, false).await,
    "httpsLenient": http_probe(&https, true).await,
  });
  server.stop().await;
  Ok(result)
}

pub async fn child() -> Result<()> {
  use std::io::Write;
  use tokio::signal::unix::{SignalKind, signal};

  if let Ok(code) = std::env::var("PROBE_EXIT") {
    std::process::exit(code.parse()?);
  }
  let mut term = signal(SignalKind::terminate())?;
  let mut interrupt = signal(SignalKind::interrupt())?;
  if std::env::var("PROBE_NOISY").as_deref() == Ok("true") {
    let bytes = vec![b'x'; 1024 * 1024];
    std::io::stdout().write_all(&bytes)?;
    std::io::stderr().write_all(&bytes)?;
  }
  let port: u16 = std::env::var("PROBE_PORT")?.parse()?;
  let marker = std::env::var("PROBE_MARKER").context("missing fixture marker path")?;
  let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
  let (cleanup, mut cleanup_rx) = tokio::sync::mpsc::channel(1);
  let (release, released) = tokio::sync::watch::channel(false);
  let app = axum::Router::new()
    .route("/", axum::routing::get(|| async { "ok" }))
    .route(
      "/unavailable",
      axum::routing::get(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
    )
    .route(
      "/stall",
      axum::routing::get(move || {
        let mut released = released.clone();
        async move {
          released.wait_for(|value| *value).await.ok();
          axum::http::StatusCode::SERVICE_UNAVAILABLE
        }
      }),
    )
    .route(
      "/shutdown",
      axum::routing::get(move || {
        let cleanup = cleanup.clone();
        async move {
          cleanup.send(()).await.ok();
          "stopping"
        }
      }),
    );
  let (stop, stopped) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    axum::serve(listener, app)
      .with_graceful_shutdown(async {
        stopped.await.ok();
      })
      .await
  });
  let received = tokio::select! {
    _ = term.recv() => "SIGTERM",
    _ = interrupt.recv() => "SIGINT",
    _ = cleanup_rx.recv() => "HTTP",
  };
  std::fs::write(marker, received)?;
  release.send_replace(true);
  stop.send(()).ok();
  server.await??;
  Ok(())
}
