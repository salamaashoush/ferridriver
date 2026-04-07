//! `BrowserContext` class -- NAPI binding for `ferridriver::ContextRef`.

use crate::page::Page;
use crate::types::CookieData;
use napi::Result;
use napi_derive::napi;
use std::collections::HashMap;

/// Isolated browser context with its own cookies, storage, and permissions.
/// Mirrors Playwright's `BrowserContext`.
#[napi]
pub struct BrowserContext {
  inner: ferridriver::ContextRef,
}

impl BrowserContext {
  pub(crate) fn wrap(inner: ferridriver::ContextRef) -> Self {
    Self { inner }
  }
}

#[napi]
impl BrowserContext {
  /// Context name.
  #[napi(getter)]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Create a new page in this context.
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.new_page())
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Get all pages in this context.
  #[napi]
  pub async fn pages(&self) -> Result<Vec<Page>> {
    let pages = self.inner.pages().await.map_err(napi::Error::from_reason)?;
    Ok(pages.into_iter().map(Page::wrap).collect())
  }

  // ── Cookies ──

  #[napi]
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    let cookies = self.inner.cookies().await.map_err(napi::Error::from_reason)?;
    Ok(cookies.iter().map(CookieData::from).collect())
  }

  #[napi]
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let native: Vec<ferridriver::backend::CookieData> =
      cookies.iter().map(ferridriver::backend::CookieData::from).collect();
    self.inner.add_cookies(native).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn clear_cookies(&self) -> Result<()> {
    self.inner.clear_cookies().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn delete_cookie(&self, name: String, domain: Option<String>) -> Result<()> {
    let state = self.inner.state().read().await;
    let ctx = state.context(self.inner.name()).map_err(napi::Error::from_reason)?;
    ctx
      .delete_cookie(&name, domain.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Timeouts ──

  #[napi]
  pub fn set_default_timeout(&mut self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  #[napi]
  pub fn set_default_navigation_timeout(&mut self, ms: f64) {
    self.inner.set_default_navigation_timeout(crate::types::f64_to_u64(ms));
  }

  // ── Permissions ──

  #[napi]
  pub async fn grant_permissions(&self, permissions: Vec<String>, origin: Option<String>) -> Result<()> {
    self
      .inner
      .grant_permissions(&permissions, origin.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn clear_permissions(&self) -> Result<()> {
    self.inner.clear_permissions().await.map_err(napi::Error::from_reason)
  }

  // ── Context-level emulation ──

  #[napi]
  pub async fn set_geolocation(&self, latitude: f64, longitude: f64, accuracy: Option<f64>) -> Result<()> {
    self
      .inner
      .set_geolocation(latitude, longitude, accuracy.unwrap_or(1.0))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_extra_http_headers(&self, headers: HashMap<String, String>) -> Result<()> {
    let mut fx = rustc_hash::FxHashMap::default();
    for (k, v) in headers {
      fx.insert(k, v);
    }
    self
      .inner
      .set_extra_http_headers(&fx)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_offline(&self, offline: bool) -> Result<()> {
    self.inner.set_offline(offline).await.map_err(napi::Error::from_reason)
  }

  // ── Context-level init scripts ──

  #[napi]
  pub async fn add_init_script(&self, source: String) -> Result<Vec<String>> {
    self
      .inner
      .add_init_script(&source)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Lifecycle ──

  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close().await.map_err(napi::Error::from_reason)
  }
}
