use ferridriver::backend::CookieData;
use crate::params::*;
use crate::server::{sess, McpServer};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = cookies_router, vis = "pub")]
impl McpServer {
    #[tool(name = "get_cookies", description = "Get all cookies.")]
    async fn get_cookies(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let cookies = page.cookies().await.map_err(|e| Self::err(e))?;
        let list: Vec<serde_json::Value> = cookies.iter().map(|c| {
            serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain, "path": c.path, "secure": c.secure, "httpOnly": c.http_only})
        }).collect();
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&list).unwrap())]))
    }

    #[tool(name = "set_cookie", description = "Set a cookie.")]
    async fn set_cookie(&self, Parameters(p): Parameters<SetCookieParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        let cookie = CookieData {
            name: p.name.clone(),
            value: p.value.clone(),
            domain: p.domain.clone().unwrap_or_default(),
            path: p.path.clone().unwrap_or_default(),
            secure: p.secure.unwrap_or(false),
            http_only: p.http_only.unwrap_or(false),
            expires: p.expires,
        };
        page.set_cookie(cookie).await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Cookie '{}' set in session '{s}'.", p.name))]))
    }

    #[tool(name = "delete_cookie", description = "Delete a cookie.")]
    async fn delete_cookie(&self, Parameters(p): Parameters<DeleteCookieParams_>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        page.delete_cookie(&p.name, p.domain.as_deref()).await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Cookie '{}' deleted.", p.name))]))
    }

    #[tool(name = "clear_cookies", description = "Clear all cookies.")]
    async fn clear_cookies(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let _guard = self.session_guard(s).await;
        let page = self.page(s).await?;
        page.clear_cookies().await.map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text("Cookies cleared.")]))
    }
}
