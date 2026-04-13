//! BiDi browser -- manages contexts, pages, and browser lifecycle.

use serde_json::json;
use std::sync::Arc;
use tracing::debug;

use super::page::BidiPage;
use super::session::BidiSession;
use crate::backend::{AnyPage, NavLifecycle};

#[derive(Clone)]
/// Browser instance using the WebDriver BiDi protocol.
pub struct BidiBrowser {
  pub(crate) session: Arc<BidiSession>,
  child: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>,
}

impl BidiBrowser {
  /// Launch a browser with BiDi support.
  /// Auto-detects Firefox vs Chrome from the binary path.
  pub async fn launch_with_flags(browser_path: &str, flags: &[String]) -> Result<Self, String> {
    // Determine if headless from flags
    let headless = flags.iter().any(|f| f == "--headless");
    let (session, child) = BidiSession::launch(browser_path, flags, headless).await?;
    Ok(Self {
      session: Arc::new(session),
      child: Arc::new(tokio::sync::Mutex::new(Some(child))),
    })
  }

  /// Connect to an existing BiDi endpoint via WebSocket.
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    let session = BidiSession::connect(ws_url).await?;
    Ok(Self {
      session: Arc::new(session),
      child: Arc::new(tokio::sync::Mutex::new(None)),
    })
  }

  /// List all open pages (top-level browsing contexts).
  pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
    let result = self
      .session
      .transport
      .send_command("browsingContext.getTree", json!({}))
      .await?;
    let contexts = result
      .get("contexts")
      .and_then(|v| v.as_array())
      .ok_or("browsingContext.getTree: missing contexts array")?;

    let mut pages = Vec::with_capacity(contexts.len());
    for ctx in contexts {
      let context_id = ctx
        .get("context")
        .and_then(|v| v.as_str())
        .ok_or("browsingContext.getTree: context missing 'context' field")?;
      pages.push(AnyPage::Bidi(
        BidiPage::create(self.session.clone(), context_id.to_string()).await?,
      ));
    }
    Ok(pages)
  }

  /// Create a new page (tab) and optionally navigate.
  pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
    let result = self
      .session
      .transport
      .send_command("browsingContext.create", json!({"type": "tab"}))
      .await?;
    let context_id = result
      .get("context")
      .and_then(|v| v.as_str())
      .ok_or("browsingContext.create: missing context id")?
      .to_string();

    debug!("BiDi new page: context={context_id}");
    let page = BidiPage::create(self.session.clone(), context_id).await?;

    if !url.is_empty() && url != "about:blank" {
      page.goto(url, NavLifecycle::Load, 30_000).await?;
    }

    Ok(AnyPage::Bidi(page))
  }

  /// Close the browser.
  pub async fn close(&mut self) -> Result<(), String> {
    // Try graceful BiDi close first
    let _ = self.session.transport.send_command("browser.close", json!({})).await;

    // Then kill the child process if we own it
    if let Some(mut child) = self.child.lock().await.take() {
      let _ = child.kill().await;
    }

    Ok(())
  }
}
