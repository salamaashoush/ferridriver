//! `McpServer` server struct and shared helpers used by all tools.

use arc_swap::ArcSwap;
use base64::Engine;
use dashmap::DashMap;
use ferridriver::Page;
use ferridriver::actions;
use ferridriver::backend::BackendKind;
use ferridriver::backend::{AnyElement, AnyPage};
use ferridriver::snapshot;
use ferridriver::state::{BrowserState, ConnectMode, ContextLogHandles};
use rmcp::{
  ErrorData, RoleServer, ServerHandler,
  handler::server::router::tool::ToolRouter,
  model::{
    Annotated, CallToolResult, Content, GetPromptRequestParams, GetPromptResult, ListPromptsResult,
    ListResourcesResult, PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, PromptMessageRole, RawResource,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerInfo,
    SetLevelRequestParams,
  },
  service::RequestContext,
  tool_handler,
};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

// ── SharedState ──────────────────────────────────────────────────────────────

/// Shared state for the MCP server.
///
/// Hot paths (`ref_map` reads, log reads) use extracted `Arc` handles cached in
/// `DashMap`s and bypass the `RwLock` entirely. Cold paths (instance init, page
/// management) use the `RwLock<BrowserState>`.
#[derive(Clone)]
pub struct SharedState {
  /// The underlying browser state. Write-locked only for mutations
  /// (`ensure_instance`, `open_page`, `close_page`, `shutdown`, `connect`).
  /// Read-locked for lookups that extract `Arc` handles.
  inner: Arc<RwLock<BrowserState>>,
  /// Cached `ref_map` handles per context — wait-free reads via `ArcSwap`.
  ref_maps: Arc<DashMap<String, RefMapHandle>>,
  /// Cached log handles per context.
  log_handles: Arc<DashMap<String, ContextLogHandles>>,
  /// Per-context serialization locks (replaces nested `Mutex<HashMap<..>>`).
  context_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

/// Type alias for the `ArcSwap`-wrapped ref map used for wait-free reads.
type RefMapHandle = Arc<ArcSwap<FxHashMap<String, i64>>>;

impl SharedState {
  fn new(browser_state: BrowserState) -> Self {
    Self {
      inner: Arc::new(RwLock::new(browser_state)),
      ref_maps: Arc::new(DashMap::new()),
      log_handles: Arc::new(DashMap::new()),
      context_locks: Arc::new(DashMap::new()),
    }
  }

  /// Write-lock the inner state (for mutations).
  pub(crate) async fn write(&self) -> tokio::sync::RwLockWriteGuard<'_, BrowserState> {
    self.inner.write().await
  }

  /// Read-lock the inner state (for lookups).
  pub(crate) async fn read(&self) -> tokio::sync::RwLockReadGuard<'_, BrowserState> {
    self.inner.read().await
  }

  /// Get the `ref_map` for a context as a cloned `FxHashMap` (wait-free via cached `ArcSwap`).
  pub(crate) async fn ref_map_for(&self, context: &str) -> FxHashMap<String, i64> {
    // Fast path: cached handle exists
    if let Some(entry) = self.ref_maps.get(context) {
      return (**entry.value().load()).clone();
    }
    // Slow path: read-lock state, extract handle, cache it
    let state = self.inner.read().await;
    if let Some(handle) = state.ref_map_handle(context) {
      let cloned = (**handle.load()).clone();
      drop(state);
      self.ref_maps.insert(context.to_string(), handle);
      cloned
    } else {
      FxHashMap::default()
    }
  }

  /// Get a cached `ArcSwap` handle for storing `ref_map`s (wait-free store).
  pub(crate) async fn ref_map_handle(&self, context: &str) -> Option<RefMapHandle> {
    if let Some(entry) = self.ref_maps.get(context) {
      return Some(Arc::clone(entry.value()));
    }
    let state = self.inner.read().await;
    let handle = state.ref_map_handle(context)?;
    drop(state);
    self.ref_maps.insert(context.to_string(), Arc::clone(&handle));
    Some(handle)
  }

  /// Get cached log handles for a context (no `BrowserState` lock after first call).
  pub(crate) async fn log_handles_for(&self, context: &str) -> Option<ContextLogHandles> {
    if let Some(entry) = self.log_handles.get(context) {
      return Some(entry.value().clone());
    }
    let state = self.inner.read().await;
    let handles = state.log_handles(context)?;
    drop(state);
    self.log_handles.insert(context.to_string(), handles.clone());
    Some(handles)
  }

  /// Invalidate caches for a context (after `close_page`, new page, etc.).
  pub(crate) fn invalidate_context(&self, context: &str) {
    self.ref_maps.remove(context);
    self.log_handles.remove(context);
  }

  /// Invalidate all caches (after shutdown).
  pub(crate) fn invalidate_all(&self) {
    self.ref_maps.clear();
    self.log_handles.clear();
  }
}

