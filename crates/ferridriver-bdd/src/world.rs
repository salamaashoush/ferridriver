//! BrowserWorld: shared scenario state with Page, variables, and typed extensions.

use std::any::{Any, TypeId};
use std::sync::Arc;

use rustc_hash::FxHashMap;

use ferridriver::context::ContextRef;
use ferridriver::Page;

/// Shared mutable state for a single BDD scenario.
///
/// Each scenario gets a fresh `BrowserWorld` with a new browser context and page.
/// Custom state can be stored via the type-map methods (`set_state` / `get_state`).
pub struct BrowserWorld {
  page: Page,
  context: ContextRef,
  vars: FxHashMap<String, String>,
  state: FxHashMap<TypeId, Box<dyn Any + Send + Sync>>,
  test_info: Option<Arc<ferridriver_test::model::TestInfo>>,
  registry: Option<Arc<crate::registry::StepRegistry>>,
}

impl BrowserWorld {
  /// Create a new world with the given page and context.
  pub fn new(page: Page, context: ContextRef) -> Self {
    Self {
      page,
      context,
      vars: FxHashMap::default(),
      state: FxHashMap::default(),
      test_info: None,
      registry: None,
    }
  }

  /// Access the browser page.
  pub fn page(&self) -> &Page {
    &self.page
  }

  /// Mutable access to the browser page.
  pub fn page_mut(&mut self) -> &mut Page {
    &mut self.page
  }

  /// Access the browser context (for cookies, permissions, etc.).
  pub fn context(&self) -> &ContextRef {
    &self.context
  }

  /// Access scenario variables.
  pub fn vars(&self) -> &FxHashMap<String, String> {
    &self.vars
  }

  /// Mutable access to scenario variables.
  pub fn vars_mut(&mut self) -> &mut FxHashMap<String, String> {
    &mut self.vars
  }

  /// Get a variable value by name.
  pub fn var(&self, name: &str) -> Option<&str> {
    self.vars.get(name).map(String::as_str)
  }

  /// Set a variable value.
  pub fn set_var(&mut self, name: impl Into<String>, value: impl Into<String>) {
    self.vars.insert(name.into(), value.into());
  }

  /// Get typed state by type.
  pub fn get_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
    self.state.get(&TypeId::of::<T>())?.downcast_ref()
  }

  /// Get mutable typed state by type.
  pub fn get_state_mut<T: Send + Sync + 'static>(&mut self) -> Option<&mut T> {
    self.state.get_mut(&TypeId::of::<T>())?.downcast_mut()
  }

  /// Set typed state. Overwrites any previous value of the same type.
  pub fn set_state<T: Send + Sync + 'static>(&mut self, val: T) {
    self.state.insert(TypeId::of::<T>(), Box::new(val));
  }

  /// Remove and return typed state.
  pub fn take_state<T: Send + Sync + 'static>(&mut self) -> Option<T> {
    self.state.remove(&TypeId::of::<T>()).and_then(|b| b.downcast().ok()).map(|b| *b)
  }

  /// Set the test info for attachments.
  pub fn set_test_info(&mut self, info: Arc<ferridriver_test::model::TestInfo>) {
    self.test_info = Some(info);
  }

  /// Set the step registry for step composition.
  pub fn set_registry(&mut self, registry: Arc<crate::registry::StepRegistry>) {
    self.registry = Some(registry);
  }

  /// Attach binary data to the current test (appears in reports).
  pub async fn attach(&self, name: &str, content_type: &str, data: Vec<u8>) {
    if let Some(info) = &self.test_info {
      info.attach(
        name.to_string(),
        content_type.to_string(),
        ferridriver_test::model::AttachmentBody::Bytes(data),
      ).await;
    }
  }

  /// Log a text message as an attachment.
  pub async fn log(&self, text: &str) {
    self.attach("log", "text/plain", text.as_bytes().to_vec()).await;
  }

  /// Execute another step from within a step handler (step composition).
  pub async fn run_step(&mut self, text: &str) -> Result<(), crate::step::StepError> {
    let registry = self.registry.clone().ok_or_else(|| {
      crate::step::StepError::from("step composition requires registry (internal error)")
    })?;
    let step_match = registry.find_match(text).map_err(|e| crate::step::StepError::from(e.to_string()))?;
    (step_match.def.handler)(self, step_match.params, None, None).await
  }

  /// Interpolate variables in a string.
  /// `$name` is replaced with the variable value. `$$` escapes to `$`.
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
