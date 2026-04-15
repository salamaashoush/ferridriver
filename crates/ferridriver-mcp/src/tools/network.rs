use crate::params::DiagnosticsParams;
use crate::server::{McpServer, sess};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};
use std::fmt::Write;

#[tool_router(router = network_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "diagnostics",
    description = "Page diagnostics. Types: console (log/warn/error messages), network (HTTP requests since load), trace_start (begin perf tracing), trace_stop (end tracing + metrics)."
  )]
  async fn diagnostics(&self, Parameters(p): Parameters<DiagnosticsParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    match p.r#type.as_str() {
      "console" => {
        let _guard = self.session_guard(s).await;
        let handles = self
          .state
          .log_handles_for(s)
          .await
          .ok_or_else(|| Self::err(format!("Context '{s}' not found")))?;
        let limit = p.limit.unwrap_or(50);
        let level = p.level.as_deref();
        let log = handles.console.read().await;
        let msgs: Vec<_> = log
          .iter()
          .filter(|m| level.is_none_or(|l| l == "all" || m.level == l))
          .rev()
          .take(limit)
          .cloned()
          .collect::<Vec<_>>()
          .into_iter()
          .rev()
          .collect();
        drop(log);
        Ok(CallToolResult::success(vec![Content::text(
          serde_json::to_string_pretty(&msgs).unwrap_or_default(),
        )]))
      },
      "network" => {
        let _guard = self.session_guard(s).await;
        let handles = self
          .state
          .log_handles_for(s)
          .await
          .ok_or_else(|| Self::err(format!("Context '{s}' not found")))?;
        let limit = p.limit.unwrap_or(50);
        let log = handles.network.read().await;
        let reqs: Vec<_> = log
          .iter()
          .rev()
          .take(limit)
          .cloned()
          .collect::<Vec<_>>()
          .into_iter()
          .rev()
          .collect();
        drop(log);
        Ok(CallToolResult::success(vec![Content::text(
          serde_json::to_string_pretty(&reqs).unwrap_or_default(),
        )]))
      },
      "trace_start" => {
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.start_tracing().await.map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text("Trace started.")]))
      },
      "trace_stop" => {
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.stop_tracing().await.map_err(Self::err)?;
        let metrics = page.metrics().await.map_err(Self::err)?;
        let mut out = String::from("Trace stopped.\n\n### Performance Metrics\n");
        for m in &metrics {
          if m.value > 0.0 {
            let _ = writeln!(out, "- {}: {:.2}", m.name, m.value);
          }
        }
        Ok(CallToolResult::success(vec![Content::text(out)]))
      },
      other => Err(Self::err(format!(
        "Unknown type '{other}'. Use: console, network, trace_start, trace_stop."
      ))),
    }
  }
}
