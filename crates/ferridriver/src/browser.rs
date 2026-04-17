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

use crate::context::ContextRef;
use crate::error::Result;
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
  /// Product version captured once at launch from CDP
  /// `Browser.getVersion().product`. Cached here so `version()` stays
  /// synchronous and `Arc`-shared across cheap `Browser::clone`s.
  version: Arc<str>,
}

impl Browser {
  /// Launch a browser with the given options.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start or connection fails.
  pub async fn launch(options: LaunchOptions) -> Result<Self> {
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
    Box::pin(state.ensure_browser()).await?;
    let version: Arc<str> = state
      .default_browser()
      .map(crate::backend::AnyBrowser::version)
      .map_or_else(|| Arc::from("Unknown"), Arc::from);
    Ok(Self {
      state: Arc::new(RwLock::new(state)),
      version,
    })
  }

  /// Connect to a running browser via WebSocket URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection fails.
  pub async fn connect(url: &str) -> Result<Self> {
    Box::pin(Self::launch(LaunchOptions {
      ws_endpoint: Some(url.to_string()),
      ..Default::default()
    }))
    .await
  }

  /// Wrap an existing shared state as a Browser handle.
  /// Used by MCP server and other contexts that already manage browser state.
  ///
  /// The version string is read once from the state's default instance; if
  /// the instance has not been launched yet, `version()` returns
  /// `"Unknown"` until a subsequent `ensure_browser` fills it in.
  pub fn from_shared_state(state: Arc<RwLock<BrowserState>>) -> Self {
    let version: Arc<str> = state
      .try_read()
      .ok()
      .and_then(|s| s.default_browser().map(crate::backend::AnyBrowser::version))
      .map_or_else(|| Arc::from("Unknown"), Arc::from);
    Self { state, version }
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
  pub async fn new_page(&self) -> Result<Arc<Page>> {
    Box::pin(self.default_context().new_page()).await
  }

  /// Shorthand: create a new page and navigate to URL.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or navigation fails.
  pub async fn new_page_with_url(&self, url: &str) -> Result<Arc<Page>> {
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
  pub async fn page(&self) -> Result<Arc<Page>> {
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
  /// Close the browser. Accepts `Option<`[`crate::options::BrowserCloseOptions`]`>`
  /// — mirrors Playwright's `browser.close({ reason })`. The reason, if
  /// set, is surfaced on `TargetClosed` errors emitted to any in-flight
  /// operation on pages/contexts from this browser. Pass `None` for the
  /// common no-options case.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser cannot be closed cleanly.
  pub async fn close(&self, opts: Option<crate::options::BrowserCloseOptions>) -> Result<()> {
    let mut state = self.state.write().await;
    if let Some(reason) = opts.and_then(|o| o.reason) {
      state.set_close_reason(reason);
    }
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

  /// Real product version string for the running browser — mirrors
  /// Playwright's synchronous `browser.version()`.
  ///
  /// Captured once from CDP `Browser.getVersion().product` at handshake
  /// (e.g. `"HeadlessChrome/120.0.6099.109"` or `"Chrome/120.0.6099.109"`).
  /// For `WebKit` returns `"WebKit"` until we plumb `WKWebView`'s version
  /// through the IPC; for `BiDi` returns `"Firefox"`. Returns `"Unknown"`
  /// if the handshake did not complete before the `Browser` handle was
  /// constructed.
  #[must_use]
  pub fn version(&self) -> &str {
    &self.version
  }

  /// Check if the browser is connected and alive.
  pub async fn is_connected(&self) -> bool {
    let state = self.state.read().await;
    state.is_connected()
  }
}
