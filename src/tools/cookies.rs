use crate::params::*;
use crate::server::{sess, ChromeyMcp};
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, DeleteCookiesParams};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = cookies_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "get_cookies", description = "Get all cookies.")]
    async fn get_cookies(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let cookies = page.get_cookies().await.map_err(|e| Self::err(format!("{e}")))?;
        let list: Vec<serde_json::Value> = cookies.iter().map(|c| {
            serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain, "path": c.path, "secure": c.secure, "httpOnly": c.http_only})
        }).collect();
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&list).unwrap())]))
    }

    #[tool(name = "set_cookie", description = "Set a cookie.")]
    async fn set_cookie(&self, Parameters(p): Parameters<SetCookieParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let mut cookie = CookieParam::new(p.name.clone(), p.value);
        cookie.domain = p.domain;
        cookie.path = p.path;
        cookie.secure = p.secure;
        cookie.http_only = p.http_only;
        if let Some(e) = p.expires {
            cookie.expires = Some(chromiumoxide::cdp::browser_protocol::network::TimeSinceEpoch::new(e));
        }
        page.set_cookie(cookie).await.map_err(|e| Self::err(format!("{e}")))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Cookie '{}' set in session '{s}'.", p.name))]))
    }

    #[tool(name = "delete_cookie", description = "Delete a cookie.")]
    async fn delete_cookie(&self, Parameters(p): Parameters<DeleteCookieParams_>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        let mut params = DeleteCookiesParams::new(p.name.clone());
        params.domain = p.domain;
        page.delete_cookie(params).await.map_err(|e| Self::err(format!("{e}")))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Cookie '{}' deleted.", p.name))]))
    }

    #[tool(name = "clear_cookies", description = "Clear all cookies.")]
    async fn clear_cookies(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.clear_cookies().await.map_err(|e| Self::err(format!("{e}")))?;
        Ok(CallToolResult::success(vec![Content::text("Cookies cleared.")]))
    }
}
