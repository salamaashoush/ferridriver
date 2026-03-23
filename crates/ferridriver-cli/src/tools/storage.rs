use crate::params::*;
use crate::server::{sess, McpServer};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = storage_router, vis = "pub")]
impl McpServer {
    #[tool(name = "localstorage_get", description = "Get a localStorage value.")]
    async fn localstorage_get(&self, Parameters(p): Parameters<LocalStorageKeyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let r = page.evaluate(&format!("localStorage.getItem('{}')", p.key.replace('\'', "\\'")))
            .await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(r.map(|v| v.to_string()).unwrap_or("null".into()))]))
    }

    #[tool(name = "localstorage_set", description = "Set a localStorage value.")]
    async fn localstorage_set(&self, Parameters(p): Parameters<LocalStorageSetParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        page.evaluate(&format!("localStorage.setItem('{}', '{}')", p.key.replace('\'', "\\'"), p.value.replace('\'', "\\'")))
            .await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Set '{}'='{}'.", p.key, p.value))]))
    }

    #[tool(name = "localstorage_list", description = "List localStorage entries.")]
    async fn localstorage_list(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let r = page.evaluate("JSON.stringify(Object.fromEntries(Object.entries(localStorage)))")
            .await.map_err(|e| Self::err(e))?;
        let val = r.as_ref().and_then(|v| v.as_str()).unwrap_or("{}");
        let parsed: serde_json::Value = serde_json::from_str(val).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&parsed).unwrap())]))
    }

    #[tool(name = "localstorage_clear", description = "Clear localStorage.")]
    async fn localstorage_clear(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        page.evaluate("localStorage.clear()").await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text("localStorage cleared.")]))
    }
}
