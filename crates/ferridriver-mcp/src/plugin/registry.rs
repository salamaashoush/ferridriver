//! Shared registry of loaded plugins.
//!
//! The registry owns the canonical list of plugin FILES after discovery.
//! Each file may declare one or more tools; the registry exposes
//! tool-level views (lookup by name, iterate promoted tools) and
//! file-level views (for binding installation, which needs the
//! source text + every tool the file declares).

use std::sync::Arc;

use super::loader::LoadedPlugin;
use super::manifest::PluginManifest;

/// Read-only collection of loaded plugin files. Cheap to clone -- the
/// inner `Vec` is wrapped in `Arc` so all consumers share the same data.
#[derive(Debug, Default, Clone)]
pub struct PluginRegistry {
  files: Arc<Vec<LoadedPlugin>>,
}

impl PluginRegistry {
  #[must_use]
  pub fn new(files: Vec<LoadedPlugin>) -> Self {
    Self { files: Arc::new(files) }
  }

  /// Source files (one per `.js`/`.mjs` plugin loaded).
  #[must_use]
  pub fn files(&self) -> &[LoadedPlugin] {
    &self.files
  }

  /// Iterator over every tool across every file.
  pub fn tools(&self) -> impl Iterator<Item = &PluginManifest> {
    self.files.iter().flat_map(|f| f.tools.iter())
  }

  /// Find a tool by manifest name (linear scan; tool counts are small).
  #[must_use]
  pub fn get_tool(&self, name: &str) -> Option<&PluginManifest> {
    self.tools().find(|t| t.name == name)
  }

  /// Iterator over tools that opted into top-level MCP tool exposure.
  pub fn promoted_tools(&self) -> impl Iterator<Item = &PluginManifest> {
    self.tools().filter(|t| t.is_tool())
  }

  /// Total tool count across all files.
  #[must_use]
  pub fn tool_count(&self) -> usize {
    self.files.iter().map(|f| f.tools.len()).sum()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.files.is_empty()
  }
}
