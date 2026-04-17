//! Session-level variable store exposed to scripts as the `vars` global.
//!
//! The engine takes a `VarsStore` trait object so callers (the MCP server)
//! can plug in their own storage. A simple in-memory implementation is
//! provided for tests and stand-alone use.

use std::sync::RwLock;

use rustc_hash::FxHashMap;

/// Session-level key/value store.
///
/// Called from JS via the `vars` global: `vars.get(name)`, `vars.set(name, value)`,
/// `vars.has(name)`, `vars.delete(name)`, `vars.keys()`.
pub trait VarsStore: Send + Sync {
  fn get(&self, name: &str) -> Option<String>;
  fn set(&self, name: &str, value: String);
  fn has(&self, name: &str) -> bool;
  fn delete(&self, name: &str);
  fn keys(&self) -> Vec<String>;
}

/// Simple in-memory `VarsStore` backed by an `RwLock<FxHashMap>`.
///
/// Cheap to construct and safe to share across script runs; the MCP server
/// holds one of these per session so `vars` survives across `run_script` calls.
#[derive(Default)]
pub struct InMemoryVars {
  inner: RwLock<FxHashMap<String, String>>,
}

impl InMemoryVars {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }
}

impl VarsStore for InMemoryVars {
  fn get(&self, name: &str) -> Option<String> {
    self.inner.read().ok().and_then(|guard| guard.get(name).cloned())
  }

  fn set(&self, name: &str, value: String) {
    if let Ok(mut guard) = self.inner.write() {
      guard.insert(name.to_string(), value);
    }
  }

  fn has(&self, name: &str) -> bool {
    self.inner.read().ok().is_some_and(|guard| guard.contains_key(name))
  }

  fn delete(&self, name: &str) {
    if let Ok(mut guard) = self.inner.write() {
      guard.remove(name);
    }
  }

  fn keys(&self) -> Vec<String> {
    self
      .inner
      .read()
      .map(|guard| guard.keys().cloned().collect())
      .unwrap_or_default()
  }
}