/// Backward-compat type alias.
pub type State = SharedState;

/// Backward-compat free function: derive context from session only.
#[must_use]
pub fn ctx(s: Option<&String>) -> &str {
  s.map_or("default", String::as_str)
}

// Backward-compat alias so existing tool code keeps compiling during transition.
pub use self::ctx as sess;

// ── Configuration trait ─────────────────────────────────────────────────────

/// Trait for customizing the MCP server behavior.
///
/// Implement this to control chrome launch args, browser instance resolution,
/// server metadata, and pre-dispatch validation. The library stays generic --
/// any domain-specific concepts (environments, auth, etc.) belong in the
/// consumer's own `ServerHandler` wrapper.
pub trait McpServerConfig: Send + Sync + 'static {
  /// Base Chrome arguments applied to ALL browser instances.
  ///
  /// Called once at server construction. Override to inject flags that
  /// apply globally (e.g. shared proxy settings).
  fn chrome_args(&self) -> Vec<String> {
    Vec::new()
  }

  /// Additional Chrome arguments for a specific browser instance.
  ///
  /// Called when launching a new Chrome process for the given instance name.
  /// The instance name comes from the composite session key `"<instance>:<context>"`.
  /// Override to inject per-instance flags like DNS resolver rules, cert flags.
  ///
  /// Default: no additional args (all instances get the same base flags).
  fn chrome_args_for_instance(&self, _instance: &str) -> Vec<String> {
    Vec::new()
  }

  /// Resolve how to connect to a browser instance by name.
  ///
  /// Called before launching a new browser. If this returns `Some(ConnectMode)`,
  /// ferridriver connects to an existing browser instead of launching a new one.
  ///
  /// Use this to integrate with external browser managers:
  /// - Read a `DevToolsActivePort` file from a known profile directory
  /// - Query a service registry for running browser endpoints
  /// - Connect to a browser launched by another tool with debugging enabled
  ///
  /// The instance name comes from the session key (e.g. `"staging"` from `"staging:admin"`).
  /// Return `None` to fall through to the default behavior (launch a new browser).
  fn resolve_instance(&self, _instance: &str) -> Option<ConnectMode> {
    None
  }

  /// Server name for MCP `get_info`.
  fn server_name(&self) -> &'static str {
    "ferridriver"
  }

  /// Server instructions for MCP `get_info`.
  fn server_instructions(&self) -> &str {
    DEFAULT_INSTRUCTIONS
  }

  /// Called before dispatching each tool call.
  /// Return `Err(message)` to block the call with an error.
  ///
  /// # Errors
  ///
  /// Returns an error string to reject the tool call before it executes.
  fn before_dispatch(&self, _tool_name: &str, _args: &serde_json::Value) -> Result<(), String> {
    Ok(())
  }
}

/// Default instructions embedded in the MCP server.
pub const DEFAULT_INSTRUCTIONS: &str = "Browser automation. All tools accept optional 'session' param (default: 'default'). \
     Different sessions have isolated cookies/storage -- use for multi-user testing.\n\
     Actions return an accessibility snapshot with [ref=eN] identifiers. \
     Use these refs with click/hover/fill. Prefer snapshot over screenshot.\n\
     IMPORTANT: Refs are tied to the current page snapshot. After page(select) or navigate, \
     old refs are invalid -- use the new snapshot's refs. Always prefer 'ref' over CSS selectors \
     in click/fill/hover since refs are resolved from the snapshot and work reliably across frames.\n\
     To switch between browser tabs, use page(action='list') then page(action='select', page_index=N). \
     Do NOT use evaluate to list or switch tabs.";

/// Default config for standalone ferridriver (no customization).
pub struct DefaultConfig;
impl McpServerConfig for DefaultConfig {}

// ── McpServer ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct McpServer {
  pub(crate) state: SharedState,
  /// The composed tool router. Public so consumers can list tools or dispatch directly.
  pub tool_router: ToolRouter<Self>,
  /// Configuration trait object for customizing server behavior.
  pub config: Arc<dyn McpServerConfig>,
  /// Typed extension slot for consumer-specific state (e.g. Jira clients).
  extensions: Arc<dyn std::any::Any + Send + Sync>,
}

