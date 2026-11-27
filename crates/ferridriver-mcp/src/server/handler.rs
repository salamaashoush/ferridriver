//! `rmcp::ServerHandler` implementation and trace-context wiring for [`McpServer`].

use super::McpServer;
use rmcp::{
  ErrorData, RoleServer, ServerHandler,
  model::{
    CallToolResponse, GetPromptRequestParams, GetPromptResponse, ListPromptsResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, Resource, ServerCapabilities,
    ServerConfig,
  },
  service::RequestContext,
  tool_handler,
};
use tracing::Instrument;

/// Parse a W3C `traceparent` header (`version-traceId-parentId-flags`) into
/// its `(traceId, parentSpanId)` hex pair. Returns `None` for anything that
/// doesn't match the trace-context shape (32-hex trace id, 16-hex span id,
/// neither all-zero), so a malformed value never pollutes the span.
fn parse_traceparent(tp: &str) -> Option<(&str, &str)> {
  let mut parts = tp.split('-');
  let _version = parts.next()?;
  let trace_id = parts.next()?;
  let parent_id = parts.next()?;
  let _flags = parts.next()?;
  let all_hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
  if trace_id.len() == 32
    && parent_id.len() == 16
    && all_hex(trace_id)
    && all_hex(parent_id)
    && trace_id.bytes().any(|b| b != b'0')
    && parent_id.bytes().any(|b| b != b'0')
  {
    Some((trace_id, parent_id))
  } else {
    None
  }
}

/// Build the dispatch span for a tool call, linked to the caller's trace when
/// `_meta.traceparent` (SEP-414) carries a valid W3C trace context.
fn tool_call_span(tool: &str, meta: Option<&rmcp::model::RequestMetaObject>) -> tracing::Span {
  if let Some((trace_id, parent_span_id)) = meta.and_then(|m| m.get_traceparent()).and_then(parse_traceparent) {
    tracing::info_span!("mcp.call_tool", tool, trace_id, parent_span_id)
  } else {
    tracing::info_span!("mcp.call_tool", tool)
  }
}

/// How long a client may treat a tool listing as fresh. Bounded rather than
/// indefinite so a client that misses the `tools/list_changed` notification
/// still picks a reloaded extension's tools up on its own.
const TOOL_LIST_TTL_MS: u64 = 5 * 60 * 1000;

