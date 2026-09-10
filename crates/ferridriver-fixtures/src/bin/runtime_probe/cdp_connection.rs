use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use ferridriver::backend::cdp::{CdpBrowser, ws::WsTransport};
use ferridriver::backend::process::ChildGroup;
use serde_json::{Value, json};

async fn pages(browser: &CdpBrowser<WsTransport>) -> Result<Vec<Value>> {
  let mut observations = Vec::new();
  for page in Box::pin(browser.pages()).await? {
    let url = tokio::time::timeout(Duration::from_secs(3), page.url()).await??;
    let title = tokio::time::timeout(Duration::from_secs(3), page.title()).await??;
    observations.push(json!({ "url": url, "title": title }));
  }
  Ok(observations)
}

pub async fn run(root: &Path, urls: Vec<String>) -> Result<Value> {
  let profile = root.join("cdp-connection-profile");
  tokio::fs::create_dir(&profile).await?;
  let (transport, child) = WsTransport::spawn(
    &ferridriver::state::detect_chromium(),
    &profile,
    &ferridriver::state::chrome_flags(true, &[]),
    false,
    &rustc_hash::FxHashMap::default(),
  )
  .await?;
  let mut child = ChildGroup::recorded(child, Some(&profile), false);
  drop(transport);
  let result = async {
    let port_file = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await?;
    let mut lines = port_file.lines();
    let port = lines.next().context("DevTools port missing")?;
    let path = lines.next().context("DevTools browser path missing")?;
    let endpoint = format!("ws://127.0.0.1:{port}{path}");
    let browser = CdpBrowser::<WsTransport>::connect(&endpoint).await?;
    for url in urls {
      Box::pin(browser.new_page(&url, None, None)).await?;
    }
    let before = pages(&browser).await?;
    drop(browser);
    let running_after_disconnect = child.is_running();
    let browser = CdpBrowser::<WsTransport>::connect(&endpoint).await?;
    let after = pages(&browser).await?;
    drop(browser);
    Ok(json!({ "before": before, "after": after, "runningAfterDisconnect": running_after_disconnect }))
  }
  .await;
  child.shutdown().await;
  result
}
