//! `BrowserContext` -- isolated browser environment with pages, cookies, and logs.
//!
//! Mirrors Playwright's `BrowserContext` exactly:
//! - Owns pages (Vec<AnyPage>)
//! - Owns cookies (via any page in the context)
//! - Owns console/network/dialog logs
//! - Created by `Browser.new_context()`
//! - Pages are created by `context.new_page()`

use crate::backend::{AnyPage, CookieData};
use crate::page::Page;
use crate::state::SessionKey;
use rustc_hash::FxHashMap as HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A collected console message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConsoleMsg {
  pub level: String,
  pub text: String,
}

/// A collected network request with headers and optional post data.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetRequest {
  pub id: String,
  pub method: String,
  pub url: String,
  pub resource_type: String,
  pub status: Option<i64>,
  pub mime_type: Option<String>,
  /// Request headers (key -> value).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub headers: Option<HashMap<String, String>>,
  /// POST body data (if applicable).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub post_data: Option<String>,
}

/// A dismissed dialog event (alert, confirm, prompt).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DialogEvent {
  pub dialog_type: String,
  pub message: String,
  pub action: String,
}

/// Isolated browser context. Directly holds pages, cookies, and event logs.
/// This IS the state -- not a wrapper around some other struct.
/// Stored in `BrowserState`'s context map.
pub struct BrowserContext {
  /// Pages in this context.
  pub pages: Vec<AnyPage>,
  /// Active page index.
  pub active_page_idx: usize,
  /// Element ref map for accessibility snapshots.
  pub ref_map: HashMap<String, i64>,
  /// Console messages collected from page events.
  pub console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
  /// Network requests collected from page events.
  pub network_log: Arc<RwLock<Vec<NetRequest>>>,
  /// Dialog events.
  pub dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
  /// Context name (unique identifier).
  name: String,
}

impl BrowserContext {
  /// Create a new empty context.
  pub(crate) fn new(name: String) -> Self {
    Self {
      pages: Vec::new(),
      active_page_idx: 0,
      ref_map: HashMap::default(),
      console_log: Arc::new(RwLock::new(Vec::new())),
      network_log: Arc::new(RwLock::new(Vec::new())),
      dialog_log: Arc::new(RwLock::new(Vec::new())),
      name,
    }
  }

  /// Context name.
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Get the active page in this context.
  #[must_use]
  pub fn active_page(&self) -> Option<&AnyPage> {
    self.pages.get(self.active_page_idx)
  }

  // -- Cookies (operate on active page) ------------------------------------

  /// Get all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if cookies cannot be retrieved from the active page.
  pub async fn cookies(&self) -> Result<Vec<CookieData>, String> {
    if let Some(page) = self.active_page() {
      page.get_cookies().await
    } else {
      Ok(Vec::new())
    }
  }

  /// Add cookies to this context.
  ///
  /// # Errors
  ///
  /// Returns an error if no page exists or if setting a cookie fails.
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<(), String> {
    let page = self.active_page().ok_or("No page in context")?;
    for cookie in cookies {
      page.set_cookie(cookie).await?;
    }
    Ok(())
  }

  /// Clear all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if clearing cookies fails on the active page.
  pub async fn clear_cookies(&self) -> Result<(), String> {
    if let Some(page) = self.active_page() {
      page.clear_cookies().await?;
    }
    Ok(())
  }

  /// Delete specific cookies by name and optional domain.
  ///
  /// # Errors
  ///
  /// Returns an error if reading or re-setting cookies fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    let cookies = self.cookies().await?;
    if let Some(page) = self.active_page() {
      page.clear_cookies().await?;
      for cookie in cookies {
        let name_matches = cookie.name == name;
        let domain_matches = domain.is_none_or(|d| cookie.domain == d);
        if !(name_matches && domain_matches) {
          page.set_cookie(cookie).await?;
        }
      }
    }
    Ok(())
  }

  // -- Console/network/dialog log access -----------------------------------

  /// Get console messages, optionally filtered by level.
  pub async fn console_messages(&self, level: Option<&str>, limit: usize) -> Vec<ConsoleMsg> {
    let msgs = self.console_log.read().await;
    msgs
      .iter()
      .filter(|m| level.is_none_or(|l| l == "all" || m.level == l))
      .rev()
      .take(limit)
      .cloned()
      .collect::<Vec<_>>()
      .into_iter()
      .rev()
      .collect()
  }

  /// Get network requests.
  pub async fn network_requests(&self, limit: usize) -> Vec<NetRequest> {
    let reqs = self.network_log.read().await;
    reqs
      .iter()
      .rev()
      .take(limit)
      .cloned()
      .collect::<Vec<_>>()
      .into_iter()
      .rev()
      .collect()
  }

  /// Get dialog events.
  pub async fn dialog_messages(&self, limit: usize) -> Vec<DialogEvent> {
    let msgs = self.dialog_log.read().await;
    let start = msgs.len().saturating_sub(limit);
    msgs[start..].to_vec()
  }
}

