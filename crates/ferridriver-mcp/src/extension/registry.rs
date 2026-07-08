//! Shared registry of loaded extensions.
//!
//! The registry owns the canonical list of extension FILES after discovery.
//! Each file may declare one or more tools; the registry exposes
//! tool-level views (lookup by name, iterate promoted tools) and
//! file-level views (for binding installation, which needs the
//! source text + every tool the file declares).

use std::sync::Arc;

use rustc_hash::FxHashMap;

use super::loader::LoadedExtension;
use super::manifest::ToolManifest;

/// Read-only collection of loaded extension files. Cheap to clone -- the
/// inner `Vec` is wrapped in `Arc` so all consumers share the same data.
#[derive(Default, Clone)]
pub struct ExtensionRegistry {
  files: Arc<Vec<LoadedExtension>>,
  /// Per-file/spec load failures recorded at startup (discovery,
  /// bundle/compile, manifest extraction). Kept so the
  /// `ferridriver_extensions` tool can report what failed to load —
  /// previously the only trace was a startup log line.
  errors: Arc<Vec<(String, String)>>,
  /// Pre-compiled `inputSchema` validator per tool name, or the error
  /// message an invalid schema produces. Built once here so tool
  /// invocations look a validator up instead of recompiling the schema
  /// on every call.
  validators: Arc<FxHashMap<String, Result<jsonschema::Validator, String>>>,
}

impl std::fmt::Debug for ExtensionRegistry {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // `jsonschema::Validator` is not `Debug`; render the compile
    // outcome per tool instead of the validator itself.
    let validators: Vec<(&str, Result<&str, &str>)> = self
      .validators
      .iter()
      .map(|(name, v)| (name.as_str(), v.as_ref().map(|_| "ok").map_err(String::as_str)))
      .collect();
    f.debug_struct("ExtensionRegistry")
      .field("files", &self.files)
      .field("errors", &self.errors)
      .field("validators", &validators)
      .finish()
  }
}

impl ExtensionRegistry {
  #[must_use]
  pub fn new(files: Vec<LoadedExtension>, errors: Vec<(String, String)>) -> Self {
    let validators = files
      .iter()
      .flat_map(|f| f.tools.iter())
      .filter_map(|t| {
        let schema = t.input_schema.as_ref()?;
        let compiled = jsonschema::validator_for(schema)
          .map_err(|e| format!("extension `{}` has an invalid inputSchema: {e}", t.name));
        Some((t.name.clone(), compiled))
      })
      .collect();
    Self {
      files: Arc::new(files),
      errors: Arc::new(errors),
      validators: Arc::new(validators),
    }
  }

  /// `(source, message)` pairs for everything that failed to load at
  /// startup.
  #[must_use]
  pub fn errors(&self) -> &[(String, String)] {
    &self.errors
  }

  /// The pre-compiled validator for `name`'s `inputSchema` (`None` when
  /// the tool declared no schema; `Some(Err(_))` when the declared
  /// schema itself is invalid).
  #[must_use]
  pub fn validator(&self, name: &str) -> Option<&Result<jsonschema::Validator, String>> {
    self.validators.get(name)
  }

  /// Loaded extension files, one per discovered source file (any
  /// bundleable extension: `.js .cjs .mjs .jsx .ts .cts .mts .tsx`).
  #[must_use]
  pub fn files(&self) -> &[LoadedExtension] {
    &self.files
  }

  /// Iterator over every tool across every file.
  pub fn tools(&self) -> impl Iterator<Item = &ToolManifest> {
    self.files.iter().flat_map(|f| f.tools.iter())
  }

  /// Find a tool by manifest name (linear scan; tool counts are small).
  #[must_use]
  pub fn get_tool(&self, name: &str) -> Option<&ToolManifest> {
    self.tools().find(|t| t.name == name)
  }

  /// Iterator over tools that opted into top-level MCP tool exposure.
  pub fn promoted_tools(&self) -> impl Iterator<Item = &ToolManifest> {
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
