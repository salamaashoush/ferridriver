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

use crate::backend::BackendKind;
use crate::context::ContextRef;
use crate::options::LaunchOptions;
use crate::page::Page;
use crate::state::{BrowserState, ConnectMode};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Browser instance. Manages contexts, pages, and browser lifecycle.
///
/// `Clone` is cheap — all clones share the same underlying browser process
/// and state via `Arc`. This enables exposing `browser` as a test fixture.
#[derive(Clone)]
pub struct Browser {
  state: Arc<RwLock<BrowserState>>,
  backend_kind: BackendKind,
}

impl Browser {
  /// Launch a browser with the given options.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start or connection fails.
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

    let backend_kind = options.backend;
    let mut state = BrowserState::with_options(mode, options);
    Box::pin(state.ensure_browser()).await?;
    Ok(Self {
      state: Arc::new(RwLock::new(state)),
      backend_kind,
    })
  }

  /// Connect to a running browser via WebSocket URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection fails.
  pub async fn connect(url: &str) -> Result<Self, String> {
    Box::pin(Self::launch(LaunchOptions {
      ws_endpoint: Some(url.to_string()),
      ..Default::default()
    }))
    .await
  }

  /// Wrap an existing shared state as a Browser handle.
  /// Used by MCP server and other contexts that already manage browser state.
  pub fn from_shared_state(state: Arc<RwLock<BrowserState>>, backend_kind: BackendKind) -> Self {
    Self { state, backend_kind }
  }

  /// Create a new isolated browser context.
  /// Mirrors Playwright's `browser.newContext()`.
  pub fn new_context(&self) -> ContextRef {
    static CTX_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTX_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("context-{id}");
    ContextRef::new(self.state.clone(), name)
  }

  /// Get the default browser context.
  #[must_use]
  pub fn default_context(&self) -> ContextRef {
    ContextRef::new(self.state.clone(), "default".to_string())
  }

  /// Shorthand: create a new page in the default context.
  /// Equivalent to `browser.default_context().new_page()`.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page(&self) -> Result<Arc<Page>, String> {
    Box::pin(self.default_context().new_page()).await
  }

  /// Shorthand: create a new page and navigate to URL.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or navigation fails.
  pub async fn new_page_with_url(&self, url: &str) -> Result<Arc<Page>, String> {
    let page = Box::pin(self.new_page()).await?;
    page.goto(url, None).await?;
    Ok(page)
  }

  /// Shorthand: get the active page in the default context.
  /// Creates a page if none exists.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or retrieval fails.
  ///
  pub async fn page(&self) -> Result<Arc<Page>, String> {
    let ctx = self.default_context();
    let mut pages = ctx.pages().await.unwrap_or_default();
    if pages.is_empty() {
      Box::pin(ctx.new_page()).await
    } else {
      Ok(pages.swap_remove(0))
    }
  }

  /// Close the browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser cannot be closed cleanly.
  pub async fn close(&self) -> Result<(), String> {
    let mut state = self.state.write().await;
    state.shutdown().await;
    Ok(())
  }

  /// Access the internal state (for MCP server integration).
  #[must_use]
  pub fn state(&self) -> &Arc<RwLock<BrowserState>> {
    &self.state
  }

  /// List all browser contexts.
  pub async fn contexts(&self) -> Vec<ContextRef> {
    let state = self.state.read().await;
    state
      .list_contexts()
      .await
      .iter()
      .map(|c| ContextRef::new(self.state.clone(), c.name.clone()))
      .collect()
  }

  /// Get the browser engine name.
  #[must_use]
  pub fn version(&self) -> &'static str {
    match self.backend_kind {
      BackendKind::CdpPipe | BackendKind::CdpRaw => "Chromium",
      #[cfg(target_os = "macos")]
      BackendKind::WebKit => "WebKit",

      BackendKind::Bidi => "BiDi",
    }
  }

  /// Check if the browser is connected and alive.
  pub async fn is_connected(&self) -> bool {
    let state = self.state.read().await;
    state.is_connected()
  }
}
