use std::path::Path;

use anyhow::{Context, Result};
use ferridriver_fixtures::{FixtureServer, FixtureServerOptions};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
  Tls,
  Websocket,
  Proxy,
  Static,
}

async fn observe(server: &FixtureServer, scenario: Scenario) -> Result<Value> {
  let client = reqwest::Client::new();
  match scenario {
    Scenario::Tls => {
      let url = format!("{}/secure", server.tls_url());
      let rejected = client.get(&url).send().await.is_err();
      let lax = reqwest::Client::builder().danger_accept_invalid_certs(true).build()?;
      let response = lax.get(&url).send().await?;
      Ok(json!({ "rejected": rejected, "status": response.status().as_u16(), "body": response.text().await? }))
    },
    Scenario::Websocket => {
      let url = format!("{}/fx/ws", server.url().replacen("http", "ws", 1));
      let (mut socket, _) = tokio_tungstenite::connect_async(&url).await?;
      socket
        .send(tokio_tungstenite::tungstenite::Message::Text("ping-1".into()))
        .await?;
      let text = socket.next().await.context("text echo missing")??.into_text()?;
      socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(vec![1, 2, 3].into()))
        .await?;
      let binary = socket.next().await.context("binary echo missing")??.into_data();
      socket.close(None).await?;
      Ok(json!({ "text": text.as_str(), "binary": binary.to_vec() }))
    },
    Scenario::Proxy => proxy(server, &client).await,
    Scenario::Static => Ok(json!({
      "static": client.get(format!("{}/hello.html", server.url())).send().await?.text().await?,
      "landed": client.get(format!("{}/fx/landed", server.url())).send().await?.text().await?,
    })),
  }
}

async fn proxy(server: &FixtureServer, client: &reqwest::Client) -> Result<Value> {
  let info: Value = client
    .get(format!("{}/fx/proxy-info", server.url()))
    .send()
    .await?
    .json()
    .await?;
  let url = info["url"].as_str().context("proxy URL missing")?;
  let proxied = reqwest::Client::builder().proxy(reqwest::Proxy::http(url)?).build()?;
  let body = proxied
    .get("http://ferridriver-fixtures.invalid/behind-proxy")
    .send()
    .await?
    .text()
    .await?;
  let log: Value = client
    .get(format!("{}/fx/proxy-log", server.url()))
    .send()
    .await?
    .json()
    .await?;
  let cleared: Value = client
    .delete(format!("{}/fx/proxy-log", server.url()))
    .send()
    .await?
    .json()
    .await?;
  Ok(json!({ "advertised": url, "actual": server.proxy_url(), "body": body, "log": log, "cleared": cleared }))
}

pub async fn run(root: &Path, scenario: Scenario) -> Result<Value> {
  let server = FixtureServer::start(FixtureServerOptions {
    static_dir: matches!(scenario, Scenario::Static).then(|| root.to_path_buf()),
    ..Default::default()
  })
  .await?;
  let result = Box::pin(observe(&server, scenario)).await;
  server.stop().await;
  result
}
