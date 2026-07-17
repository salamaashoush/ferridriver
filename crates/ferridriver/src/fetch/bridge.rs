//! Two-way bridge between the engine and a browser context.
//!
//! Playwright's `BrowserContextAPIRequestContext`
//! (`server/fetch.ts:649`) shares the browser context's cookie jar in
//! both directions: the outgoing `Cookie` header is assembled from
//! `context.cookies()` before every hop, and every hop's `Set-Cookie`
//! headers are written back via `context.addCookies()`. reqwest's own
//! jar can't do that (cookies must live in the BROWSER, and each hop
//! needs a fresh jar read), so the bridged path follows redirects
//! manually and reads/writes cookies through this trait.

/// Boxed future used by [`ContextBridge`] (`async fn` in traits is not
/// dyn-compatible).
pub type BridgeFuture<'a, T> =
  std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<T>> + Send + 'a>>;

/// Live per-request defaults sourced from the owning browser context.
/// Mirrors the subset of Playwright's `_defaultOptions()` (fetch.ts:666)
/// ferridriver's context options carry today.
#[derive(Debug, Clone, Default)]
pub struct ContextDefaults {
  pub base_url: Option<String>,
  pub extra_http_headers: Vec<(String, String)>,
  pub user_agent: Option<String>,
  pub ignore_https_errors: bool,
}

/// Two-way bridge between an `HttpClient` and a browser context.
/// Implemented by `ContextRef`; read live on every request so option
/// mutations (`setExtraHTTPHeaders`) and browser-side cookie changes are
/// always visible, matching Playwright's live `_defaultOptions()` read.
pub trait ContextBridge: Send + Sync {
  fn defaults(&self) -> BridgeFuture<'_, ContextDefaults>;
  fn cookies(&self) -> BridgeFuture<'_, Vec<crate::backend::CookieData>>;
  fn add_cookies(&self, cookies: Vec<crate::backend::CookieData>) -> BridgeFuture<'_, ()>;
}
