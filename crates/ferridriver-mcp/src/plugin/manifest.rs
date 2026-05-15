//! Plugin manifest types -- the user-facing contract that plugin files declare.
//!
//! A plugin file sets `globalThis.exports = { ... }` whose JSON-shaped subset
//! deserialises into [`PluginManifest`]. The `handler` field on the JS side
//! is intentionally NOT part of this struct -- it carries an executable
//! closure that only makes sense inside a live `QuickJS` context, so the
//! loader strips it before extraction.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Manifest extracted from a plugin's `globalThis.exports` declaration.
///
/// Field naming follows the JS convention (`camelCase`) since that's what
/// plugin authors type. The Rust side stays `snake_case` via serde rename.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
  /// Globally unique handler name. Used as the binding key under the
  /// `plugins` global (`plugins['box.login']`) and as the MCP tool name
  /// when `expose_as_tool` is true. Dot-separated namespacing is recommended.
  pub name: String,

  /// Human-readable description. Surfaced in `tools/list` when the plugin
  /// is promoted to a tool. Optional for binding-only plugins but strongly
  /// recommended.
  #[serde(default)]
  pub description: Option<String>,

  /// JSON Schema describing the plugin's input arguments. Used both for
  /// the promoted tool's `inputSchema` and for argument validation before
  /// the handler runs. `serde_json::Value` so plugin authors can use any
  /// valid JSON Schema construct without us re-encoding it.
  #[serde(default)]
  pub input_schema: Option<serde_json::Value>,

  /// Whitelist of capabilities the handler may invoke. See [`PluginAllow`].
  #[serde(default)]
  pub allow: PluginAllow,

  /// When true, the plugin is registered as a first-class MCP tool with
  /// `name` as the tool name. Both the tool invocation and the binding
  /// invocation route through the same handler.
  #[serde(default)]
  pub expose_as_tool: bool,
}

/// Capability allow-list bundled with the manifest.
///
/// Anything not declared here is denied at runtime. The whole point of the
/// plugin sandbox is that escapes (shell, network) are opt-in and auditable
/// from the manifest -- the handler source alone cannot grant itself
/// new privileges.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAllow {
  /// Named CLI templates the handler may invoke via `commands.run(name, vars)`.
  /// Each template may reference handler-supplied vars as `${var}` placeholders.
  /// The plugin handler picks names -- runtime substitutes vars literally
  /// after shell-escaping each value.
  #[serde(default)]
  pub commands: HashMap<String, String>,
}

impl PluginManifest {
  /// Returns `true` when the plugin should be surfaced in `tools/list`.
  #[must_use]
  pub fn is_tool(&self) -> bool {
    self.expose_as_tool
  }
}
