//! `playwright` first-class fixture — Playwright-shaped namespace
//! exposing the three browser-type singletons and the request factory.
//!
//! Usage in a test body matches Playwright verbatim:
//!
//! ```ts
//! test('plain', async ({ playwright }) => {
//!   const browser = await playwright.chromium.launch();
//!   const ctx = await playwright.request.newContext({ baseURL: '...' });
//! });
//! ```
//!
//! Each accessor returns a fresh wrapper because the underlying Rust
//! `BrowserType` is cheap to construct and the JS-side wrappers are
//! single-use (one `.launch()` per test). The wrappers persist for the
//! lifetime of the fixture object.

#![allow(dead_code)]

use crate::api_request::{ApiRequestContext, ApiRequestOptions};
use crate::browser_type::BrowserType;
use ferridriver::options as core_opts;
use napi::Result;
use napi_derive::napi;

/// Playwright `request` namespace exposed via `playwright.request`.
///
/// Mirrors `import { request } from 'playwright'` — the only method is
/// `newContext()` which constructs an `APIRequestContext`.
#[napi]
pub struct PlaywrightRequest;

#[napi]
impl PlaywrightRequest {
  /// Playwright `apiRequest.newContext(options?) -> Promise<APIRequestContext>`.
  ///
  /// Construction is synchronous on our side (no live handshake), but
  /// the signature matches Playwright's async API so test code can use
  /// `await playwright.request.newContext(...)` unchanged.
  #[napi]
  pub async fn new_context(&self, options: Option<ApiRequestOptions>) -> Result<ApiRequestContext> {
    ApiRequestContext::create(options)
  }
}

/// First-class `playwright` fixture object.
///
/// Returned from `TestFixtures.playwright`. Mirrors
/// `import * as playwright from 'playwright'` — the three browser-type
/// accessors plus the `request` namespace.
#[napi]
pub struct PlaywrightNamespace;

#[napi]
impl PlaywrightNamespace {
  /// Playwright `playwright.chromium` — the Chromium `BrowserType`.
  #[napi(getter)]
  pub fn chromium(&self) -> BrowserType {
    BrowserType::wrap(ferridriver::BrowserType::chromium_with(
      &core_opts::BrowserTypeOptions { transport: None },
    ))
  }

  /// Playwright `playwright.firefox` — the Firefox `BrowserType`.
  #[napi(getter)]
  pub fn firefox(&self) -> BrowserType {
    BrowserType::wrap(ferridriver::BrowserType::firefox())
  }

  /// Playwright `playwright.webkit` — the WebKit `BrowserType`.
  #[napi(getter)]
  pub fn webkit(&self) -> BrowserType {
    BrowserType::wrap(ferridriver::BrowserType::webkit())
  }

  /// Playwright `playwright.request` — the `APIRequest` namespace.
  #[napi(getter)]
  pub fn request(&self) -> PlaywrightRequest {
    PlaywrightRequest
  }
}
