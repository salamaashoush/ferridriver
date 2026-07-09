//! Extension manifest types -- the user-facing contract that extension files declare.
//!
//! An extension file registers each tool with a top-level
//! `defineTool({ name, description, inputSchema, allow, exposeAsMcpTool,
//! timeoutMs, handler })` call; the JSON-shaped subset of that object
//! deserialises into [`ToolManifest`]. The `handler` field is
//! intentionally NOT part of this struct -- it carries an executable
//! closure that only makes sense inside a live `QuickJS` context, so
//! manifest extraction serialises everything except it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Manifest extracted from one top-level `defineTool(...)` registration.
///
/// Field naming follows the JS convention (`camelCase`) since that's what
/// extension authors type. The Rust side stays `snake_case` via serde rename.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolManifest {
  /// Globally unique handler name. Used as the binding key under the
  /// `tools` namespace (`tools.box.login`) and as the MCP tool name when
  /// `expose_as_mcp_tool` is true. Dot-separated namespacing is recommended.
  pub name: String,

  /// Human-readable display title, surfaced as the promoted tool's
  /// `title` in `tools/list` (MCP separates the machine `name` from the
  /// human label).
  #[serde(default)]
  pub title: Option<String>,

  /// Human-readable description. Surfaced in `tools/list` when the extension
  /// is promoted to a tool. Optional for binding-only extensions but strongly
  /// recommended.
  #[serde(default)]
  pub description: Option<String>,

  /// JSON Schema describing the extension's input arguments. Surfaced as the
  /// promoted tool's `inputSchema` and enforced: `invoke_extension_tool`
  /// validates the caller's arguments against it (Draft 2020-12 et al.,
  /// via the `jsonschema` crate) and rejects a non-conforming call before
  /// the handler runs. `serde_json::Value` so extension authors can use any
  /// valid JSON Schema construct without us re-encoding it.
  #[serde(default)]
  pub input_schema: Option<serde_json::Value>,

  /// JSON Schema describing the handler's return value. Surfaced as the
  /// promoted tool's `outputSchema` and enforced symmetrically to
  /// `input_schema`: `invoke_extension_tool` validates the handler's
  /// result against it before replying, and ships the result as MCP
  /// `structuredContent`. A non-conforming result is the extension
  /// author's bug, surfaced as a tool error.
  #[serde(default)]
  pub output_schema: Option<serde_json::Value>,

  /// MCP tool annotations (`title`, `readOnlyHint`, `destructiveHint`,
  /// `idempotentHint`, `openWorldHint`), passed through to `tools/list`
  /// verbatim. Hints only — the server enforces nothing from them.
  #[serde(default)]
  pub annotations: Option<rmcp::model::ToolAnnotations>,

  /// Whitelist of capabilities the handler may invoke. See [`ToolAllow`].
  #[serde(default)]
  pub allow: ToolAllow,

  /// When true, the extension is registered as a first-class MCP tool with
  /// `name` as the tool name. Both the tool invocation and the binding
  /// invocation route through the same handler.
  #[serde(default)]
  #[serde(alias = "exposeAsTool")]
  pub expose_as_mcp_tool: bool,

  /// Optional per-invocation handler timeout (milliseconds). Enforced
  /// natively for every caller in `extensions::dispatch_tool` (the handler
  /// is raced against this bound). Cooperative: the race can only win
  /// while the handler is awaiting — a CPU-spinning handler is halted by
  /// the session wall-clock interrupt, not this bound. `None` ⇒ only the
  /// session wall-clock applies.
  #[serde(default)]
  pub timeout_ms: Option<u64>,
}

/// Declarative capability manifest bundled with the extension.
///
/// This is the extension sandbox's opt-in authority list: each named
/// capability is independently scoped and Rust-enforced at the binding
/// boundary, so the handler source alone cannot grant itself a privilege
/// it did not declare. Defaults differ per capability: `commands` is
/// default-deny (an absent map grants nothing), while `net` is
/// default-open for back-compatibility (an absent list leaves HTTP
/// unrestricted; declaring any host flips it to default-deny).
///
/// The manifest is the AUTHOR's half of a two-party grant. The operator
/// half is the `[extensions.policy]` ceiling
/// (`ferridriver_config::ExtensionPolicyConfig`): at session
/// registration the effective authority becomes manifest ∩ ceiling — a
/// present `net` ceiling flips undeclared tools to default-deny and
/// drops declared entries outside it, and the `commands` ceiling can
/// forbid shell-form (or all) command declarations.
///
/// Covered today:
/// - **exec** (`commands`): named shell-command templates. Default-deny —
///   a `commands.run(name)` for an undeclared `name` throws.
/// - **net**: host allow-list for the handler's `request` HTTP client.
///   Empty = unrestricted (opt-in: declaring any host switches that
///   binding to default-deny). Scopes the `request` binding only;
///   `page`/`context` browser navigation is a separate, deliberately
///   ungated authority (an automation extension must be able to navigate).
///   There is no `fs` capability: the only filesystem reachable from a
///   handler is the session's `fs`/`artifacts` globals, both already
///   confined to their `PathSandbox` roots — an extension-level scope on
///   top of that has no ungated authority left to gate (see
///   `docs/extension-architecture.md`).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAllow {
  /// Named commands the handler may invoke. Each value is a shell-string
  /// shorthand or a [`ferridriver_script::CommandSpec`] object (argv vs
  /// shell, per-command timeout, env passthrough, cwd, output mode,
  /// `persistent`). Default-deny: a name not declared here cannot be
  /// run. `exec` is accepted as a synonym.
  #[serde(default, alias = "exec")]
  pub commands: HashMap<String, ferridriver_script::CommandSpec>,

  /// Host patterns the handler's `request` client may target. Each entry
  /// is an exact host (`api.box.com`) or a leading-wildcard suffix
  /// (`*.box.com`, which also matches the bare apex `box.com`). Empty
  /// leaves `request` unrestricted (back-compat) unless the operator set
  /// a `[extensions.policy]` net ceiling — then the tool gets exactly
  /// the ceiling. A non-empty list flips it to default-deny — any other
  /// host throws before the call is made.
  #[serde(default)]
  pub net: Vec<String>,
}

impl ToolManifest {
  /// Returns `true` when the extension should be surfaced in `tools/list`.
  #[must_use]
  pub fn is_tool(&self) -> bool {
    self.expose_as_mcp_tool
  }
}
