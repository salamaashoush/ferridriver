//! `BrowserContext` class -- NAPI binding for `ferridriver::ContextRef`.

use crate::error::IntoNapi;
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
    let page = Box::pin(self.inner.new_page()).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Get all pages in this context.
  #[napi]
  pub async fn pages(&self) -> Result<Vec<Page>> {
    let pages = self.inner.pages().await.into_napi()?;
    Ok(pages.into_iter().map(Page::wrap).collect())
  }

  // ── Cookies ──

  #[napi]
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    let cookies = self.inner.cookies().await.into_napi()?;
    Ok(cookies.iter().map(CookieData::from).collect())
  }

  #[napi]
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let native: Vec<ferridriver::backend::CookieData> =
      cookies.iter().map(ferridriver::backend::CookieData::from).collect();
    self.inner.add_cookies(native).await.into_napi()
  }

  #[napi]
  pub async fn clear_cookies(&self) -> Result<()> {
    self.inner.clear_cookies().await.into_napi()
  }

  #[napi]
  pub async fn delete_cookie(&self, name: String, domain: Option<String>) -> Result<()> {
    let state = self.inner.state().read().await;
    let ctx = state.context(self.inner.name()).map_err(napi::Error::from_reason)?;
    ctx.delete_cookie(&name, domain.as_deref()).await.into_napi()
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
      .into_napi()
  }

  #[napi]
  pub async fn clear_permissions(&self) -> Result<()> {
    self.inner.clear_permissions().await.into_napi()
  }

  // ── Context-level emulation ──

  #[napi]
  pub async fn set_geolocation(&self, latitude: f64, longitude: f64, accuracy: Option<f64>) -> Result<()> {
    self
      .inner
      .set_geolocation(latitude, longitude, accuracy.unwrap_or(1.0))
      .await
      .into_napi()
  }

  #[napi]
  pub async fn set_extra_http_headers(&self, headers: HashMap<String, String>) -> Result<()> {
    let mut fx = rustc_hash::FxHashMap::default();
    for (k, v) in headers {
      fx.insert(k, v);
    }
    self.inner.set_extra_http_headers(&fx).await.into_napi()
  }

  #[napi]
  pub async fn set_offline(&self, offline: bool) -> Result<()> {
    self.inner.set_offline(offline).await.into_napi()
  }

  // ── Context-level init scripts ──

  /// Register a JS snippet to run on every new document on every page in
  /// this context. Mirrors Playwright's
  /// `browserContext.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// See [`crate::page::Page::add_init_script`] for argument semantics.
  #[napi(ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any")]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
  ) -> Result<Vec<String>> {
    self.inner.add_init_script(script.into(), arg.0).await.into_napi()
  }

  // ── Context-level events ──

  /// Register a context-level event listener. Currently supports
  /// `'weberror'` — unhandled errors / rejections from any page in
  /// this context. Playwright:
  /// `browserContext.on('weberror', (webError: WebError) => …)`.
  /// Returns a numeric listener id for removal via [`Self::off`].
  #[napi(
    ts_args_type = "event: 'weberror', listener: (data: { name: string; message: string; stack: string }) => void"
  )]
  pub fn on(&self, event: String, listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>) -> Result<f64> {
    let callback = build_context_event_callback(listener, event.clone())?;
    let id = self.inner.on(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// One-shot variant of [`Self::on`]. Auto-removed after first match.
  #[napi(
    ts_args_type = "event: 'weberror', listener: (data: { name: string; message: string; stack: string }) => void"
  )]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>,
  ) -> Result<f64> {
    let callback = build_context_event_callback(listener, event.clone())?;
    let id = self.inner.once(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Remove a context-level listener by id.
  #[napi]
  pub fn off(&self, listener_id: f64) {
    self
      .inner
      .off(ferridriver::events::ListenerId(crate::types::f64_to_u64(listener_id)));
  }

  /// Wait for a context-level event. Playwright:
  /// `browserContext.waitForEvent(event, options?)`. Currently
  /// supports `'weberror'` — returns the live [`crate::web_error::WebError`]
  /// handle.
  #[napi(
    ts_args_type = "event: 'weberror', timeoutMs?: number",
    ts_return_type = "Promise<WebError>"
  )]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<crate::web_error::WebError> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let ev = self.inner.wait_for_event(&event, timeout).await.into_napi()?;
    match ev {
      ferridriver::events::ContextEvent::WebError(err) => Ok(crate::web_error::WebError::from_core(err)),
    }
  }

  // ── Lifecycle ──

  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close().await.into_napi()
  }
}

/// Lower a JS listener `Function<'_>` (which is `!Send` because it
/// holds a raw NAPI value pointer) into a pure-Send `ContextEventCallback`.
/// Kept in a separate sync function so async `BrowserContext::on` /
/// `once` don't capture the `!Send` `Function` across their await
/// points (raw-pointer borrows leak into the async generator otherwise).
fn build_context_event_callback(
  listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>,
  event_name: String,
) -> Result<ferridriver::events::ContextEventCallback> {
  let tsfn = listener
    .build_threadsafe_function()
    .callee_handled::<false>()
    .weak::<true>()
    .max_queue_size::<0>()
    .build()?;
  Ok(std::sync::Arc::new(move |ev| {
    if let Some(data) = context_event_to_js(&event_name, &ev) {
      tsfn.call(data, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
    }
  }))
}

fn context_event_to_js(name: &str, ev: &ferridriver::events::ContextEvent) -> Option<serde_json::Value> {
  match (name, ev) {
    ("weberror", ferridriver::events::ContextEvent::WebError(err)) => {
      let d = err.error();
      Some(serde_json::json!({
        "name": d.name,
        "message": d.message,
        "stack": d.stack,
      }))
    },
    _ => None,
  }
}