impl std::fmt::Debug for McpServer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("McpServer").finish()
  }
}

/// Unit struct used as the default extensions value.
struct NoExtensions;

impl McpServer {
  /// Create a server with default config (standalone mode).
  #[must_use]
  pub fn new(mode: ConnectMode, backend: BackendKind) -> Self {
    Self::with_options(mode, backend, false, Arc::new(DefaultConfig))
  }

  /// Create a server with headless option.
  #[must_use]
  pub fn new_headless(mode: ConnectMode, backend: BackendKind, headless: bool) -> Self {
    Self::with_options(mode, backend, headless, Arc::new(DefaultConfig))
  }

  /// Create a server with a custom config.
  pub fn with_config(mode: ConnectMode, backend: BackendKind, config: Arc<dyn McpServerConfig>) -> Self {
    Self::with_options(mode, backend, false, config)
  }

  /// Create a server with all options.
  pub fn with_options(mode: ConnectMode, backend: BackendKind, headless: bool, config: Arc<dyn McpServerConfig>) -> Self {
    let mut browser_state = BrowserState::new(mode, backend);
    browser_state.headless = headless;
    browser_state.extra_args = config.chrome_args();
    // Wire per-instance args callback from config trait.
    let config_clone = Arc::clone(&config);
    browser_state.set_instance_args_fn(Box::new(move |instance| {
      config_clone.chrome_args_for_instance(instance)
    }));
    // Wire per-instance connection resolver from config trait.
    let config_clone = Arc::clone(&config);
    browser_state.set_instance_resolver_fn(Box::new(move |instance| {
      config_clone.resolve_instance(instance)
    }));
    let state = SharedState::new(browser_state);
    Self {
      state,
      tool_router: Self::combined_router(),
      config,
      extensions: Arc::new(NoExtensions),
    }
  }

  /// Add extra tool routers (merges with built-in browser tools).
  #[must_use]
  pub fn with_extra_tools(mut self, extra: ToolRouter<Self>) -> Self {
    self.tool_router += extra;
    self
  }

  /// Attach custom state accessible from tool handlers via `extension()`.
  #[must_use]
  pub fn with_extension<T: Send + Sync + 'static>(mut self, ext: Arc<T>) -> Self {
    self.extensions = ext;
    self
  }

  /// Access a typed extension stored on the server.
  #[must_use]
  pub fn extension<T: Send + Sync + 'static>(&self) -> Option<&T> {
    self.extensions.downcast_ref::<T>()
  }

  pub fn err(msg: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(msg.into(), None)
  }

  pub async fn context_guard(&self, context: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = self
      .state
      .context_locks
      .entry(context.to_string())
      .or_insert_with(|| Arc::new(Mutex::new(())))
      .clone();
    lock.lock_owned().await
  }

  // Backward-compat alias.
  pub async fn session_guard(&self, context: &str) -> tokio::sync::OwnedMutexGuard<()> {
    self.context_guard(context).await
  }

  /// Get a Page for a context, ensuring the required browser instance exists.
  /// Parses the composite session key to ensure the correct instance is launched.
  ///
  /// Fast path (instance exists): shared read lock — concurrent reads allowed.
  /// Slow path (cold start): exclusive write lock — only when launching a new browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn page(&self, context: &str) -> Result<Page, ErrorData> {
    // Fast path: instance already exists, read lock only
    {
      let state = self.state.read().await;
      if let Ok(any_page) = state.active_page(context) {
        return Ok(Page::new(any_page.clone()));
      }
    }
    // Slow path: need to create instance (write lock)
    let key = ferridriver::state::SessionKey::parse(context);
    let mut state = self.state.write().await;
    Box::pin(state.ensure_instance(&key.instance))
      .await
      .map_err(Self::err)?;
    let any_page = state.active_page(context).map_err(Self::err)?.clone();
    Ok(Page::new(any_page))
  }

  /// Get raw `AnyPage` (for low-level ops that Page doesn't cover yet).
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn raw_page(&self, context: &str) -> Result<AnyPage, ErrorData> {
    // Fast path: read lock
    {
      let state = self.state.read().await;
      if let Ok(any_page) = state.active_page(context) {
        return Ok(any_page.clone());
      }
    }
    // Slow path: write lock
    let key = ferridriver::state::SessionKey::parse(context);
    let mut state = self.state.write().await;
    Box::pin(state.ensure_instance(&key.instance))
      .await
      .map_err(Self::err)?;
    let page = state.active_page(context).map_err(Self::err)?.clone();
    Ok(page)
  }

  /// Resolve ref to element -- delegates to `actions::resolve_element`.
  ///
  /// # Errors
  ///
  /// Returns an error if neither ref nor selector resolves to a valid element,
  /// or if the underlying element lookup fails.
  pub async fn resolve(
    page: &Page,
    ref_map: &rustc_hash::FxHashMap<String, i64>,
    r#ref: Option<&String>,
    selector: Option<&String>,
  ) -> Result<AnyElement, String> {
    actions::resolve_element(
      page.inner(),
      ref_map,
      r#ref.map(String::as_str),
      selector.map(String::as_str),
    )
    .await
  }

  /// Build snapshot text and store `ref_map` for the context.
  /// Uses a 5-second timeout to avoid hanging on unresponsive pages.
  /// Stores the `ref_map` via wait-free `ArcSwap` — never drops updates.
  pub async fn snap(&self, page: &Page, context: &str) -> String {
    let snap_fut = page.snapshot_for_ai(snapshot::SnapshotOptions::default());
    match tokio::time::timeout(std::time::Duration::from_secs(5), snap_fut).await {
      Ok(Ok(result)) => {
        // Wait-free store via cached ArcSwap handle
        if let Some(handle) = self.state.ref_map_handle(context).await {
          handle.store(Arc::new(result.ref_map));
        } else {
          // Fallback: read-lock state to store (context may not be cached yet)
          let state = self.state.read().await;
          state.set_ref_map(context, result.ref_map);
        }
        result.full
      },
      Ok(Err(e)) => format!("\n[snapshot error: {e}]"),
      Err(_) => "\n[snapshot timed out — page may be unresponsive or have a very large DOM]".to_string(),
    }
  }

  /// Action result: description + auto-snapshot.
  ///
  /// # Errors
  ///
  /// Returns an `ErrorData` if snapshot acquisition fails critically
  /// (soft failures produce inline error text instead).
  pub async fn action_ok(&self, page: &Page, context: &str, msg: &str) -> Result<CallToolResult, ErrorData> {
    let snap = self.snap(page, context).await;
    Ok(CallToolResult::success(vec![Content::text(format!("{msg}\n\n{snap}"))]))
  }
}

