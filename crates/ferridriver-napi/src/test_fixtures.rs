//! NAPI `TestFixtures` class — thin wrapper over the core `TestFixtures` model.
//!
//! Unified fixture bag for both E2E and BDD callbacks. E2E tests get
//! browser/page/context/request/testInfo. BDD steps additionally get
//! args/dataTable/docString.

use std::sync::Arc;

use napi_derive::napi;

/// Fixture object passed to all JS callbacks (tests, steps, hooks).
///
/// E2E usage:
/// ```js
/// test('name', async ({ page, browserName, testInfo }) => { ... });
/// ```
///
/// BDD usage:
/// ```js
/// Given('I click {string}', async ({ page, args: [selector], testInfo }) => { ... });
/// ```
#[napi]
pub struct TestFixtures {
  inner: ferridriver_test::model::TestFixtures,
}

impl TestFixtures {
  /// Wrap a core `TestFixtures` for NAPI consumption.
  pub(crate) fn wrap(inner: ferridriver_test::model::TestFixtures) -> Self {
    Self { inner }
  }

}

#[napi]
impl TestFixtures {
  // ── Core fixtures ──

  #[napi(getter)]
  pub fn browser(&self) -> crate::browser::Browser {
    crate::browser::Browser::wrap((*self.inner.browser).clone())
  }

  #[napi(getter)]
  pub fn page(&self) -> crate::page::Page {
    crate::page::Page::wrap((*self.inner.page).clone())
  }

  #[napi(getter)]
  pub fn context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap((*self.inner.context).clone())
  }

  #[napi(getter)]
  pub fn request(&self) -> crate::api_request::ApiRequestContext {
    crate::api_request::ApiRequestContext::wrap((*self.inner.request).clone())
  }

  #[napi(getter, js_name = "testInfo")]
  pub fn test_info(&self) -> crate::test_info::TestInfo {
    crate::test_info::TestInfo::new(Arc::clone(&self.inner.test_info), Arc::clone(&self.inner.modifiers))
  }

  // ── Browser config fixtures (Playwright worker-scoped options) ──

  #[napi(getter, js_name = "browserName")]
  pub fn browser_name(&self) -> String {
    self.inner.browser_config.browser.clone()
  }

  #[napi(getter)]
  pub fn headless(&self) -> bool {
    self.inner.browser_config.headless
  }

  #[napi(getter)]
  pub fn channel(&self) -> Option<String> {
    self.inner.browser_config.channel.clone()
  }

  // ── Context config fixtures (Playwright test-scoped options) ──

  #[napi(getter, js_name = "isMobile")]
  pub fn is_mobile(&self) -> bool {
    self.inner.browser_config.context.is_mobile
  }

  #[napi(getter, js_name = "hasTouch")]
  pub fn has_touch(&self) -> bool {
    self.inner.browser_config.context.has_touch
  }

  #[napi(getter, js_name = "colorScheme")]
  pub fn color_scheme(&self) -> Option<String> {
    self.inner.browser_config.context.color_scheme.clone()
  }

  #[napi(getter)]
  pub fn locale(&self) -> Option<String> {
    self.inner.browser_config.context.locale.clone()
  }

  // ── BDD fixtures (None for E2E tests/hooks) ──

  /// Extracted parameters from the BDD step expression (typed: int→number, string→string).
  /// Returns null for E2E tests.
  #[napi(getter, ts_return_type = "unknown[] | null")]
  pub fn args(&self) -> Option<serde_json::Value> {
    self.inner.bdd_args.as_ref().map(|a| serde_json::Value::Array(a.clone()))
  }

  /// Alias for `args`.
  #[napi(getter, ts_return_type = "unknown[] | null")]
  pub fn params(&self) -> Option<serde_json::Value> {
    self.args()
  }

  /// The inline data table attached to this BDD step, if any.
  #[napi(getter, js_name = "dataTable")]
  pub fn data_table(&self) -> Option<Vec<Vec<String>>> {
    self.inner.bdd_data_table.clone()
  }

  /// The doc string attached to this BDD step, if any.
  #[napi(getter, js_name = "docString")]
  pub fn doc_string(&self) -> Option<String> {
    self.inner.bdd_doc_string.clone()
  }
}
