use crate::params::{
  EvaluateParams, FindElementsParams, ScreenshotParams_, SearchPageParams, SessionOnlyParams, SnapshotParams,
  WaitForParams,
};
use crate::server::{McpServer, sess};
use base64::Engine;
use ferridriver::options::ScreenshotOptions;
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};

#[tool_router(router = content_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "snapshot",
    description = "Take an accessibility snapshot of the page. Supports depth limiting and incremental tracking."
  )]
  async fn snapshot(&self, Parameters(p): Parameters<SnapshotParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let opts = ferridriver::snapshot::SnapshotOptions {
      depth: p.depth,
      track: p.track,
    };
    match page.snapshot_for_ai(opts).await {
      Ok(result) => {
        if let Some(handle) = self.state.ref_map_handle(s).await {
          handle.store(std::sync::Arc::new(result.ref_map));
        } else {
          let state = self.state.read().await;
          state.set_ref_map(s, result.ref_map);
        }
        let mut text = result.full;
        if let Some(inc) = result.incremental {
          text.push_str("\n### Changes since last snapshot\n");
          text.push_str(&inc);
        }
        Ok(CallToolResult::success(vec![Content::text(text)]))
      },
      Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
        "[snapshot error: {e}]"
      ))])),
    }
  }

  #[tool(
    name = "screenshot",
    description = "Take a visual screenshot of the page as a base64-encoded image. Use 'selector' to capture a specific element, or 'full_page' for the entire scrollable page. Prefer snapshot for structured page content -- screenshot is for visual verification only."
  )]
  async fn screenshot(&self, Parameters(p): Parameters<ScreenshotParams_>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let mime = match p.format.as_deref() {
      Some("jpeg" | "jpg") => "image/jpeg",
      Some("webp") => "image/webp",
      _ => "image/png",
    };
    let bytes = if let Some(sel) = &p.selector {
      page.screenshot_element(sel).await.map_err(Self::err)?
    } else {
      let opts = ScreenshotOptions {
        format: p.format.clone(),
        quality: p.quality,
        full_page: p.full_page,
      };
      page.screenshot(opts).await.map_err(Self::err)?
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(CallToolResult::success(vec![Content::image(b64, mime)]))
  }

  #[tool(name = "evaluate", description = "Evaluate JavaScript on the page.")]
  async fn evaluate(&self, Parameters(p): Parameters<EvaluateParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let result = page.evaluate(p.expression.as_str()).await.map_err(Self::err)?;
    let val = result.map_or_else(
      || "undefined".to_string(),
      |v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
    );
    Ok(CallToolResult::success(vec![Content::text(val)]))
  }

  #[tool(
    name = "wait_for",
    description = "Wait until a CSS selector matches or body text contains a substring. Provide at least one of `selector` or `text`."
  )]
  async fn wait_for(&self, Parameters(p): Parameters<WaitForParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    if p.selector.is_none() && p.text.is_none() {
      return Err(Self::err("Provide `selector` and/or `text` to wait for."));
    }
    let timeout_ms = p.timeout.unwrap_or(30000);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(Self::err("Timeout waiting for condition"));
      }
      if let Some(sel) = &p.selector {
        if page.find_element(sel).await.is_ok() {
          return self.action_ok(&page, s, &format!("Found '{sel}'.")).await;
        }
      }
      if let Some(text) = &p.text {
        let needle = serde_json::to_string(text).map_err(|e| Self::err(format!("Invalid text for wait: {e}")))?;
        let js =
          format!("(() => {{ const body = document.body?.innerText ?? ''; return body.includes({needle}); }})()");
        if let Ok(r) = page.evaluate(&js).await {
          if r == Some(serde_json::Value::Bool(true)) {
            return self.action_ok(&page, s, &format!("Found text '{text}'.")).await;
          }
        }
      }
      tokio::time::sleep(std::time::Duration::from_millis(16)).await;
    }
  }

  #[tool(
    name = "search_page",
    description = "Search page text for a pattern (like grep). Zero cost, instant. Returns matches with surrounding context."
  )]
  async fn search_page(&self, Parameters(p): Parameters<SearchPageParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let opts = ferridriver::actions::SearchOptions {
      pattern: p.pattern.clone(),
      regex: p.regex.unwrap_or(false),
      case_sensitive: p.case_sensitive.unwrap_or(false),
      context_chars: p.context_chars.unwrap_or(150),
      css_scope: p.selector.clone(),
      max_results: p.max_results.unwrap_or(25),
    };
    let result = ferridriver::actions::search_page(page.inner(), &opts)
      .await
      .map_err(Self::err)?;
    Ok(CallToolResult::success(vec![Content::text(
      ferridriver::actions::format_search_results(&result, &p.pattern),
    )]))
  }

  #[tool(
    name = "find_elements",
    description = "List DOM nodes matching a CSS or ferridriver rich selector. Use snapshot+ref to interact with one element; use this to discover many links/rows at once."
  )]
  async fn find_elements(&self, Parameters(p): Parameters<FindElementsParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let opts = ferridriver::actions::FindElementsOptions {
      selector: p.selector.clone(),
      attributes: p.attributes.clone(),
      max_results: p.max_results.unwrap_or(50),
      include_text: p.include_text.unwrap_or(true),
    };
    let result = ferridriver::actions::find_elements(page.inner(), &opts)
      .await
      .map_err(Self::err)?;
    Ok(CallToolResult::success(vec![Content::text(
      ferridriver::actions::format_find_results(&result, &p.selector),
    )]))
  }

  #[tool(
    name = "get_markdown",
    description = "Extract page content as clean markdown. More useful than raw HTML for reading and analysis."
  )]
  async fn get_markdown(&self, Parameters(p): Parameters<SessionOnlyParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let md = page.markdown().await.map_err(Self::err)?;
    Ok(CallToolResult::success(vec![Content::text(md)]))
  }
}
