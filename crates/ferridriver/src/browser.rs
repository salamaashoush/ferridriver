//! Browser management -- mirrors Playwright's Browser interface.

use crate::backend::BackendKind;
use crate::page::Page;
use crate::state::{BrowserState, ConnectMode};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Browser instance. Manages pages and browser lifecycle.
pub struct Browser {
  state: Arc<Mutex<BrowserState>>,
}

impl Browser {
  /// Launch a new browser with default settings.
  pub async fn launch() -> Result<Self, String> {
    Self::launch_with(ConnectMode::Launch, BackendKind::CdpWs).await
  }

  /// Launch with specific backend and connection mode.
  pub async fn launch_with(mode: ConnectMode, backend: BackendKind) -> Result<Self, String> {
    let mut state = BrowserState::new(mode, backend);
    state.ensure_browser().await?;
    Ok(Self { state: Arc::new(Mutex::new(state)) })
  }

  /// Connect to a running browser via WebSocket URL.
  pub async fn connect(url: &str) -> Result<Self, String> {
    Self::launch_with(ConnectMode::ConnectUrl(url.to_string()), BackendKind::CdpWs).await
  }

  /// Create a new page (tab). Returns the newly created page.
  pub async fn new_page(&self) -> Result<Page, String> {
    let mut state = self.state.lock().await;
    let idx = state.open_page("default", "about:blank").await?;
    // Return the specific page we just created, not the "active" one
    let sess = state.session("default")?;
    let page = sess.pages.get(idx).ok_or("Page not found after creation")?.clone();
    Ok(Page::new(page))
  }

  /// Create a new page and navigate to URL.
  pub async fn new_page_with_url(&self, url: &str) -> Result<Page, String> {
    let page = self.new_page().await?;
    page.goto(url).await?;
    Ok(page)
  }

  /// Get the active page for the default session.
  pub async fn page(&self) -> Result<Page, String> {
    let mut state = self.state.lock().await;
    state.ensure_browser().await?;
    let page = state.active_page("default")?.clone();
    Ok(Page::new(page))
  }

  /// Close the browser.
  pub async fn close(&self) -> Result<(), String> {
    let mut state = self.state.lock().await;
    state.shutdown().await;
    Ok(())
  }

  /// Access the internal state (for MCP server integration).
  pub fn state(&self) -> &Arc<Mutex<BrowserState>> {
    &self.state
  }
}
