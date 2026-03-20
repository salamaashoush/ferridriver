use crate::server::ChromeyMcp;
use rmcp::{model::*, tool, tool_router, ErrorData};

#[tool_router(router = lifecycle_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "close_browser", description = "Close the browser.")]
    async fn close_browser(&self) -> Result<CallToolResult, ErrorData> {
        self.state.lock().await.shutdown().await;
        Ok(CallToolResult::success(vec![Content::text("Browser closed.")]))
    }
}
