use crate::params::*;
use crate::server::{sess, ChromeyMcp};
use chromiumoxide::cdp::browser_protocol::network::OverrideNetworkStateParams;
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = network_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "set_network_state", description = "Set network conditions.")]
    async fn set_network_state(&self, Parameters(p): Parameters<SetNetworkStateParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let params = OverrideNetworkStateParams::new(
            p.state == "offline", p.latency.unwrap_or(0.0),
            p.download_throughput.unwrap_or(-1.0), p.upload_throughput.unwrap_or(-1.0),
        );
        page.execute(params).await.map_err(|e| Self::err(format!("{e}")))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Network: {}.", p.state))]))
    }

    #[tool(name = "console_messages", description = "Get console log/warn/error messages.")]
    async fn console_messages(&self, Parameters(p): Parameters<ConsoleMessagesParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let state = self.state.lock().await;
        let msgs = state.console_messages(s, p.level.as_deref(), p.limit.unwrap_or(50)).await.map_err(|e| Self::err(e))?;
        drop(state);
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&msgs).unwrap())]))
    }

    #[tool(name = "network_requests", description = "List network requests since page load.")]
    async fn network_requests(&self, Parameters(p): Parameters<NetworkRequestsParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let state = self.state.lock().await;
        let reqs = state.network_requests(s, p.limit.unwrap_or(50)).await.map_err(|e| Self::err(e))?;
        drop(state);
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&reqs).unwrap())]))
    }

    #[tool(name = "trace_start", description = "Start performance tracing.")]
    async fn trace_start(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let params = chromiumoxide::cdp::browser_protocol::tracing::StartParams::builder().build();
        page.execute(params).await.map_err(|e| Self::err(format!("Start trace: {e}")))?;
        Ok(CallToolResult::success(vec![Content::text("Trace started.")]))
    }

    #[tool(name = "trace_stop", description = "Stop tracing and return collected data.")]
    async fn trace_stop(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.execute(chromiumoxide::cdp::browser_protocol::tracing::EndParams {}).await.map_err(|e| Self::err(format!("Stop trace: {e}")))?;
        let metrics = page.metrics().await.map_err(|e| Self::err(format!("{e}")))?;
        let mut out = String::from("Trace stopped.\n\n### Performance Metrics\n");
        for m in &metrics { if m.value > 0.0 { out.push_str(&format!("- {}: {:.2}\n", m.name, m.value)); } }
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }
}
