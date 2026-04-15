use crate::params::{ConnectParams, NavigateParams, PageParams};
use crate::server::{McpServer, sess};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};
use std::fmt::Write;

#[tool_router(router = navigation_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "connect",
    description = "Connect to a running Chrome browser. Provide a WebSocket/HTTP URL, or use auto_discover to find a running instance by reading DevToolsActivePort."
  )]
  async fn connect(&self, Parameters(p): Parameters<ConnectParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    // Parse the composite session key to get the instance name.
    // "staging:admin" -> instance="staging", context="admin"
    // The connect operation targets the browser instance, not the context.
    let key = ferridriver::state::SessionKey::parse(s);
    let instance = &*key.instance;

    if let Some(url) = &p.url {
      let page_count = {
        let mut state = self.state.write().await;
        let count = Box::pin(state.connect_to_url(instance, url)).await.map_err(Self::err)?;
        drop(state);
        self.state.invalidate_context(s);
        count
      };
      let page = Box::pin(self.page(s)).await?;
      let snap = self.snap(&page, s).await;
      Ok(CallToolResult::success(vec![Content::text(format!(
        "Connected to browser at {url}. Found {page_count} existing page(s) in session '{s}'.\n\n{snap}"
      ))]))
    } else if p.auto_discover.unwrap_or(false) {
      let channel = p.channel.as_deref().unwrap_or("stable");
      let page_count = {
        let mut state = self.state.write().await;
        let count = Box::pin(state.connect_auto(instance, channel, p.user_data_dir.as_deref()))
          .await
          .map_err(Self::err)?;
        drop(state);
        self.state.invalidate_context(s);
        count
      };
      let page = Box::pin(self.page(s)).await?;
      let snap = self.snap(&page, s).await;
      Ok(CallToolResult::success(vec![Content::text(format!(
        "Auto-connected to {channel} Chrome. Found {page_count} existing page(s) in session '{s}'.\n\n{snap}"
      ))]))
    } else {
      Err(Self::err(
        "Provide 'url' (WebSocket/HTTP debugger URL) or set 'auto_discover: true' to find a running Chrome.",
      ))
    }
  }

  #[tool(
    name = "navigate",
    description = "Navigate the browser to a URL and wait for the page to load. Returns an accessibility snapshot of the loaded page. After navigation, all previous element refs are invalidated -- use the new snapshot's refs."
  )]
  async fn navigate(&self, Parameters(p): Parameters<NavigateParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let opts = ferridriver::options::GotoOptions {
      wait_until: Some(p.wait_until.unwrap_or_else(|| "commit".into())),
      timeout: None,
    };
    page.goto(&p.url, Some(opts)).await.map_err(Self::err)?;
    Box::pin(self.action_ok(&page, s, "Navigation complete.")).await
  }

  #[tool(
    name = "page",
    description = "Manage pages (tabs) and sessions. Actions: list (show all tabs with URLs), select (switch to tab by index -- invalidates old refs), new (open tab), close (close tab by index), back, forward, reload, close_browser. Use 'list' to find tabs, then 'select' to switch."
  )]
  async fn page_manage(&self, Parameters(p): Parameters<PageParams>) -> Result<CallToolResult, ErrorData> {
    match p.action.as_str() {
      "back" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.go_back(None).await.map_err(Self::err)?;
        Box::pin(self.action_ok(&page, s, "Navigated back.")).await
      },
      "forward" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.go_forward(None).await.map_err(Self::err)?;
        Box::pin(self.action_ok(&page, s, "Navigated forward.")).await
      },
      "reload" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.reload(None).await.map_err(Self::err)?;
        Box::pin(self.action_ok(&page, s, "Page reloaded.")).await
      },
      "new" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let url = p.url.as_deref().unwrap_or("about:blank");
        let mut state = self.state.write().await;
        let any_page = Box::pin(state.open_page(s, url)).await.map_err(Self::err)?;
        drop(state);
        self.state.invalidate_context(s);
        let page = ferridriver::Page::new(any_page);
        let snap = self.snap(&page, s).await;
        Ok(CallToolResult::success(vec![Content::text(format!(
          "Opened new page in session '{s}'.\n\n{snap}"
        ))]))
      },
      "close" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let idx = p
          .page_index
          .ok_or_else(|| Self::err("'page_index' required for close"))?;
        let mut state = self.state.write().await;
        state.close_page(s, idx).map_err(Self::err)?;
        drop(state);
        self.state.invalidate_context(s);
        Ok(CallToolResult::success(vec![Content::text(format!(
          "Closed page {idx} in session '{s}'."
        ))]))
      },
      "select" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let idx = p
          .page_index
          .ok_or_else(|| Self::err("'page_index' required for select"))?;
        let mut state = self.state.write().await;
        state.select_page(s, idx).map_err(Self::err)?;
        let any_page = state.active_page(s).map_err(Self::err)?.clone();
        drop(state);
        let page = ferridriver::Page::new(any_page);
        Box::pin(self.action_ok(&page, s, &format!("Switched to page {idx}."))).await
      },
      "list" => {
        let state = self.state.read().await;
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
      },
      "close_browser" => {
        self.state.write().await.shutdown().await;
        self.state.invalidate_all();
        Ok(CallToolResult::success(vec![Content::text("Browser closed.")]))
      },
      other => Err(Self::err(format!(
        "Unknown action '{other}'. Use: back, forward, reload, new, close, select, list, close_browser."
      ))),
    }
  }
}
