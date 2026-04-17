//! `BrowserContextJs`: JS wrapper around `ferridriver::context::ContextRef`.

use std::sync::Arc;

use ferridriver::context::ContextRef;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use rustc_hash::FxHashMap;

use crate::bindings::convert::{FerriResultExt, serde_from_js, serde_to_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "BrowserContext")]
pub struct BrowserContextJs {
  #[qjs(skip_trace)]
  inner: Arc<ContextRef>,
}

impl BrowserContextJs {
  #[must_use]
  pub fn new(inner: Arc<ContextRef>) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl BrowserContextJs {
  // ── Cookies ───────────────────────────────────────────────────────────────

  /// All cookies visible in this context.
  ///
  /// Returns an array of `{ name, value, domain, path, secure, httpOnly,
  /// expires, sameSite }` objects matching Playwright's cookie shape.
  #[qjs(rename = "cookies")]
  pub async fn cookies<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let cookies = self.inner.cookies().await.into_js()?;
    serde_to_js(&ctx, &cookies)
  }

  /// Append cookies to this context.
  ///
  /// `cookies` is an array matching Playwright's `SetNetworkCookieParam[]`.
  #[qjs(rename = "addCookies")]
  pub async fn add_cookies<'js>(&self, ctx: Ctx<'js>, cookies: Value<'js>) -> rquickjs::Result<()> {
    let parsed: Vec<ferridriver::backend::CookieData> = serde_from_js(&ctx, cookies)?;
    self.inner.add_cookies(parsed).await.into_js()
  }

  /// Clear all cookies.
  #[qjs(rename = "clearCookies")]
  pub async fn clear_cookies(&self) -> rquickjs::Result<()> {
    self.inner.clear_cookies().await.into_js()
  }

  /// Delete a cookie by name (optionally scoped to a domain).
  #[qjs(rename = "deleteCookie")]
  pub async fn delete_cookie(&self, name: String, domain: Option<String>) -> rquickjs::Result<()> {
    self.inner.delete_cookie(&name, domain.as_deref()).await.into_js()
  }

  // ── Permissions ───────────────────────────────────────────────────────────

  /// Grant a set of permissions (e.g. `['geolocation', 'notifications']`),
  /// optionally scoped to `origin`.
  #[qjs(rename = "grantPermissions")]
  pub async fn grant_permissions(&self, permissions: Vec<String>, origin: Option<String>) -> rquickjs::Result<()> {
    self
      .inner
      .grant_permissions(&permissions, origin.as_deref())
      .await
      .into_js()
  }

  /// Revoke all previously granted permissions.
  #[qjs(rename = "clearPermissions")]
  pub async fn clear_permissions(&self) -> rquickjs::Result<()> {
    self.inner.clear_permissions().await.into_js()
  }

  // ── Emulation ─────────────────────────────────────────────────────────────

  /// Override the geolocation reported to pages in this context.
  #[qjs(rename = "setGeolocation")]
  pub async fn set_geolocation(&self, latitude: f64, longitude: f64, accuracy: f64) -> rquickjs::Result<()> {
    self
      .inner
      .set_geolocation(latitude, longitude, accuracy)
      .await
      .into_js()
  }

  /// Toggle offline mode for this context.
  #[qjs(rename = "setOffline")]
  pub async fn set_offline(&self, offline: bool) -> rquickjs::Result<()> {
    self.inner.set_offline(offline).await.into_js()
  }

  /// Set HTTP headers sent with every request in this context.
  ///
  /// `headers` is a plain object (e.g. `{ 'X-Foo': 'bar' }`).
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(&self, ctx: Ctx<'js>, headers: Value<'js>) -> rquickjs::Result<()> {
    let map: FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
    self.inner.set_extra_http_headers(&map).await.into_js()
  }

  // ── Init scripts ──────────────────────────────────────────────────────────

  /// Register a JS snippet to run on every new page in this context before
  /// page scripts execute. Returns identifier tokens for the injected scripts.
  #[qjs(rename = "addInitScript")]
  pub async fn add_init_script(&self, source: String) -> rquickjs::Result<Vec<String>> {
    self.inner.add_init_script(&source).await.into_js()
  }

  // ── Timeouts ──────────────────────────────────────────────────────────────

  // `set_default_timeout` takes `&mut self` on core, which rquickjs can't
  // safely expose on `&self`. Expose read-only for now; callers that need to
  // change it can do so via the page's own timeout setters.

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  /// Name of the session this context belongs to.
  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Close the context (tears down the underlying browser state).
  #[qjs(rename = "close")]
  pub async fn close(&self) -> rquickjs::Result<()> {
    self.inner.close().await.into_js()
  }
}
