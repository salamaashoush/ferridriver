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
}

pub async fn shutdown(root: &Path, request: Request) -> Result<Value> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  let url = format!("http://127.0.0.1:{port}");
  let executable = std::env::current_exe()?.display().to_string().replace('\'', "'\\''");
  let config = WebServerConfig {
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
  drop(listener);
  let manager = WebServerManager::start(&[config]).await?;
  let first_url = manager.first_url();
  let response = reqwest::get(&url).await;
  manager.stop().await;
  let response = response?;
  let marker = match std::fs::read_to_string(root.join("shutdown-marker")) {
    Ok(marker) => Some(marker),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
    Err(error) => return Err(error.into()),
  };
  Ok(json!({
    "url": url,
    "firstUrl": first_url,
    "status": response.status().as_u16(),
    "body": response.text().await?,
    "marker": marker,
    "reachableAfterStop": http_probe(&url, false).await,
  }))
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
  let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
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
  };
  std::fs::write(marker, received)?;
  stop.send(()).ok();
  server.await??;
  Ok(())
}
