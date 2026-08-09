use crate::params::{ConnectParams, NavigateParams, PageParams};
use crate::server::{McpServer, sess};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, ContentBlock},
  tool, tool_router,
};
use std::fmt::Write;

#[tool_router(router = navigation_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "connect",
    title = "Connect to Browser",
    description = "Connect to a running Chrome browser. Provide a WebSocket/HTTP URL, or use auto_discover to find a running instance by reading DevToolsActivePort.",
    annotations(read_only_hint = false, idempotent_hint = true, open_world_hint = true)
  )]
  async fn connect(&self, Parameters(p): Parameters<ConnectParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    // Same serialization every other tool takes: without it a connect
    // racing a navigate on the same session has both sides cold-start
    // the context and open a page apiece.
    let _guard = self.session_guard(s).await;
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
        self.invalidate_context(s);
        count
      };
      let page = Box::pin(self.page(s)).await?;
      let snap = self.snap(&page, s).await;
      Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
        self.invalidate_context(s);
        count
      };
      let page = Box::pin(self.page(s)).await?;
      let snap = self.snap(&page, s).await;
      Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
    title = "Navigate",
    description = "Navigate the browser to a URL and wait for the page to load. Returns an accessibility snapshot of the loaded page. After navigation, all previous element refs are invalidated -- use the new snapshot's refs.",
    annotations(read_only_hint = false, open_world_hint = true)
  )]
  async fn navigate(
    &self,
    Parameters(p): Parameters<NavigateParams>,
    meta: rmcp::model::Meta,
    peer: rmcp::service::Peer<rmcp::RoleServer>,
  ) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let token = meta.get_progress_token();
    let _guard = self.session_guard(s).await;
    McpServer::emit_progress(
      &peer,
      token.as_ref(),
      0.0,
      Some(2.0),
      &format!("navigating to {}", p.url),
    )
    .await;
    let page = Box::pin(self.page(s)).await?;
    let opts = ferridriver::options::GotoOptions {
      wait_until: Some(
        p.wait_until
          .as_deref()
          .map_or(ferridriver::options::LoadState::Commit, Into::into),
      ),
      timeout: None,
      referer: None,
    };
    page.goto(&p.url).options(opts).await.map_err(Self::err)?;
    McpServer::emit_progress(&peer, token.as_ref(), 1.0, Some(2.0), "loaded; capturing snapshot").await;
    let out = Box::pin(self.action_ok(&page, s, "Navigation complete.")).await;
    McpServer::emit_progress(&peer, token.as_ref(), 2.0, Some(2.0), "done").await;
    out
  }

  #[tool(
    name = "page",
    title = "Manage Tabs",
    description = "Manage pages (tabs) and sessions. Actions: list (show all tabs with URLs), select (switch to tab by index -- invalidates old refs), new (open tab), close (close tab by index), back, forward, reload, close_context (close one session's context: its tabs, cookies and storage, leaving the browser up), close_instance (close one browser process and its contexts, leaving other instances alone -- also how you pick up changed per-instance chrome flags), close_browser (close every browser this server launched). Use 'list' to find tabs, then 'select' to switch.",
    annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false)
  )]
  async fn page_manage(&self, Parameters(p): Parameters<PageParams>) -> Result<CallToolResult, ErrorData> {
    match p.action.as_str() {
      "back" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.go_back().await.map_err(Self::err)?;
        Box::pin(self.action_ok(&page, s, "Navigated back.")).await
      },
      "forward" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.go_forward().await.map_err(Self::err)?;
        Box::pin(self.action_ok(&page, s, "Navigated forward.")).await
      },
      "reload" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.reload().await.map_err(Self::err)?;
        Box::pin(self.action_ok(&page, s, "Page reloaded.")).await
      },
      "new" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let url = p.url.as_deref().unwrap_or("about:blank");
        let ctx_ref = ferridriver::context::ContextRef::new(self.state.state_arc(), s.to_string());
        let page = Box::pin(ctx_ref.new_page()).await.map_err(Self::err)?;
        if url != "about:blank" {
          page.goto(url).await.map_err(Self::err)?;
        }
        self.invalidate_context(s);
        let snap = self.snap(&page, s).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
        Box::pin(state.close_page(s, idx)).await.map_err(Self::err)?;
        drop(state);
        self.invalidate_context(s);
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
        drop(state);
        // Route through `page()` so the switch lands on the context's
        // cached wrapper: minting a bare `Page` here dropped the
        // wrapper-level state (default timeouts, emulateMedia merge)
        // and left the cache pointing at the previous tab.
        self.invalidate_context(s);
        let page = Box::pin(self.page(s)).await?;
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
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
      },
      "close_context" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let mut state = self.state.write().await;
        Box::pin(state.remove_context(s)).await;
        drop(state);
        self.release_context(s);
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
          "Closed context '{s}'. Its pages, cookies and storage are gone; the browser stays up."
        ))]))
      },
      "close_instance" => {
        let s = sess(p.session.as_opt());
        let _guard = self.session_guard(s).await;
        let instance = ferridriver::state::SessionKey::parse(s).instance;
        let closed = self.state.write().await.close_instance(&instance).await;
        self.invalidate_all_caches();
        // Session names are free-form, so match the way they are routed:
        // everything before ':' (or the whole name for the default
        // instance) selects the browser that just went away.
        self
          .sessions
          .remove_matching(|name| *ferridriver::state::SessionKey::parse(name).instance == *instance);
        let msg = if closed {
          format!(
            "Closed browser instance '{instance}'. Other instances keep running; \
             the next call on '{instance}' launches a fresh browser with current flags."
          )
        } else {
          format!("No live browser instance '{instance}'.")
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
      },
      "close_browser" => {
        self.shutdown_browsers().await;
        Ok(CallToolResult::success(vec![ContentBlock::text("Browser closed.")]))
      },
      other => Err(Self::err(format!(
        "Unknown action '{other}'. Use: back, forward, reload, new, close, select, list, \
         close_context, close_instance, close_browser."
      ))),
    }
  }
}