// -- ContextRef: handle for the high-level Browser API -----------------------

use crate::state::BrowserState;
use tokio::sync::Mutex;

/// Handle to a browser context. Created by `Browser::new_context()` / `default_context()`.
/// Provides the Playwright-compatible context API by delegating to `BrowserState`.
#[derive(Clone)]
pub struct ContextRef {
  pub(crate) state: Arc<Mutex<BrowserState>>,
  pub(crate) name: String,
  /// Pre-parsed session key (avoids re-parsing on every operation).
  pub(crate) key: SessionKey,
  /// Default timeout for actions in this context (ms). 0 = no override.
  default_timeout_ms: u64,
  /// Default navigation timeout in this context (ms). 0 = no override.
  default_navigation_timeout_ms: u64,
}

impl ContextRef {
  pub(crate) fn new(state: Arc<Mutex<BrowserState>>, name: String) -> Self {
    let key = SessionKey::parse(&name);
    Self {
      state,
      name,
      key,
      default_timeout_ms: 0,
      default_navigation_timeout_ms: 0,
    }
  }

  /// Context name.
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Create a new page in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page(&self) -> Result<Page, String> {
    let mut state = self.state.lock().await;
    let any_page = state.open_page_keyed(&self.key, "about:blank").await?;
    Ok(Page::new(any_page))
  }

  /// Get all pages in this context as Page handles.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist.
  pub async fn pages(&self) -> Result<Vec<Page>, String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    Ok(ctx.pages.iter().map(|p| Page::new(p.clone())).collect())
  }

  /// Get all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or cookie retrieval fails.
  pub async fn cookies(&self) -> Result<Vec<CookieData>, String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    ctx.cookies().await
  }

  /// Add cookies to this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or setting cookies fails.
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    ctx.add_cookies(cookies).await
  }

  /// Clear all cookies in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or clearing cookies fails.
  pub async fn clear_cookies(&self) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    ctx.clear_cookies().await
  }

  /// Set the default timeout for actions in this context (ms).
  pub fn set_default_timeout(&mut self, ms: u64) {
    self.default_timeout_ms = ms;
  }

  /// Set the default navigation timeout for this context (ms).
  pub fn set_default_navigation_timeout(&mut self, ms: u64) {
    self.default_navigation_timeout_ms = ms;
  }

  /// Grant permissions in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context or page does not exist, or granting fails.
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    if let Some(page) = ctx.active_page() {
      page.grant_permissions(permissions, origin).await
    } else {
      Err("No page in context".into())
    }
  }

  /// Clear all granted permissions.
  ///
  /// # Errors
  ///
  /// Returns an error if resetting permissions fails.
  pub async fn clear_permissions(&self) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    if let Some(page) = ctx.active_page() {
      page.reset_permissions().await
    } else {
      Ok(())
    }
  }

  /// Close this context (remove from `BrowserState`).
  ///
  /// # Errors
  ///
  /// Returns an error if state lock acquisition fails.
  pub async fn close(&self) -> Result<(), String> {
    let mut state = self.state.lock().await;
    state.remove_context(&self.name);
    Ok(())
  }

  /// Access the internal state (for MCP server integration).
  #[must_use]
  pub fn state(&self) -> &Arc<Mutex<BrowserState>> {
    &self.state
  }

  // ── Context-level APIs (apply to all pages) ────────────────────────────

  /// Add an init script to all pages in this context (current + future).
  /// Returns identifiers for each page.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or script injection fails.
  pub async fn add_init_script(&self, source: &str) -> Result<Vec<String>, String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    let mut ids = Vec::new();
    for page in &ctx.pages {
      ids.push(page.add_init_script(source).await?);
    }
    Ok(ids)
  }

  /// Set geolocation for all pages in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or geolocation emulation fails.
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.set_geolocation(lat, lng, accuracy).await?;
    }
    Ok(())
  }

  /// Set extra HTTP headers for all pages in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or setting headers fails.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.set_extra_http_headers(headers).await?;
    }
    Ok(())
  }

  /// Set offline mode for all pages in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or network state change fails.
  pub async fn set_offline(&self, offline: bool) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.set_network_state(offline, 0.0, -1.0, -1.0).await?;
    }
    Ok(())
  }

  /// Register a route handler for all pages in this context.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route registration fails.
  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.route(pattern, handler.clone()).await?;
    }
    Ok(())
  }

  /// Remove route handlers matching pattern from all pages.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route removal fails.
  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    let state = self.state.lock().await;
    let ctx = state.context(&self.name)?;
    for page in &ctx.pages {
      page.unroute(pattern).await?;
    }
    Ok(())
  }
}
