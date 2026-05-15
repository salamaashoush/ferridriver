//! Shared registry of loaded plugins.
//!
//! The registry owns the canonical list of plugins after discovery. It is
//! cloned (cheaply, behind `Arc`) into:
//!
//! - `McpServer` for tool-list / tool-dispatch (when a plugin sets
//!   `exposeAsTool: true`)
//! - `RunContext` for binding installation inside each `run_script`
//!   invocation
//! - `commands.run` native callback for allow-list lookup

use std::sync::Arc;

use super::loader::LoadedPlugin;

/// Read-only collection of loaded plugins. Cheap to clone -- the inner
/// `Vec` is wrapped in `Arc` so all consumers share the same data.
#[derive(Debug, Default, Clone)]
pub struct PluginRegistry {
  plugins: Arc<Vec<LoadedPlugin>>,
}

impl PluginRegistry {
  #[must_use]
  pub fn new(plugins: Vec<LoadedPlugin>) -> Self {
    Self {
      plugins: Arc::new(plugins),
    }
  }

  #[must_use]
  pub fn plugins(&self) -> &[LoadedPlugin] {
    &self.plugins
  }

  #[must_use]
  pub fn get(&self, name: &str) -> Option<&LoadedPlugin> {
    self.plugins.iter().find(|p| p.manifest.name == name)
  }

  pub fn promoted_tools(&self) -> impl Iterator<Item = &LoadedPlugin> {
    self.plugins.iter().filter(|p| p.manifest.is_tool())
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.plugins.is_empty()
  }
}
