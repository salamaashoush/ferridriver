use crate::params::*;
use crate::server::{sess, ChromeyMcp};
use rmcp::{handler::server::wrapper::Parameters, model::*, tool, tool_router, ErrorData};

#[tool_router(router = navigation_router, vis = "pub")]
impl ChromeyMcp {
    #[tool(name = "navigate", description = "Navigate to a URL.")]
    async fn navigate(&self, Parameters(p): Parameters<NavigateParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.goto(&p.url).await.map_err(|e| Self::err(format!("Navigation failed: {e}")))?;
        if p.wait_until.as_deref() != Some("none") {
            let _ = page.wait_for_navigation().await;
        }
        self.action_ok(&page, s, "Navigation complete.").await
    }

    #[tool(name = "go_back", description = "Go back in history.")]
    async fn go_back(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.evaluate("window.history.back()").await.map_err(|e| Self::err(format!("{e}")))?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.action_ok(&page, s, "Navigated back.").await
    }

    #[tool(name = "go_forward", description = "Go forward in history.")]
    async fn go_forward(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.evaluate("window.history.forward()").await.map_err(|e| Self::err(format!("{e}")))?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.action_ok(&page, s, "Navigated forward.").await
    }

    #[tool(name = "reload", description = "Reload the page.")]
    async fn reload(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let page = self.page(s).await?;
        page.reload().await.map_err(|e| Self::err(format!("{e}")))?;
        let _ = page.wait_for_navigation().await;
        self.action_ok(&page, s, "Page reloaded.").await
    }

    #[tool(name = "new_page", description = "Open a new page in a session.")]
    async fn new_page(&self, Parameters(p): Parameters<NewPageParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let url = p.url.as_deref().unwrap_or("about:blank");
        let mut state = self.state.lock().await;
        let idx = state.open_page(s, url).await.map_err(|e| Self::err(e))?;
        let page = state.active_page(s).map_err(|e| Self::err(e))?.clone();
        drop(state);
        let snap = self.snap(&page, s).await;
        Ok(CallToolResult::success(vec![Content::text(format!("Opened page {idx} in session '{s}'.\n\n{snap}"))]))
    }

    #[tool(name = "close_page", description = "Close a page by index.")]
    async fn close_page(&self, Parameters(p): Parameters<ClosePageParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let mut state = self.state.lock().await;
        state.close_page(s, p.page_index).map_err(|e| Self::err(e))?;
        Ok(CallToolResult::success(vec![Content::text(format!("Closed page {} in session '{s}'.", p.page_index))]))
    }

    #[tool(name = "select_page", description = "Select a page by index.")]
    async fn select_page(&self, Parameters(p): Parameters<SelectPageParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(&p.session);
        let mut state = self.state.lock().await;
        state.select_page(s, p.page_index).map_err(|e| Self::err(e))?;
        let page = state.active_page(s).map_err(|e| Self::err(e))?.clone();
        drop(state);
        self.action_ok(&page, s, &format!("Switched to page {}.", p.page_index)).await
    }

    #[tool(name = "list_sessions", description = "List all sessions and pages.")]
    async fn list_sessions(&self) -> Result<CallToolResult, ErrorData> {
        let state = self.state.lock().await;
        let sessions = state.list_sessions().await;
        drop(state);
        let mut out = String::from("### Sessions\n");
        for s in &sessions {
            out.push_str(&format!("**{}**\n", s.name));
            for p in &s.pages {
                let marker = if p.active { " (active)" } else { "" };
                out.push_str(&format!("  Page {}{}: {} - {}\n", p.index, marker, p.url, p.title));
            }
        }
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }
}
