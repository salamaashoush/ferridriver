//! NAPI `TestFixtures` class — the fixture bag passed to JS test callbacks.
//!
//! Mirrors Playwright's `({ page, browserName, isMobile, ... }, testInfo)` signature.
//! In ferridriver, everything is in a single fixtures object: `({ page, testInfo, browserName, ... })`.

use std::sync::Arc;

use napi_derive::napi;

/// Fixture object passed to JS test callbacks.
///
/// Matches Playwright's test function signature — JS tests destructure this:
/// ```js
/// test('name', async ({ page, browserName, testInfo }) => { ... });
/// ```
#[napi]
pub struct TestFixtures {
  /// Core page (Clone-able, reconstructed as NAPI Page on each getter call).
  inner_page: ferridriver::Page,
  /// Core context.
  inner_context: ferridriver::context::ContextRef,
  /// Core request context.
  inner_request: Arc<ferridriver::api_request::APIRequestContext>,
  /// Core test info + modifiers (shared Arc).
  inner_test_info: Arc<ferridriver_test::model::TestInfo>,
  modifiers: Arc<ferridriver_test::model::TestModifiers>,
  // Browser config fixtures.
  browser_name: String,
  headless: bool,
  is_mobile: bool,
  has_touch: bool,
  color_scheme: Option<String>,
  locale: Option<String>,
  channel: Option<String>,
}

impl TestFixtures {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    page: ferridriver::Page,
    context: ferridriver::context::ContextRef,
    request: Arc<ferridriver::api_request::APIRequestContext>,
    test_info: Arc<ferridriver_test::model::TestInfo>,
    modifiers: Arc<ferridriver_test::model::TestModifiers>,
    browser_name: String,
    headless: bool,
    is_mobile: bool,
    has_touch: bool,
    color_scheme: Option<String>,
    locale: Option<String>,
    channel: Option<String>,
  ) -> Self {
    Self {
      inner_page: page,
      inner_context: context,
      inner_request: request,
      inner_test_info: test_info,
      modifiers,
      browser_name,
      headless,
      is_mobile,
      has_touch,
      color_scheme,
      locale,
      channel,
    }
  }
}

#[napi]
impl TestFixtures {
  // ── Core fixtures ──

  #[napi(getter)]
  pub fn page(&self) -> crate::page::Page {
    crate::page::Page::wrap(self.inner_page.clone())
  }

  #[napi(getter)]
  pub fn context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap(self.inner_context.clone())
  }

  #[napi(getter)]
  pub fn request(&self) -> crate::api_request::ApiRequestContext {
    crate::api_request::ApiRequestContext::wrap((*self.inner_request).clone())
  }

  #[napi(getter, js_name = "testInfo")]
  pub fn test_info(&self) -> crate::test_info::TestInfo {
    crate::test_info::TestInfo::new(Arc::clone(&self.inner_test_info), Arc::clone(&self.modifiers))
  }

  // ── Browser config fixtures (Playwright worker-scoped options) ──

  #[napi(getter, js_name = "browserName")]
  pub fn browser_name(&self) -> String {
    self.browser_name.clone()
  }

  #[napi(getter)]
  pub fn headless(&self) -> bool {
    self.headless
  }

  #[napi(getter)]
  pub fn channel(&self) -> Option<String> {
    self.channel.clone()
  }

  // ── Context config fixtures (Playwright test-scoped options) ──

  #[napi(getter, js_name = "isMobile")]
  pub fn is_mobile(&self) -> bool {
    self.is_mobile
  }

  #[napi(getter, js_name = "hasTouch")]
  pub fn has_touch(&self) -> bool {
    self.has_touch
  }

  #[napi(getter, js_name = "colorScheme")]
  pub fn color_scheme(&self) -> Option<String> {
    self.color_scheme.clone()
  }

  #[napi(getter)]
  pub fn locale(&self) -> Option<String> {
    self.locale.clone()
  }
}
