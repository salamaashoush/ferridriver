//! Introspection + reload for the loaded extensions.
//!
//! `list` surfaces the live registry to the client so an agent can
//! discover available tools and their declared capabilities without
//! restarting the server to read logs.
//!
//! `reload` exists because the only way to pick up an edited extension
//! used to be restarting the MCP client, which tears down every browser
//! session with it — the authoring loop cost far more than the edit. A
//! reload re-resolves the configured specs, re-bundles, re-checks the
//! package requirements, replaces the promoted tool set (with a
//! `tools/list_changed` notification), and discards live session VMs so
//! open sessions pick the new code up on their next call while keeping
//! their `vars`, cookies and persistent processes.

use rmcp::{ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use serde::Deserialize;

use crate::server::McpServer;

#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionsAction {
  /// Report the live registry (default).
  #[default]
  List,
  /// Re-resolve, re-bundle and re-install every configured extension,
  /// then report the new registry.
  Reload,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExtensionsParams {
  #[schemars(
    description = "`list` (default) reports the loaded extensions; `reload` re-reads every \
    configured extension from disk first, then reports the result. Use `reload` after editing an \
    extension: it replaces the tools without restarting the server or losing browser sessions."
  )]
  pub action: Option<ExtensionsAction>,

  #[schemars(description = "Include each tool's full JSON inputSchema in the output. \
    Default false (schemas can be large; names + capabilities are usually enough).")]
  pub include_schema: Option<bool>,
}

#[tool_router(router = extensions_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "ferridriver_extensions",
    title = "List or Reload Extensions",
    description = "List the loaded extensions, or reload them from disk. \
    For each source file: the tools it declares with their description, whether they are exposed as \
    first-class MCP tools, the per-tool timeout, and the declared capability allow-lists (exec command \
    names, net host patterns) — plus every file/spec that FAILED to load with its error (including an \
    extension package whose declared `ferridriver.requires` are unmet), and every operator-policy \
    conflict warning. Pass `action: \"reload\"` to re-read every configured extension after editing \
    one: tools are replaced in place (a tools/list_changed notification follows) and live browser \
    sessions keep their state, dropping only their script VM so the next call runs the new code. \
    Use to discover available tools, audit what authority each one was granted, debug an extension \
    that did not come up, and iterate on an extension without a restart.",
    annotations(read_only_hint = false, idempotent_hint = true, open_world_hint = false)
  )]
  async fn ferridriver_extensions(
    &self,
    Parameters(p): Parameters<ExtensionsParams>,
    peer: rmcp::service::Peer<rmcp::RoleServer>,
  ) -> Result<CallToolResult, ErrorData> {
    let include_schema = p.include_schema.unwrap_or(false);
    let action = p.action.unwrap_or_default();

    let mut reloaded = None;
    if action == ExtensionsAction::Reload {
      let before: Vec<String> = self.promoted_tool_names();
      let (tools, dropped_vms) = self.reload_extensions().await;
      let after: Vec<String> = self.promoted_tool_names();
      // Only notify when the advertised set actually moved: a client that
      // re-lists on every notification should not be woken by a reload
      // that changed only a handler body.
      let mut notified = before != after;
      if notified && let Err(e) = peer.notify_tool_list_changed().await {
        // A dropped notification is precisely what leaves a client
        // calling a tool that no longer exists, so it is reported in the
        // result rather than swallowed.
        tracing::warn!(error = %e, "tools/list_changed notification failed");
        notified = false;
      }
      reloaded = Some(serde_json::json!({
        "tools": tools,
        "droppedSessionVms": dropped_vms,
        "toolListChanged": before != after,
        // False when the set moved but the notification could not be
        // delivered: the caller has to re-list rather than trust its copy.
        "toolListChangeNotified": notified,
        "added": after.iter().filter(|n| !before.contains(n)).collect::<Vec<_>>(),
        "removed": before.iter().filter(|n| !after.contains(n)).collect::<Vec<_>>(),
      }));
    }

    let json = self.render_extensions_report(include_schema, reloaded)?;
    Ok(self.ok_text(json))
  }
}

impl McpServer {
  /// Render the extension report the `ferridriver_extensions` tool
  /// returns. Separate from the tool body so the list path is testable
  /// without a live MCP peer.
  pub(crate) fn render_extensions_report(
    &self,
    include_schema: bool,
    reloaded: Option<serde_json::Value>,
  ) -> Result<String, ErrorData> {
    let loaded = self.extensions();
    let registry = &loaded.registry;
    let files: Vec<serde_json::Value> = registry
      .files()
      .iter()
      .map(|f| {
        let tools: Vec<serde_json::Value> = f
          .tools
          .iter()
          .map(|t| {
            let mut command_names: Vec<&String> = t.allow.commands.keys().collect();
            command_names.sort();
            let mut obj = serde_json::json!({
              "name": t.name,
              "description": t.description,
              "exposeAsMcpTool": t.expose_as_mcp_tool,
              "timeoutMs": t.timeout_ms,
              "allow": {
                "commands": command_names,
                "net": t.allow.net,
              },
            });
            if let Some(title) = &t.title {
              obj["title"] = serde_json::json!(title);
            }
            if let Some(annotations) = &t.annotations {
              obj["annotations"] = serde_json::to_value(annotations).unwrap_or(serde_json::Value::Null);
            }
            if include_schema {
              if let Some(schema) = &t.input_schema {
                obj["inputSchema"] = schema.clone();
              }
              if let Some(schema) = &t.output_schema {
                obj["outputSchema"] = schema.clone();
              }
            }
            obj
          })
          .collect();
        serde_json::json!({ "path": f.path.display().to_string(), "tools": tools })
      })
      .collect();

    let errors: Vec<serde_json::Value> = registry
      .errors()
      .iter()
      .map(|(source, message)| serde_json::json!({ "source": source, "error": message }))
      .collect();

    let warnings: Vec<serde_json::Value> = registry
      .warnings()
      .iter()
      .map(|(source, message)| serde_json::json!({ "source": source, "warning": message }))
      .collect();

    let mut payload = serde_json::json!({
      "count": registry.tool_count(),
      "files": files,
      "errors": errors,
      "warnings": warnings,
    });
    if let Some(reloaded) = reloaded {
      payload["reloaded"] = reloaded;
    }
    serde_json::to_string_pretty(&payload).map_err(|e| McpServer::err(format!("serialize extensions: {e}")))
  }
}
