use crate::params::{ConnectParams, NavigateParams, PageParams};
use crate::server::{sess, McpServer};
use rmcp::{handler::server::wrapper::Parameters, model::{CallToolResult, Content}, tool, tool_router, ErrorData};
use std::fmt::Write;

#[tool_router(router = navigation_router, vis = "pub")]
impl McpServer {
    #[tool(name = "connect", description = "Connect to a running Chrome browser. Provide a WebSocket/HTTP URL, or use auto_discover to find a running instance by reading DevToolsActivePort.")]
    async fn connect(&self, Parameters(p): Parameters<ConnectParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let mut state = self.state.lock().await;

        if let Some(url) = &p.url {
            let page_count = Box::pin(state.connect_to_url(s, url)).await.map_err(Self::err)?;
            drop(state);
            let page = Box::pin(self.page(s)).await?;
            let snap = self.snap(&page, s).await;
            Ok(CallToolResult::success(vec![Content::text(
                format!("Connected to browser at {url}. Found {page_count} existing page(s) in session '{s}'.\n\n{snap}")
            )]))
        } else if p.auto_discover.unwrap_or(false) {
            let channel = p.channel.as_deref().unwrap_or("stable");
            let page_count = Box::pin(state.connect_auto(s, channel, p.user_data_dir.as_deref()))
                .await.map_err(Self::err)?;
            drop(state);
            let page = Box::pin(self.page(s)).await?;
            let snap = self.snap(&page, s).await;
            Ok(CallToolResult::success(vec![Content::text(
                format!("Auto-connected to {channel} Chrome. Found {page_count} existing page(s) in session '{s}'.\n\n{snap}")
            )]))
        } else {
            Err(Self::err("Provide 'url' (WebSocket/HTTP debugger URL) or set 'auto_discover: true' to find a running Chrome."))
        }
    }

    #[tool(name = "navigate", description = "Navigate to a URL.")]
    async fn navigate(&self, Parameters(p): Parameters<NavigateParams>) -> Result<CallToolResult, ErrorData> {
        let s = sess(p.session.as_ref());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.goto(&p.url, None).await.map_err(Self::err)?;
        self.action_ok(&page, s, "Navigation complete.").await
    }

    #[tool(name = "page", description = "Manage pages and sessions. Actions: back, forward, reload, new (open page), close (by index), select (by index), list (all sessions/pages), close_browser.")]
    async fn page_manage(&self, Parameters(p): Parameters<PageParams>) -> Result<CallToolResult, ErrorData> {
        match p.action.as_str() {
            "back" => {
                let s = sess(p.session.as_ref());
                let _guard = self.session_guard(s).await;
                let page = Box::pin(self.page(s)).await?;
                page.go_back(None).await.map_err(Self::err)?;
                self.action_ok(&page, s, "Navigated back.").await
            }
            "forward" => {
                let s = sess(p.session.as_ref());
                let _guard = self.session_guard(s).await;
                let page = Box::pin(self.page(s)).await?;
                page.go_forward(None).await.map_err(Self::err)?;
                self.action_ok(&page, s, "Navigated forward.").await
            }
            "reload" => {
                let s = sess(p.session.as_ref());
                let _guard = self.session_guard(s).await;
                let page = Box::pin(self.page(s)).await?;
                page.reload(None).await.map_err(Self::err)?;
                self.action_ok(&page, s, "Page reloaded.").await
            }
            "new" => {
                let s = sess(p.session.as_ref());
                let _guard = self.session_guard(s).await;
                let url = p.url.as_deref().unwrap_or("about:blank");
                let mut state = self.state.lock().await;
                let idx = Box::pin(state.open_page(s, url)).await.map_err(Self::err)?;
                let any_page = state.active_page(s).map_err(Self::err)?.clone();
                drop(state);
                let page = ferridriver::Page::new(any_page);
                let snap = self.snap(&page, s).await;
                Ok(CallToolResult::success(vec![Content::text(format!("Opened page {idx} in session '{s}'.\n\n{snap}"))]))
            }
            "close" => {
                let s = sess(p.session.as_ref());
                let _guard = self.session_guard(s).await;
                let idx = p.page_index.ok_or_else(|| Self::err("'page_index' required for close"))?;
                let mut state = self.state.lock().await;
                state.close_page(s, idx).map_err(Self::err)?;
                Ok(CallToolResult::success(vec![Content::text(format!("Closed page {idx} in session '{s}'."))]))
            }
            "select" => {
                let s = sess(p.session.as_ref());
                let _guard = self.session_guard(s).await;
                let idx = p.page_index.ok_or_else(|| Self::err("'page_index' required for select"))?;
                let mut state = self.state.lock().await;
                state.select_page(s, idx).map_err(Self::err)?;
                let any_page = state.active_page(s).map_err(Self::err)?.clone();
                drop(state);
                let page = ferridriver::Page::new(any_page);
                self.action_ok(&page, s, &format!("Switched to page {idx}.")).await
            }
            "list" => {
                let state = self.state.lock().await;
                let contexts = state.list_contexts().await;
                drop(state);
                let mut out = String::from("### Sessions\n");
                for c in &contexts {
                    let _ = writeln!(out, "**{}**", c.name);
                    for pg in &c.pages {
                        let marker = if pg.active { " (active)" } else { "" };
                        let _ = writeln!(out, "  Page {}{}: {} - {}", pg.index, marker, pg.url, pg.title);
                    }
                }
                Ok(CallToolResult::success(vec![Content::text(out)]))
            }
            "close_browser" => {
                self.state.lock().await.shutdown().await;
                Ok(CallToolResult::success(vec![Content::text("Browser closed.")]))
            }
            other => Err(Self::err(format!("Unknown action '{other}'. Use: back, forward, reload, new, close, select, list, close_browser."))),
        }
    }
}
