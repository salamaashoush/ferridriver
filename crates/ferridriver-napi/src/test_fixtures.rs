//! NAPI `TestFixtures` — pool-backed lazy fixture resolution.
//!
//! Single construction path: all fixtures resolve lazily from a `FixturePool`
//! via sync DashMap reads. No eager fetching, no cloning until a getter is
//! actually called by JS.

use std::sync::Arc;

use napi_derive::napi;

/// Fixture object passed to all JS callbacks (tests, steps, hooks).
/// Backed by a FixturePool — getters resolve lazily via sync DashMap reads.
#[napi]
pub struct TestFixtures {
  pool: ferridriver_test::fixture::FixturePool,
  test_info: Arc<ferridriver_test::model::TestInfo>,
  modifiers: Arc<ferridriver_test::model::TestModifiers>,
  browser_config: ferridriver_test::config::BrowserConfig,
  bdd_args: Option<Vec<serde_json::Value>>,
  bdd_data_table: Option<Vec<Vec<String>>>,
  bdd_doc_string: Option<String>,
}

impl TestFixtures {
  /// Construct from a pool. The only construction path.
  /// Pool already has fixtures cached in its DashMap — getters do sync reads.
  pub(crate) fn from_pool(
    pool: ferridriver_test::fixture::FixturePool,
    test_info: Arc<ferridriver_test::model::TestInfo>,
    modifiers: Arc<ferridriver_test::model::TestModifiers>,
    browser_config: ferridriver_test::config::BrowserConfig,
  ) -> Self {
    Self {
      pool,
      test_info,
      modifiers,
      browser_config,
      bdd_args: None,
      bdd_data_table: None,
      bdd_doc_string: None,
    }
  }

  /// Construct from already-resolved model::TestFixtures.
  /// Injects the resolved fixtures into a fresh pool so getters
  /// use the same code path.
  pub(crate) fn from_resolved(inner: ferridriver_test::model::TestFixtures) -> Self {
    let pool = ferridriver_test::fixture::FixturePool::new(
      rustc_hash::FxHashMap::default(),
      ferridriver_test::fixture::FixtureScope::Test,
    );
    pool.inject("browser", inner.browser);
    pool.inject("page", inner.page);
    pool.inject("context", inner.context);
    pool.inject("request", inner.request);

    Self {
      pool,
      test_info: inner.test_info,
      modifiers: inner.modifiers,
      browser_config: inner.browser_config,
      bdd_args: inner.bdd_args,
      bdd_data_table: inner.bdd_data_table,
      bdd_doc_string: inner.bdd_doc_string,
    }
  }
}

#[napi]
impl TestFixtures {
  #[napi(getter)]
  pub fn browser(&self) -> crate::browser::Browser {
    let b = self
      .pool
      .try_get_cached::<ferridriver::Browser>("browser")
      .expect("fixture 'browser' not resolved");
    crate::browser::Browser::wrap((*b).clone())
  }

  #[napi(getter)]
  pub fn page(&self) -> crate::page::Page {
    let p = self
      .pool
      .try_get_cached::<ferridriver::Page>("page")
      .expect("fixture 'page' not resolved");
    crate::page::Page::wrap(p)
  }

  #[napi(getter)]
  pub fn context(&self) -> crate::context::BrowserContext {
    let c = self
      .pool
      .try_get_cached::<ferridriver::context::ContextRef>("context")
      .expect("fixture 'context' not resolved");
    crate::context::BrowserContext::wrap((*c).clone())
  }

  #[napi(getter)]
  pub fn request(&self) -> crate::api_request::ApiRequestContext {
    let r = self
      .pool
      .try_get_cached::<ferridriver::api_request::APIRequestContext>("request")
      .expect("fixture 'request' not resolved");
    crate::api_request::ApiRequestContext::wrap((*r).clone())
  }

  #[napi(getter, js_name = "testInfo")]
  pub fn test_info(&self) -> crate::test_info::TestInfo {
    crate::test_info::TestInfo::new(Arc::clone(&self.test_info), Arc::clone(&self.modifiers))
  }

  #[napi(getter, js_name = "browserName")]
  pub fn browser_name(&self) -> String {
    self.browser_config.browser.clone()
  }

  #[napi(getter)]
  pub fn headless(&self) -> bool {
    self.browser_config.headless
  }

  #[napi(getter)]
  pub fn channel(&self) -> Option<String> {
    self.browser_config.channel.clone()
  }

  #[napi(getter, js_name = "isMobile")]
  pub fn is_mobile(&self) -> bool {
    self.browser_config.context.is_mobile
  }

  #[napi(getter, js_name = "hasTouch")]
  pub fn has_touch(&self) -> bool {
    self.browser_config.context.has_touch
  }

  #[napi(getter, js_name = "colorScheme")]
  pub fn color_scheme(&self) -> Option<String> {
    self.browser_config.context.color_scheme.clone()
  }

  #[napi(getter)]
  pub fn locale(&self) -> Option<String> {
    self.browser_config.context.locale.clone()
  }

  #[napi(getter, ts_return_type = "unknown[] | null")]
  pub fn args(&self) -> Option<serde_json::Value> {
    self.bdd_args.as_ref().map(|a| serde_json::Value::Array(a.clone()))
  }

  #[napi(getter, ts_return_type = "unknown[] | null")]
  pub fn params(&self) -> Option<serde_json::Value> {
    self.args()
  }

  #[napi(getter, js_name = "dataTable")]
  pub fn data_table(&self) -> Option<Vec<Vec<String>>> {
    self.bdd_data_table.clone()
  }

  #[napi(getter, js_name = "docString")]
  pub fn doc_string(&self) -> Option<String> {
    self.bdd_doc_string.clone()
  }
}
