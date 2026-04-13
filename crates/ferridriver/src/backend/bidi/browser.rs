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
  async fn wait_for_context_event(
    &self,
    method: &str,
    context_id: &str,
    timeout: std::time::Duration,
  ) -> Result<(), String> {
    let mut rx = self.session.transport.subscribe_events();
    let wait_for_event = async {
      while let Ok(event) = rx.recv().await {
        if event.method != method {
          continue;
        }
        if event.params.get("context").and_then(|v| v.as_str()) == Some(context_id) {
          return Ok(());
        }
      }
      Err(format!("BiDi event stream closed while waiting for {method}"))
    };
    tokio::time::timeout(timeout, wait_for_event)
      .await
      .map_err(|_| format!("Timed out waiting for {method} on {context_id}"))?
  }

  async fn list_context_ids_for_user_context(&self, user_context_id: &str) -> Result<Vec<String>, String> {
    let result = self
      .session
      .transport
      .send_command("browsingContext.getTree", json!({}))
      .await?;
    let contexts = result
      .get("contexts")
      .and_then(|v| v.as_array())
      .ok_or("browsingContext.getTree: missing contexts array")?;

    Ok(
      contexts
        .iter()
        .filter(|ctx| ctx.get("userContext").and_then(|v| v.as_str()) == Some(user_context_id))
        .filter_map(|ctx| ctx.get("context").and_then(|v| v.as_str()).map(ToOwned::to_owned))
        .collect(),
    )
  }

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

  /// Create a new isolated user context.
  pub async fn new_context(&self) -> Result<String, String> {
    let result = self
      .session
      .transport
      .send_command("browser.createUserContext", json!({}))
      .await?;
    result
      .get("userContext")
      .and_then(|v| v.as_str())
      .map(ToOwned::to_owned)
      .ok_or("browser.createUserContext: missing userContext id".into())
  }

  /// Dispose an isolated user context.
  pub async fn dispose_context(&self, user_context_id: &str) -> Result<(), String> {
    let context_ids = self.list_context_ids_for_user_context(user_context_id).await.unwrap_or_default();
    let mut waiters = Vec::with_capacity(context_ids.len());
    for context_id in &context_ids {
      waiters.push(self.wait_for_context_event(
        "browsingContext.contextDestroyed",
        context_id,
        std::time::Duration::from_secs(2),
      ));
    }
    self
      .session
      .transport
      .send_command(
        "browser.removeUserContext",
        json!({"userContext": user_context_id}),
      )
      .await?;
    for waiter in waiters {
      let _ = waiter.await;
    }
    Ok(())
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
        BidiPage::create(self.session.clone(), context_id.to_string())?,
      ));
    }
    Ok(pages)
  }

  /// Create a new page (tab) and optionally navigate.
  pub async fn new_page(
    &self,
    url: &str,
    user_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage, String> {
    let mut params = json!({"type": "window"});
    if let Some(user_context_id) = user_context_id {
      params["userContext"] = json!(user_context_id);
    }
    let mut rx = self.session.transport.subscribe_events();
    let result = self
      .session
      .transport
      .send_command("browsingContext.create", params)
      .await?;
    let context_id = result
      .get("context")
      .and_then(|v| v.as_str())
      .ok_or("browsingContext.create: missing context id")?
      .to_string();

    let wait_for_created = async {
      while let Ok(event) = rx.recv().await {
        if event.method != "browsingContext.contextCreated" {
          continue;
        }
        if event.params.get("context").and_then(|v| v.as_str()) == Some(&context_id) {
          return Ok::<(), String>(());
        }
      }
      Err("BiDi event stream closed while waiting for browsingContext.contextCreated".to_string())
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), wait_for_created).await;

    debug!("BiDi new page: context={context_id}");
    let page = BidiPage::create(self.session.clone(), context_id)?;
    page.wait_until_ready().await?;

    if let Some(viewport) = viewport {
      page.emulate_viewport(viewport).await?;
    }

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
