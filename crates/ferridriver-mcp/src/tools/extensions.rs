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
    net host patterns) — plus every file/spec that FAILED to load with its error, and every \
    operator-policy conflict warning (a declared capability outside the [extensions.policy] \
    ceiling). Use to discover available tools, audit what authority each one was granted, and \
    debug an extension that did not come up."
  )]
  async fn ferridriver_extensions(
    &self,
    Parameters(p): Parameters<ExtensionsParams>,
  ) -> Result<CallToolResult, ErrorData> {
    let include_schema = p.include_schema.unwrap_or(false);

    let files: Vec<serde_json::Value> = self
      .extensions
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

    let errors: Vec<serde_json::Value> = self
      .extensions
      .errors()
      .iter()
      .map(|(source, message)| serde_json::json!({ "source": source, "error": message }))
      .collect();

    let warnings: Vec<serde_json::Value> = self
      .extensions
      .warnings()
      .iter()
      .map(|(source, message)| serde_json::json!({ "source": source, "warning": message }))
      .collect();

    let payload = serde_json::json!({
      "count": self.extensions.tool_count(),
      "files": files,
      "errors": errors,
      "warnings": warnings,
    });
    let json =
      serde_json::to_string_pretty(&payload).map_err(|e| McpServer::err(format!("serialize extensions: {e}")))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn server_with_registry() -> McpServer {
    let manifest: crate::extension::ToolManifest = serde_json::from_value(serde_json::json!({
      "name": "box.login",
      "title": "Box Login",
      "description": "Logs in",
      "exposeAsMcpTool": true,
      "timeoutMs": 5000,
      "inputSchema": { "type": "object", "properties": { "user": { "type": "string" } } },
      "outputSchema": { "type": "object", "properties": { "cookie": { "type": "string" } } },
      "annotations": { "readOnlyHint": false, "destructiveHint": false },
      "allow": { "net": ["*.box.com"], "commands": { "curlish": "echo hi" } }
    }))
    .expect("manifest");
    let files = vec![crate::extension::LoadedExtension {
      tools: vec![manifest],
      bytecode: std::sync::Arc::from(Vec::new().into_boxed_slice()),
      path: std::path::PathBuf::from("box-login.ts"),
    }];
    let errors = vec![("broken.js".to_string(), "bundle: syntax error".to_string())];
    let warnings = vec![("box-login.ts".to_string(), "tool `box.login`: shell-form".to_string())];
    let mut server = McpServer::with_options(
      ferridriver::state::ConnectMode::Launch,
      ferridriver::backend::BackendKind::CdpPipe,
      true,
      std::sync::Arc::new(ferridriver_config::mcp::McpConfig::default()),
    );
    server.extensions = crate::extension::ExtensionRegistry::with_warnings(files, errors, warnings);
    server
  }

  async fn payload(server: &McpServer, include_schema: Option<bool>) -> serde_json::Value {
    let result = server
      .ferridriver_extensions(Parameters(ExtensionsParams { include_schema }))
      .await
      .expect("tool result");
    let as_json = serde_json::to_value(&result).expect("serialize result");
    let text = as_json["content"][0]["text"].as_str().expect("text content");
    serde_json::from_str(text).expect("payload JSON")
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn introspection_reports_tools_errors_and_warnings() {
    let server = server_with_registry();
    let p = payload(&server, None).await;

    assert_eq!(p["count"], 1);
    let tool = &p["files"][0]["tools"][0];
    assert_eq!(tool["name"], "box.login");
    assert_eq!(tool["title"], "Box Login");
    assert_eq!(tool["exposeAsMcpTool"], true);
    assert_eq!(tool["timeoutMs"], 5000);
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    assert_eq!(tool["allow"]["net"][0], "*.box.com");
    assert_eq!(tool["allow"]["commands"][0], "curlish");
    // Schemas stay out of the default payload (they can be large).
    assert!(tool.get("inputSchema").is_none());
    assert!(tool.get("outputSchema").is_none());

    assert_eq!(p["errors"][0]["source"], "broken.js");
    assert!(p["errors"][0]["error"].as_str().unwrap().contains("syntax error"));
    assert_eq!(p["warnings"][0]["source"], "box-login.ts");
    assert!(p["warnings"][0]["warning"].as_str().unwrap().contains("shell-form"));
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn include_schema_adds_both_schemas() {
    let server = server_with_registry();
    let p = payload(&server, Some(true)).await;
    let tool = &p["files"][0]["tools"][0];
    assert_eq!(tool["inputSchema"]["properties"]["user"]["type"], "string");
    assert_eq!(tool["outputSchema"]["properties"]["cookie"]["type"], "string");
  }
}
