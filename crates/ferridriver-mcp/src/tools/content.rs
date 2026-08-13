use crate::params::{EvaluateParams, ScreenshotParams_, SearchPageParams, SnapshotParams};
use crate::server::{McpServer, sess};
use base64::Engine;
use ferridriver::options::ScreenshotOptions;
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, ContentBlock},
  tool, tool_router,
};

#[tool_router(router = content_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "snapshot",
    title = "Accessibility Snapshot",
    description = "PRIMARY grounding tool — call this FIRST before deciding on any selectors or actions. \
    Returns the page as an accessibility tree: every interactable role/name, visible text, and \
    [ref=eN] handles. Cheap, fast, token-efficient, and deterministic — much better than screenshot \
    for picking what to click/fill. Supports depth limiting and incremental tracking (shows only \
    what changed since the last snapshot). Re-snapshot after any navigate/click/fill/run_script; \
    refs are invalidated by DOM mutations.",
    annotations(read_only_hint = true, open_world_hint = false)
  )]
  async fn snapshot(&self, Parameters(p): Parameters<SnapshotParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let opts = ferridriver::snapshot::SnapshotOptions {
      depth: p.depth,
      track: p.track,
    };
    match page.snapshot_for_ai().options(opts).await {
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
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
      },
      Err(e) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
        "[snapshot error: {e}]"
      ))])),
    }
  }

  #[tool(
    name = "screenshot",
    title = "Screenshot",
    description = "Capture the page (or a single element via `selector`, or the full scrollable page \
    via `full_page`) as a base64-encoded image. USE SPARINGLY — it is much more token-expensive \
    than `snapshot`. Reach for it only when the a11y tree is ambiguous (icons without labels, \
    canvas, complex layout), or when the caller explicitly needs visual verification. The image is \
    also persisted under artifacts_root and returned as a resource link for on-demand re-fetch.",
    annotations(read_only_hint = true, open_world_hint = false)
  )]
  async fn screenshot(&self, Parameters(p): Parameters<ScreenshotParams_>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let format = p.format.unwrap_or_default();
    let mime = format.mime();
    let bytes = if let Some(sel) = &p.selector {
      page.screenshot_element(sel).await.map_err(Self::err)?
    } else {
      let opts = ScreenshotOptions {
        format: Some(format.into()),
        quality: p.quality,
        full_page: p.full_page,
        ..Default::default()
      };
      page.screenshot().options(opts).await.map_err(Self::err)?
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mut out = vec![ContentBlock::image(b64, mime)];
    // Persist the capture under artifacts_root and hand back a resource link
    // so the caller can re-fetch it on demand (no re-capture, no re-inlining).
    let ext = format.extension();
    let stamp = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |d| d.as_millis());
    let safe = s.replace([':', '/', '\\'], "_");
    let rel = format!("screenshots/{safe}-{stamp}.{ext}");
    if let Some(link) = self.persist_artifact(&rel, &bytes, mime).await {
      out.push(link);
    }
    Ok(CallToolResult::success(out))
  }

  #[tool(
    name = "evaluate",
    title = "Evaluate JavaScript",
    description = "Evaluate a single JavaScript expression IN the page (DOM context) and return its \
    JSON-serialized value. Use for quick one-liners: `document.title`, \
    `document.querySelectorAll('.row').length`, feature-detection. For multi-step imperative \
    logic — loops, conditionals, try/catch, chained navigations — use `run_script` instead. \
    The expression runs with the page's globals (`document`, `window`, `fetch`, etc.).",
    annotations(read_only_hint = false, open_world_hint = true)
  )]
  async fn evaluate(&self, Parameters(p): Parameters<EvaluateParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    let result = page
      .evaluate(
        p.expression.as_str(),
        ferridriver::protocol::SerializedArgument::default(),
        None,
      )
      .await
      .map_err(Self::err)?;
    let val = result.to_json_like().map_or_else(
      || result.as_string_lossy(),
      |v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
    );
    Ok(CallToolResult::success(vec![ContentBlock::text(val)]))
  }

  #[tool(
    name = "search_page",
    title = "Search Page Text",
    description = "Grep the page's rendered text for a pattern (literal or regex), returning matches \
    with surrounding context. Fast and token-cheap; use to locate content without re-reading the \
    whole snapshot. Supports `regex`, `case_sensitive`, and `selector` for scoped search.",
    annotations(read_only_hint = true, open_world_hint = false)
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
    Ok(CallToolResult::success(vec![ContentBlock::text(
      ferridriver::actions::format_search_results(&result, &p.pattern),
    )]))
  }
}