#[tool_handler]
impl ServerHandler for McpServer {
  fn get_info(&self) -> ServerInfo {
    ServerInfo::new(
      ServerCapabilities::builder()
        .enable_tools()
        .enable_resources()
        .enable_prompts()
        .enable_logging()
        .build(),
    )
    .with_instructions(self.config.server_instructions().to_string())
  }

  fn set_level(
    &self,
    _request: SetLevelRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> impl std::future::Future<Output = Result<(), ErrorData>> + Send + '_ {
    std::future::ready(Ok(()))
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
      Annotated::new(
        RawResource {
          uri: uri.into(),
          name: name.into(),
          title: None,
          description: Some(desc.into()),
          mime_type: Some(mime.into()),
          size: None,
          icons: None,
          meta: None,
        },
        None,
      )
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
  ) -> Result<ReadResourceResult, ErrorData> {
    let uri = &request.uri;
    let (context_name, resource) = if let Some(rest) = uri.strip_prefix("browser://session/") {
      let mut parts = rest.splitn(2, '/');
      (
        parts.next().unwrap_or("default").to_string(),
        parts.next().unwrap_or("").to_string(),
      )
    } else if let Some(stripped) = uri.strip_prefix("browser://") {
      ("default".to_string(), stripped.to_string())
    } else {
      return Err(Self::err(format!("Unknown resource URI: {uri}")));
    };

    let page = Box::pin(self.page(&context_name)).await?;

    match resource.as_str() {
      "page-info" => {
        let url = page.url().await.unwrap_or_default();
        let title = page.title().await.unwrap_or_default();
        let json =
          serde_json::to_string_pretty(&serde_json::json!({"url": url, "title": title, "session": context_name}))
            .unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(json, uri).with_mime_type("application/json"),
        ]))
      },
      "console" => {
        let handles = self
          .state
          .log_handles_for(&context_name)
          .await
          .ok_or_else(|| Self::err(format!("Context '{context_name}' not found")))?;
        let msgs = handles.console.read().await;
        let last: Vec<_> = msgs.iter().rev().take(100).cloned().collect::<Vec<_>>().into_iter().rev().collect();
        drop(msgs);
        let text = serde_json::to_string_pretty(&last).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
      },
      "network" => {
        let handles = self
          .state
          .log_handles_for(&context_name)
          .await
          .ok_or_else(|| Self::err(format!("Context '{context_name}' not found")))?;
        let reqs = handles.network.read().await;
        let last: Vec<_> = reqs.iter().rev().take(100).cloned().collect::<Vec<_>>().into_iter().rev().collect();
        drop(reqs);
        let text = serde_json::to_string_pretty(&last).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
      },
      "snapshot" => {
        let snap = self.snap(&page, &context_name).await;
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(snap, uri).with_mime_type("text/plain"),
        ]))
      },
      "screenshot" => {
        let bytes = page
          .screenshot(ferridriver::options::ScreenshotOptions::default())
          .await
          .map_err(Self::err)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(ReadResourceResult::new(vec![
          ResourceContents::blob(b64, uri).with_mime_type("image/png"),
        ]))
      },
      "cookies" => {
        let cookies = page.inner().get_cookies().await.map_err(Self::err)?;
        let list: Vec<serde_json::Value> = cookies
          .iter()
          .map(|c| serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain}))
          .collect();
        let text = serde_json::to_string_pretty(&list).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
      },
      _ => Err(Self::err(format!("Unknown resource: {uri}"))),
    }
  }

  async fn list_prompts(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> Result<ListPromptsResult, ErrorData> {
    let prompts = vec![
      Prompt::new(
        "debug-page",
        Some("Analyze the page for errors, broken elements, and console issues"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("URL to debug")
            .with_required(false),
        ]),
      ),
      Prompt::new(
        "test-form",
        Some("Fill and submit a form, verify the result"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("Page URL with the form")
            .with_required(true),
          PromptArgument::new("submit_selector")
            .with_description("Submit button selector")
            .with_required(false),
        ]),
      ),
      Prompt::new(
        "audit-accessibility",
        Some("Check page accessibility using the a11y tree"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("URL to audit")
            .with_required(true),
        ]),
      ),
      Prompt::new(
        "compare-sessions",
        Some("Compare page state between two browser sessions"),
        Some(vec![
          PromptArgument::new("url")
            .with_description("URL to compare")
            .with_required(true),
          PromptArgument::new("session_a")
            .with_description("First session")
            .with_required(true),
          PromptArgument::new("session_b")
            .with_description("Second session")
            .with_required(true),
        ]),
      ),
    ];
    let result = ListPromptsResult {
      prompts,
      ..Default::default()
    };
    Ok(result)
  }

  async fn get_prompt(
    &self,
    request: GetPromptRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> Result<GetPromptResult, ErrorData> {
    let args = request.arguments.unwrap_or_default();
    let get_arg = |key: &str| -> String { args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() };
    let url = get_arg("url");

    match request.name.as_str() {
      "debug-page" => {
        let nav = if url.is_empty() {
          String::new()
        } else {
          format!("First navigate to {url}.\n")
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
          PromptMessageRole::User,
          format!(
            "{nav}Debug the current page:\n1. Take a snapshot to understand the page structure\n2. Check console_messages for errors\n3. Check network_requests for failed requests (4xx/5xx)\n4. Report all issues found with suggested fixes"
          ),
        )]))
      },
      "test-form" => {
        let submit = {
          let s = get_arg("submit_selector");
          if s.is_empty() { "the submit button".into() } else { s }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
          PromptMessageRole::User,
          format!(
            "Test the form on {url}:\n1. Navigate to the page\n2. Take a snapshot to identify form fields\n3. Fill all required fields with realistic test data\n4. Click {submit}\n5. Verify the form submitted successfully\n6. Report the result"
          ),
        )]))
      },
      "audit-accessibility" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::User,
        format!(
          "Audit the accessibility of {url}:\n1. Navigate to the page\n2. Take a snapshot (a11y tree)\n3. Check for: missing labels, incorrect heading hierarchy, images without alt text, interactive elements without accessible names, form inputs without labels\n4. Report issues with severity and how to fix each one"
        ),
      )])),
      "compare-sessions" => {
        let sa = {
          let s = get_arg("session_a");
          if s.is_empty() { "userA".into() } else { s }
        };
        let sb = {
          let s = get_arg("session_b");
          if s.is_empty() { "userB".into() } else { s }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
          PromptMessageRole::User,
          format!(
            "Compare {url} between two sessions:\n1. Open the page in session='{sa}' and session='{sb}'\n2. Take a snapshot of each\n3. Compare: visible content differences, available navigation, form fields, cookies\n4. Report what differs between the two sessions"
          ),
        )]))
      },
      _ => Err(Self::err(format!("Unknown prompt: {}", request.name))),
    }
  }
}