/// Whether this peer negotiated a protocol version that defines the SEP-2549
/// `ttlMs` / `cacheScope` hints. Older peers get the listing without them.
fn supports_cache_hints(context: &RequestContext<RoleServer>) -> bool {
  context
    .protocol_version()
    .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
  fn get_info(&self) -> ServerConfig {
    ServerConfig::new(
      // Logging capability dropped: deprecated by MCP SEP-2577 (rmcp
      // deprecates `enable_logging` and will remove it); the server
      // never emitted `notifications/message` anyway.
      ServerCapabilities::builder()
        .enable_tools()
        // Extensions reload without a restart, so the advertised tool set
        // can change mid-session; a client that does not know that would
        // keep calling a tool that no longer exists.
        .enable_tool_list_changed()
        .enable_resources()
        .enable_prompts()
        .build(),
    )
    .with_server_info(rmcp::model::Implementation::new(
      self.config.server_name(),
      env!("CARGO_PKG_VERSION"),
    ))
    .with_instructions(self.instructions_with_paths())
  }

  /// Manual `call_tool` (replaces the one `#[tool_handler]` would generate)
  /// so every tool dispatch runs inside a tracing span seeded from the
  /// caller's W3C `traceparent` (`_meta`, SEP-414). This threads the MCP
  /// client's trace/span ids into the ferridriver → CDP spans, giving one
  /// correlated trace across the whole automation. Dispatch itself is
  /// identical to the generated version (`ToolCallContext` → router).
  ///
  /// It is also the single place the two MCP error channels are sorted
  /// out. A handler that fails an operation reports it as a tool
  /// execution error (`isError: true`, message in `content`) so the
  /// model sees it and can adapt; only errors that mean "this request
  /// could not be processed at all" — an unknown tool, arguments the
  /// declared schema rejects — stay JSON-RPC errors for the host.
  async fn call_tool(
    &self,
    request: rmcp::model::CallToolRequestParams,
    context: RequestContext<RoleServer>,
  ) -> Result<CallToolResponse, ErrorData> {
    // The serve loop moves the request's `_meta` into `context.meta` before
    // dispatch (so the progress token isn't lost), leaving `request.meta`
    // empty — read the trace context from the context, not the request.
    let span = tool_call_span(request.name.as_ref(), Some(&context.meta));

    // Promoted extension tools live outside the static router (their set
    // changes on reload), so dispatch them here. Built-ins win: a
    // colliding extension name is never promoted in the first place.
    if !self.tool_router.has_route(request.name.as_ref()) && self.has_promoted_extension_tool(request.name.as_ref()) {
      let name = request.name.to_string();
      let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
      return match Box::pin(self.invoke_extension_tool(&name, args).instrument(span)).await {
        Ok(result) => Ok(result.into()),
        Err(e) if e.code == rmcp::model::ErrorCode::INTERNAL_ERROR => Ok(Self::tool_failure(&e).into()),
        Err(e) => Err(e),
      };
    }

    let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
    match self.tool_router.call(tcc).instrument(span).await {
      Ok(result) => Ok(result),
      Err(e) if e.code == rmcp::model::ErrorCode::INTERNAL_ERROR => Ok(Self::tool_failure(&e).into()),
      Err(e) => Err(e),
    }
  }

  /// Manual `list_tools` (replaces the one `#[tool_handler]` would
  /// generate) so the reloadable extension tools are advertised beside
  /// the static router's built-ins.
  fn list_tools(
    &self,
    _request: Option<PaginatedRequestParams>,
    context: RequestContext<RoleServer>,
  ) -> impl std::future::Future<Output = Result<rmcp::model::ListToolsResult, ErrorData>> + Send {
    let mut tools = self.tool_router.list_all();
    tools.extend(self.extensions().promoted.iter().cloned());
    let result = rmcp::model::ListToolsResult::with_all_items(tools);
    // The listing is worth caching — the built-ins are compiled in and an
    // extension reload publishes `tools/list_changed`, which invalidates the
    // client's copy — but the hints are only spec-legal for a peer that
    // negotiated 2026-07-28.
    std::future::ready(Ok(if supports_cache_hints(&context) {
      result
        .with_ttl_ms(TOOL_LIST_TTL_MS)
        .with_cache_scope(rmcp::model::CacheScope::Public)
    } else {
      result
    }))
  }

  /// Same reason as [`Self::list_tools`]: a promoted extension tool must
  /// be discoverable by name, not just in the list.
  fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
    self
      .tool_router
      .get(name)
      .cloned()
      .or_else(|| self.promoted_extension_tool(name))
  }

  async fn list_resources(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> Result<ListResourcesResult, ErrorData> {
    let state = self.state.read().await;
    let contexts = state.list_contexts().await;
    drop(state);

    let mut resources = Vec::new();
    let res = |uri: &str, name: &str, desc: &str, mime: &str| -> Resource {
      Resource::new(uri, name).with_description(desc).with_mime_type(mime)
    };

    for c in &contexts {
      let s = &c.name;
      let url = c.pages.iter().find(|p| p.active).map_or("", |p| p.url.as_str());
      let title = c.pages.iter().find(|p| p.active).map_or("", |p| p.title.as_str());
      resources.push(res(
        &format!("browser://session/{s}/page-info"),
        &format!("[{s}] Page Info"),
        &format!("{url} -- {title}"),
        "application/json",
      ));
      resources.push(res(
        &format!("browser://session/{s}/snapshot"),
        &format!("[{s}] Snapshot"),
        &format!("A11y tree for session '{s}'"),
        "text/plain",
      ));
      resources.push(res(
        &format!("browser://session/{s}/screenshot"),
        &format!("[{s}] Screenshot"),
        &format!("PNG screenshot of session '{s}'"),
        "image/png",
      ));
      resources.push(res(
        &format!("browser://session/{s}/console"),
        &format!("[{s}] Console"),
        &format!("Console messages in session '{s}'"),
        "application/json",
      ));
      resources.push(res(
        &format!("browser://session/{s}/network"),
        &format!("[{s}] Network"),
        &format!("Network requests in session '{s}'"),
        "application/json",
      ));
      resources.push(res(
        &format!("browser://session/{s}/cookies"),
        &format!("[{s}] Cookies"),
        &format!("Cookies in session '{s}'"),
        "application/json",
      ));
    }

    // Files written under artifacts_root (screenshots, PDFs, traces) are
    // fetchable as `artifact://<relpath>` resources.
    self.list_artifact_resources(&mut resources).await;

    let result = ListResourcesResult {
      resources,
      ..Default::default()
    };
    Ok(result)
  }

  async fn read_resource(
    &self,
    request: ReadResourceRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> Result<ReadResourceResponse, ErrorData> {
    Box::pin(self.read_resource_contents(request)).await.map(Into::into)
  }

  fn list_prompts(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> impl std::future::Future<Output = Result<ListPromptsResult, ErrorData>> + Send {
    std::future::ready(Ok(ListPromptsResult::with_all_items(Self::prompt_definitions())))
  }

  fn get_prompt(
    &self,
    request: GetPromptRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> impl std::future::Future<Output = Result<GetPromptResponse, ErrorData>> + Send {
    std::future::ready(Self::prompt_messages(&request).map(Into::into))
  }
}
