//! BrowserWorld: shared scenario state with fixtures, variables, and typed extensions.

use std::any::{Any, TypeId};
use std::sync::Arc;

use rustc_hash::FxHashMap;

use ferridriver::Page;
use ferridriver::context::ContextRef;

/// Shared mutable state for a single BDD scenario.
///
/// Holds the unified `TestFixtures` from the core runner, plus scenario-specific
/// state (variables, typed extensions, registry). Built-in steps access page/context
/// via delegate methods; NAPI step handlers access the full `TestFixtures` directly.
pub struct BrowserWorld {
  fixtures: ferridriver_test::model::TestFixtures,
  vars: FxHashMap<String, String>,
  state: FxHashMap<TypeId, Box<dyn Any + Send + Sync>>,
  registry: Option<Arc<crate::registry::StepRegistry>>,
  feature_dir: Option<std::path::PathBuf>,
}

impl BrowserWorld {
  /// Create a new world from the unified test fixtures.
  pub fn new(fixtures: ferridriver_test::model::TestFixtures) -> Self {
    Self {
      fixtures,
      vars: FxHashMap::default(),
      state: FxHashMap::default(),
      registry: None,
      feature_dir: None,
    }
  }

  /// Access the unified test fixtures.
  pub fn fixtures(&self) -> &ferridriver_test::model::TestFixtures {
    &self.fixtures
  }

  /// Mutable access to the unified test fixtures.
  pub fn fixtures_mut(&mut self) -> &mut ferridriver_test::model::TestFixtures {
    &mut self.fixtures
  }

  // ── Delegate accessors (used by built-in steps) ──

  pub fn page(&self) -> &Arc<Page> {
    &self.fixtures.page
  }

  /// Replace the active page Arc entirely (used for tab switching).
  pub fn set_page(&mut self, page: Arc<Page>) {
    self.fixtures.page = page;
  }

  pub fn context(&self) -> &ContextRef {
    &self.fixtures.context
  }

  pub fn browser(&self) -> &ferridriver::Browser {
    &self.fixtures.browser
  }

  pub fn request(&self) -> &ferridriver::api_request::APIRequestContext {
    &self.fixtures.request
  }

  pub fn test_info(&self) -> &Arc<ferridriver_test::model::TestInfo> {
    &self.fixtures.test_info
  }

  pub fn browser_config(&self) -> &ferridriver_test::config::BrowserConfig {
    &self.fixtures.browser_config
  }

  // ── Scenario variables ──

  pub fn vars(&self) -> &FxHashMap<String, String> {
    &self.vars
  }

  pub fn vars_mut(&mut self) -> &mut FxHashMap<String, String> {
    &mut self.vars
  }

  pub fn var(&self, name: &str) -> Option<&str> {
    self.vars.get(name).map(String::as_str)
  }

  pub fn set_var(&mut self, name: impl Into<String>, value: impl Into<String>) {
    self.vars.insert(name.into(), value.into());
  }

  // ── Typed state extensions ──

  pub fn get_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
    self.state.get(&TypeId::of::<T>())?.downcast_ref()
  }

  pub fn get_state_mut<T: Send + Sync + 'static>(&mut self) -> Option<&mut T> {
    self.state.get_mut(&TypeId::of::<T>())?.downcast_mut()
  }

  pub fn set_state<T: Send + Sync + 'static>(&mut self, val: T) {
    self.state.insert(TypeId::of::<T>(), Box::new(val));
  }

  pub fn take_state<T: Send + Sync + 'static>(&mut self) -> Option<T> {
    self
      .state
      .remove(&TypeId::of::<T>())
      .and_then(|b| b.downcast().ok())
      .map(|b| *b)
  }

  // ── Registry + feature dir ──

  pub fn set_registry(&mut self, registry: Arc<crate::registry::StepRegistry>) {
    self.registry = Some(registry);
  }

  pub fn set_feature_dir(&mut self, dir: std::path::PathBuf) {
    self.feature_dir = Some(dir);
  }

  /// Clear scenario-specific state between runs. Fixtures are preserved.
  pub fn reset_scenario_state(&mut self) {
    self.vars.clear();
    self.state.clear();
    self.feature_dir = None;
  }

  pub fn resolve_fixture_path(&self, relative: &str) -> std::path::PathBuf {
    if let Some(dir) = &self.feature_dir {
      dir.join(relative)
    } else {
      std::path::PathBuf::from(relative)
    }
  }

  pub fn registry_arc(&self) -> Option<Arc<crate::registry::StepRegistry>> {
    self.registry.clone()
  }

  pub async fn attach(&self, name: &str, content_type: &str, data: Vec<u8>) {
    self.fixtures.test_info
      .attach(
        name.to_string(),
        content_type.to_string(),
        ferridriver_test::model::AttachmentBody::Bytes(data),
      )
      .await;
  }

  pub async fn log(&self, text: &str) {
    self.attach("log", "text/plain", text.as_bytes().to_vec()).await;
  }

  pub async fn run_step(&mut self, text: &str) -> Result<(), crate::step::StepError> {
    let registry = self
      .registry
      .clone()
      .ok_or_else(|| crate::step::StepError::from("step composition requires registry (internal error)"))?;
    let step_match = registry
      .find_match(text)
      .map_err(|e| crate::step::StepError::from(e.to_string()))?;
    (step_match.def.handler)(self, step_match.params, None, None).await
  }

  pub fn interpolate(&self, text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
      if c == '$' {
        if chars.peek() == Some(&'$') {
          chars.next();
          result.push('$');
        } else {
          let mut name = String::new();
          while let Some(&nc) = chars.peek() {
            if nc.is_alphanumeric() || nc == '_' {
              name.push(nc);
              chars.next();
            } else {
              break;
            }
          }
          if let Some(val) = self.vars.get(&name) {
            result.push_str(val);
          } else {
            result.push('$');
            result.push_str(&name);
          }
        }
      } else {
        result.push(c);
      }
    }

    result
  }
}
