//! Browser management -- mirrors Playwright's Browser interface.
//!
//! ```ignore
//! use ferridriver::{Browser, options::LaunchOptions};
//!
//! // Simple launch (headless, auto-detect Chrome, cdp-pipe backend)
//! let browser = Browser::launch(LaunchOptions::default()).await?;
//!
//! // Headful with custom args
//! let browser = Browser::launch(LaunchOptions {
//!     headless: false,
//!     args: vec!["--window-size=1920,1080".into()],
//!     ..Default::default()
//! }).await?;
//!
//! // Connect to running browser
//! let browser = Browser::connect("ws://localhost:9222/...").await?;
//! ```

use crate::options::LaunchOptions;
use crate::page::Page;
use crate::state::{BrowserState, ConnectMode};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Browser instance. Manages pages and browser lifecycle.
pub struct Browser {
  state: Arc<Mutex<BrowserState>>,
}

impl Browser {
  /// Launch a browser with the given options.
  pub async fn launch(options: LaunchOptions) -> Result<Self, String> {
    let mode = if let Some(url) = &options.ws_endpoint {
      ConnectMode::ConnectUrl(url.clone())
    } else if let Some(ac) = &options.auto_connect {
      ConnectMode::AutoConnect {
        channel: ac.channel.clone(),
        user_data_dir: ac.user_data_dir.clone(),
      }
    } else {
      ConnectMode::Launch
    };

    let mut state = BrowserState::with_options(mode, options);
    state.ensure_browser().await?;
    Ok(Self { state: Arc::new(Mutex::new(state)) })
  }

  /// Connect to a running browser via WebSocket URL.
  pub async fn connect(url: &str) -> Result<Self, String> {
    Self::launch(LaunchOptions {
      ws_endpoint: Some(url.to_string()),
      ..Default::default()
    }).await
  }

  /// Create a new page (tab). Returns the newly created page.
  pub async fn new_page(&self) -> Result<Page, String> {
    let mut state = self.state.lock().await;
    let idx = state.open_page("default", "about:blank").await?;
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
