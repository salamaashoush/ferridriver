//! Introspection tool: list the extensions loaded at startup.
//!
//! Before this, the only signal that an extension loaded (or failed to) was
//! a `tracing` line at boot. This surfaces the live registry to the
//! client so an agent can discover available tools and their
//! declared capabilities without restarting the server to read logs.

use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};
use serde::Deserialize;

use crate::server::McpServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExtensionsParams {
  #[schemars(description = "Include each tool's full JSON inputSchema in the output. \
    Default false (schemas can be large; names + capabilities are usually enough).")]
  pub include_schema: Option<bool>,
}

#[tool_router(router = extensions_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "ferridriver_extensions",
    description = "List the extensions loaded at server startup: for each source file, \
    the tools it declares with their description, whether they are exposed as first-class MCP \
    tools, the per-tool timeout, and the declared capability allow-lists (exec command names, \
    net host patterns) — plus every file/spec that FAILED to load, with its error. Use to \
    discover available tools, audit what authority each one was granted, and debug an \
    extension that did not come up."
  )]
  async fn ferridriver_extensions(
    &self,
    Parameters(p): Parameters<ExtensionsParams>,
  ) -> Result<CallToolResult, ErrorData> {
    let include_schema = p.include_schema.unwrap_or(false);

    let files: Vec<serde_json::Value> = self
      .plugins
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
            if include_schema && let Some(schema) = &t.input_schema {
              obj["inputSchema"] = schema.clone();
            }
            obj
          })
          .collect();
        serde_json::json!({ "path": f.path.display().to_string(), "tools": tools })
      })
      .collect();

    let errors: Vec<serde_json::Value> = self
      .plugins
      .errors()
      .iter()
      .map(|(source, message)| serde_json::json!({ "source": source, "error": message }))
      .collect();

    let payload = serde_json::json!({
      "count": self.plugins.tool_count(),
      "files": files,
      "errors": errors,
    });
    let json =
      serde_json::to_string_pretty(&payload).map_err(|e| McpServer::err(format!("serialize extensions: {e}")))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
  }
}
