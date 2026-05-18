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
use std::time::Instant;
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

  /// Get a clone of the inner `Arc<RwLock<BrowserState>>` for constructing `ContextRef`.
  pub(crate) fn state_arc(&self) -> Arc<RwLock<BrowserState>> {
    Arc::clone(&self.inner)
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
  /// Root directory for the scripting sandbox used by `run_script`.
  ///
  /// All `fs` operations inside scripts (`readFile`, `writeFile`, `readdir`,
  /// `exists`) and all dynamic `import(...)` calls are constrained to this
  /// directory — traversal (`..`), absolute paths, and symlink escapes are
  /// rejected. The directory is created at server startup if it does not
  /// exist.
  ///
  /// Default: `./.ferridriver/scripts` relative to cwd. The dotfolder
  /// convention avoids colliding with the common `scripts/` directory most
  /// projects already use for build/CI tooling, and leaves room for sibling
  /// subdirectories (`.ferridriver/artifacts`, `.ferridriver/cache`, ...)
  /// without further namespace pollution.
  fn script_root(&self) -> std::path::PathBuf {
    std::path::PathBuf::from(".ferridriver/scripts")
  }

  /// Root directory for script output artifacts (screenshots, PDFs, traces,
  /// downloaded bodies). Exposed to scripts as the `artifacts` global.
  ///
  /// Kept separate from `script_root` so outputs don't pollute the source
  /// tree. Same sandbox rules apply. The directory is created at server
  /// startup if it does not exist.
  ///
  /// Default: `./.ferridriver/artifacts` relative to cwd.
  fn artifacts_root(&self) -> std::path::PathBuf {
    std::path::PathBuf::from(".ferridriver/artifacts")
  }

  /// Engine-level defaults (timeout, memory, console limits) for `run_script`.
  fn script_engine_config(&self) -> ferridriver_script::ScriptEngineConfig {
    ferridriver_script::ScriptEngineConfig::default()
  }

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
  fn server_name(&self) -> &str {
    DEFAULT_SERVER_NAME
  }

  /// Server instructions for MCP `get_info`.
  fn server_instructions(&self) -> &str {
    DEFAULT_INSTRUCTIONS
  }

  /// Paths to plugin files or directories to load at startup.
  ///
  /// Each path is either a single `.js`/`.mjs` file or a directory scanned
  /// shallowly for those extensions. Plugins are loaded once and registered
  /// as `run_script` bindings; manifests marked `exposeAsTool: true` are
  /// additionally surfaced in `tools/list`. Default: no plugins.
  fn plugin_paths(&self) -> Vec<std::path::PathBuf> {
    Vec::new()
  }
}

/// Default server name for MCP `get_info`.
pub const DEFAULT_SERVER_NAME: &str = "ferridriver";

/// Default instructions embedded in the MCP server.
pub const DEFAULT_INSTRUCTIONS: &str = "\
Browser automation via Chrome DevTools Protocol.\n\
\n\
== RECOMMENDED WORKFLOW ==\n\
1. `navigate` or `connect` to bring up a session.\n\
2. `snapshot` to see the page as an accessibility tree (ref=eN handles, text, roles) \
BEFORE deciding on selectors. Cheap, fast, low token cost — always your first action.\n\
3. Act via one of:\n\
   a. `run_script` — sandboxed JS with full `page`, `context`, `request` globals for \
imperative logic (loops, conditionals, try/catch, computed values, HTTP calls). \
Pair with `args` to avoid string interpolation. This is the primary action tool.\n\
   b. `evaluate` — single-line JS executed IN the page (DOM context). Use for \
quick reads; use `run_script` for anything multi-step.\n\
4. `snapshot` again to verify.\n\
\n\
Browser interaction flows through `run_script` bindings:\n\
- Clicks, fills, hovers → `await page.click(sel)`, `await page.fill(sel, val)`, \
`await page.locator(sel).hover()`.\n\
- Locator chains → `page.getByRole('button', ...).first().click()`.\n\
- Cookies, storage, geolocation → `await context.addCookies([...])`, \
`await context.setGeolocation(...)`.\n\
- Waits → `await page.waitForSelector(sel, { state, timeout })`.\n\
- API calls → `await request.get(url)`, `await request.post(url, { json: {...} })`.\n\
- Saving outputs (screenshots, PDFs, traces) → `await artifacts.writeBytes('page.png', \
await page.screenshot())`. The `artifacts` global is rooted at the server's configured \
artifacts_root (default `.ferridriver/artifacts/`) — separate from script source so outputs \
don't pollute your tree.\n\
\n\
== SESSION KEYS ==\n\
All tools accept an optional 'session' parameter. Format: 'instance:context'.\n\
- Instance (before ':') selects which browser process. Each instance can have its own \
Chrome flags, DNS rules, and profile. Examples: 'staging', 'dev', 'prod'.\n\
- Context (after ':') isolates cookies/storage within that browser. Use for multi-user \
testing. Examples: 'admin', 'user1', 'tester'.\n\
- Combined: 'staging:admin' = staging browser, admin context.\n\
- Plain name without ':' uses the default instance: 'mytest' = 'default:mytest'.\n\
- Omitted entirely: uses 'default:default'.\n\
- `run_script` `vars` persist per session: values set via `vars.set(...)` in one call \
are visible to the next `run_script` with the same session. The `vars` global is a \
plain string key/value store (use JSON.stringify for complex values).\n\
\n\
== SNAPSHOTS AND REFS ==\n\
`snapshot` returns an accessibility tree with [ref=eN] identifiers. Refs are tied to \
that specific snapshot — after `navigate`, `page(select)`, or any DOM mutation, old \
refs are invalid. Re-snapshot before acting. When scripting, prefer Playwright-style \
locators (`page.getByRole`, `page.getByText`, `page.locator(selector)`) over refs \
— they survive re-snapshots.\n\
\n\
== TAB MANAGEMENT ==\n\
`page(action='list')` lists tabs, `page(action='select', page_index=N)` switches. Do \
not use `evaluate` or `run_script` to enumerate tabs — CDP page-target mapping is \
only exposed via the `page` tool.\n\
\n\
== SCRIPTING SAFETY ==\n\
`run_script` runs in a sandboxed QuickJS runtime: no raw filesystem access (only \
`fs.*` scoped to script_root for source files + `artifacts.*` scoped to artifacts_root \
for outputs), no runner-side network except via `request.*` (APIRequestContext), no \
`process` / `require` / bare `import`. Caller-controlled data MUST be passed via the \
`args` array, never interpolated into the `source` string — the engine does not protect \
against source-level injection.";

/// Default config for standalone ferridriver (no customization).
pub struct DefaultConfig;
impl McpServerConfig for DefaultConfig {}

// ── McpServer ───────────────────────────────────────────────────────────────

/// One session's persistent script VM slot. `vm` is `None` before the
/// first script for the session and after a poisoning fault discards it
/// (the next call rebuilds transparently). `last_used` drives LRU
/// eviction when the warm-VM cap is exceeded.
pub(crate) struct SessionSlot {
  vm: Option<ferridriver_script::Session>,
  last_used: Instant,
}

impl Default for SessionSlot {
  fn default() -> Self {
    Self {
      vm: None,
      last_used: Instant::now(),
    }
  }
}

#[derive(Clone)]
pub struct McpServer {
  pub(crate) state: SharedState,
  /// The composed tool router. Public so consumers can list tools or dispatch directly.
  pub tool_router: ToolRouter<Self>,
  /// Configuration trait object for customizing server behavior.
  pub config: Arc<dyn McpServerConfig>,
  /// Typed extension slot for consumer-specific state (e.g. Jira clients).
  extensions: Arc<dyn std::any::Any + Send + Sync>,
  /// `QuickJS` scripting engine -- fresh context per `run_script` invocation.
  pub(crate) script_engine: Arc<ferridriver_script::ScriptEngine>,
  /// Filesystem sandbox for scripts (`None` if the configured root could not
  /// be created or canonicalised; `run_script` will return an error).
  pub(crate) script_sandbox: Option<Arc<ferridriver_script::PathSandbox>>,
  /// Filesystem sandbox for script outputs, exposed as the `artifacts`
  /// global. `None` if the configured artifacts root could not be prepared;
  /// in that case scripts just don't get an `artifacts` binding and must
  /// use `fs` for output (which pollutes the script source directory).
  pub(crate) artifacts_sandbox: Option<Arc<ferridriver_script::PathSandbox>>,
  /// Per-session variable stores exposed to scripts via the `vars` global.
  /// Lazily created on first `run_script` for a given session name.
  pub(crate) session_vars: Arc<DashMap<String, Arc<ferridriver_script::InMemoryVars>>>,
  /// Persistent per-session script VMs. A session reuses one `QuickJS`
  /// runtime+context across every `run_script` / plugin call so user
  /// `globalThis` state survives REPL-style. Keyed identically to
  /// `session_vars`; access is serialized by the per-session guard.
  pub(crate) session_vms: Arc<DashMap<String, Arc<Mutex<SessionSlot>>>>,
  /// Plugins discovered + parsed at startup. Empty by default; populated
  /// by [`McpServer::load_plugins`].
  pub(crate) plugins: crate::plugin::PluginRegistry,
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
  pub fn with_options(
    mode: ConnectMode,
    backend: BackendKind,
    headless: bool,
    config: Arc<dyn McpServerConfig>,
  ) -> Self {
    let kind = match backend {
      BackendKind::Bidi => ferridriver::options::BrowserKind::Firefox,
      #[cfg(target_os = "macos")]
      BackendKind::WebKit => ferridriver::options::BrowserKind::WebKit,
      _ => ferridriver::options::BrowserKind::Chromium,
    };
    let mut browser_state = BrowserState::with_plan(
      mode,
      ferridriver::options::LaunchPlan {
        backend,
        kind,
        headless,
        args: config.chrome_args(),
        ..Default::default()
      },
    );
    // Wire per-instance args callback from config trait.
    let config_clone = Arc::clone(&config);
    browser_state.set_instance_args_fn(Box::new(move |instance| {
      config_clone.chrome_args_for_instance(instance)
    }));
    // Wire per-instance connection resolver from config trait.
    let config_clone = Arc::clone(&config);
    browser_state.set_instance_resolver_fn(Box::new(move |instance| config_clone.resolve_instance(instance)));
    let state = SharedState::new(browser_state);

    // Scripting engine + sandbox. The sandbox needs an existing canonical
    // directory; we create the configured root up front and log (not panic)
    // if initialisation fails so the rest of the server still works.
    let script_engine = Arc::new(ferridriver_script::ScriptEngine::new(config.script_engine_config()));
    let script_root = config.script_root();
    let script_sandbox = match std::fs::create_dir_all(&script_root)
      .map_err(|e| format!("{e}"))
      .and_then(|()| ferridriver_script::PathSandbox::new(&script_root).map_err(|e| e.message.clone()))
    {
      Ok(sb) => Some(Arc::new(sb)),
      Err(msg) => {
        tracing::warn!(
          script_root = %script_root.display(),
          error = %msg,
          "scripting disabled: failed to prepare script_root; run_script will return an error"
        );
        None
      },
    };

    // Artifacts sandbox — separate directory for script outputs. If it
    // fails to prepare we log and disable the `artifacts` global only;
    // `run_script` itself keeps working.
    let artifacts_root = config.artifacts_root();
    let artifacts_sandbox = match std::fs::create_dir_all(&artifacts_root)
      .map_err(|e| format!("{e}"))
      .and_then(|()| ferridriver_script::PathSandbox::new(&artifacts_root).map_err(|e| e.message.clone()))
    {
      Ok(sb) => Some(Arc::new(sb)),
      Err(msg) => {
        tracing::warn!(
          artifacts_root = %artifacts_root.display(),
          error = %msg,
          "artifacts binding disabled: failed to prepare artifacts_root; scripts can still write via fs into script_root"
        );
        None
      },
    };

    Self {
      state,
      tool_router: Self::tool_router(),
      config,
      extensions: Arc::new(NoExtensions),
      script_engine,
      script_sandbox,
      artifacts_sandbox,
      session_vars: Arc::new(DashMap::new()),
      session_vms: Arc::new(DashMap::new()),
      plugins: crate::plugin::PluginRegistry::default(),
    }
  }

  /// Discover and load every plugin configured via [`McpServerConfig::plugin_paths`].
  ///
  /// Failed plugins are logged and skipped -- one broken file should not
  /// prevent the server from starting. Successfully loaded plugins are
  /// stored in `self.plugins` and become available as `run_script` bindings
  /// (and, when promoted, as MCP tools) on the next invocation.
  pub async fn load_plugins(&mut self) {
    let paths = self.config.plugin_paths();
    if paths.is_empty() {
      return;
    }

    // Discover every file across all configured roots, then bundle +
    // compile + extract the whole set in ONE batch runtime (rolldown ->
    // QuickJS bytecode; TypeScript and plugin-local imports resolved).
    let mut files = Vec::new();
    for root in paths {
      match crate::plugin::discover(&root) {
        Ok(v) => files.extend(v),
        Err(e) => tracing::warn!(path = %root.display(), error = %e, "plugin discovery failed; skipping path"),
      }
    }
    if files.is_empty() {
      return;
    }

    let (loaded, errors) = crate::plugin::load_all(&files).await;
    for e in errors {
      tracing::warn!(error = %e, "plugin load failed; skipping");
    }
    for lp in &loaded {
      let tool_names: Vec<&str> = lp.tools.iter().map(|t| t.name.as_str()).collect();
      tracing::info!(path = %lp.path.display(), tools = ?tool_names, "loaded plugin file");
    }

    self.plugins = crate::plugin::PluginRegistry::new(loaded);
    self.promote_plugins();
  }

  /// Register a dynamic tool route for each plugin manifest that declares
  /// `exposeAsTool: true`. The tool's name, description, and `inputSchema`
  /// come from the manifest. The dispatcher synthesises a one-line script
  /// that awaits the matching binding (`await plugins['<name>'](args[0])`)
  /// so the tool path and the `run_script` binding path share one handler.
  fn promote_plugins(&mut self) {
    use rmcp::handler::server::router::tool::ToolRoute;
    use rmcp::model::Tool;

    let promoted: Vec<_> = self
      .plugins
      .promoted_tools()
      .map(|t| {
        let name = t.name.clone();
        let desc = t.description.clone().unwrap_or_default();
        let schema_value = t
          .input_schema
          .clone()
          .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));
        let schema_obj = match schema_value {
          serde_json::Value::Object(m) => m,
          _ => serde_json::Map::new(),
        };
        (name, desc, Arc::new(schema_obj))
      })
      .collect();

    for (name, desc, schema_obj) in promoted {
      let tool = Tool::new(name.clone(), desc, schema_obj);
      let plugin_name = name.clone();

      let route = ToolRoute::<Self>::new_dyn(tool, move |ctx| {
        let plugin_name = plugin_name.clone();
        Box::pin(async move {
          let args_obj = ctx.arguments.clone().unwrap_or_default();
          let args_value = serde_json::Value::Object(args_obj);
          ctx.service.invoke_plugin(&plugin_name, args_value).await
        })
      });
      self.tool_router.add_route(route);
      tracing::info!(name = %name, "promoted plugin to MCP tool");
    }
  }

  /// Invoke a plugin by manifest name with the given argument object.
  /// Backs both the `exposeAsTool` registration and any direct caller
  /// that wants to dispatch a plugin without writing JS by hand.
  ///
  /// `args_obj` is wrapped into a single positional `args[0]` for the
  /// underlying script run. The plugin's `session` argument (if present)
  /// is honoured for browser context selection.
  ///
  /// # Errors
  ///
  /// Returns an [`ErrorData`] if the plugin name is unknown, scripting
  /// is disabled (no usable script root), the underlying browser
  /// session cannot be established, or the final result fails to
  /// serialise.
  pub async fn invoke_plugin(
    &self,
    plugin_name: &str,
    args_obj: serde_json::Value,
  ) -> Result<rmcp::model::CallToolResult, ErrorData> {
    use rmcp::model::{CallToolResult, Content};

    if self.plugins.get_tool(plugin_name).is_none() {
      return Err(Self::err(format!("unknown plugin: {plugin_name}")));
    }

    let session = args_obj
      .get("session")
      .and_then(|v| v.as_str())
      .map_or_else(|| "default".into(), str::to_string);
    // Serialize per-session tool calls so concurrent run_script and plugin
    // invocations on the same session don't race against each other's
    // browser state (cookies, navigation, page identity). Matches the
    // pattern other tool routers use.
    let _guard = self.session_guard(&session).await;

    let Some(sandbox) = self.script_sandbox.clone() else {
      return Err(Self::err(
        "scripting is disabled: the configured script_root could not be prepared at server startup.",
      ));
    };

    let vars = self.session_vars(&session);

    // Resolve live browser handles -- same path the run_script tool uses.
    let (page, ctx_ref) = Box::pin(self.page_and_context(&session)).await?;
    let request = Arc::new(ferridriver::api_request::APIRequestContext::new(
      ferridriver::api_request::RequestContextOptions::default(),
    ));
    let browser_handle = Arc::new(ferridriver::Browser::from_shared_state(self.state.state_arc()));

    let context = ferridriver_script::RunContext {
      vars,
      sandbox,
      artifacts: self.artifacts_sandbox.clone(),
      page: Some(page),
      browser_context: Some(Arc::new(ctx_ref)),
      request: Some(request),
      browser: Some(browser_handle),
      plugins: self.plugin_bindings(),
      trusted_modules: false,
    };

    let name_literal = serde_json::to_string(plugin_name).unwrap_or_else(|_| "\"\"".into());
    let source = format!("return await plugins[{name_literal}](args[0]);");
    let args = vec![args_obj];

    let result = self
      .run_in_session(
        &session,
        &source,
        &args,
        ferridriver_script::RunOptions::default(),
        context,
      )
      .await;

    let json = serde_json::to_string_pretty(&result).map_err(|e| Self::err(format!("serialize result: {e}")))?;
    let mut contents = vec![Content::text(json)];
    if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
      let summary = format!("[{:?}] {} ({}ms)", error.kind, error.message, result.duration_ms);
      contents.insert(0, Content::text(summary));
    }
    Ok(CallToolResult::success(contents))
  }

  /// Get-or-create the `InMemoryVars` store for a given session name.
  ///
  /// Called from `run_script` so each session sees a stable vars namespace
  /// across tool invocations (matching the "fresh context per call, but
  /// session-level vars persist" design choice).
  pub(crate) fn session_vars(&self, session: &str) -> Arc<ferridriver_script::InMemoryVars> {
    self
      .session_vars
      .entry(session.to_string())
      .or_insert_with(|| Arc::new(ferridriver_script::InMemoryVars::new()))
      .clone()
  }

  /// Snapshot the loaded plugin registry into the script-engine binding
  /// shape. Shared by `run_script` and `invoke_plugin` so the mapping
  /// lives in exactly one place.
  pub(crate) fn plugin_bindings(&self) -> Vec<ferridriver_script::PluginBinding> {
    self
      .plugins
      .files()
      .iter()
      .map(|f| ferridriver_script::PluginBinding {
        bytecode: f.bytecode.clone(),
        tools: f
          .tools
          .iter()
          .map(|t| ferridriver_script::PluginToolBinding {
            name: t.name.clone(),
            allowed_commands: t.allow.commands.clone(),
            allowed_net: t.allow.net.clone(),
          })
          .collect(),
      })
      .collect()
  }

  /// Get-or-create the persistent VM slot for a session. Mirrors
  /// `session_vars` keying. Enforces the warm-VM cap by evicting the
  /// least-recently-used idle session before admitting a new one.
  fn session_slot(&self, session: &str) -> Arc<Mutex<SessionSlot>> {
    if !self.session_vms.contains_key(session) && self.session_vms.len() >= self.script_engine.config().max_session_vms
    {
      self.evict_lru_session();
    }
    self
      .session_vms
      .entry(session.to_string())
      .or_insert_with(|| Arc::new(Mutex::new(SessionSlot::default())))
      .clone()
  }

  /// Evict the oldest session whose slot is not currently in use. A
  /// locked slot means an execution is in flight on another session, so
  /// it is skipped (the cap is soft — never drop a VM mid-run).
  fn evict_lru_session(&self) {
    let mut victim: Option<(String, Instant)> = None;
    for entry in self.session_vms.iter() {
      if let Ok(slot) = entry.value().try_lock() {
        let t = slot.last_used;
        if victim.as_ref().is_none_or(|(_, oldest)| t < *oldest) {
          victim = Some((entry.key().clone(), t));
        }
      }
    }
    if let Some((key, _)) = victim {
      self.session_vms.remove(&key);
    }
  }

  /// Execute a script against the session's persistent VM, creating it
  /// on first use and transparently rebuilding it after a poisoning
  /// fault. The caller must already hold the per-session guard so slot
  /// access is uncontended.
  pub(crate) async fn run_in_session(
    &self,
    session: &str,
    source: &str,
    args: &[serde_json::Value],
    options: ferridriver_script::RunOptions,
    context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = self.session_slot(session);
    let mut slot = slot.lock().await;

    if slot.vm.is_none() {
      match ferridriver_script::Session::create(self.script_engine.config().clone(), &context).await {
        Ok(vm) => slot.vm = Some(vm),
        Err(e) => return ferridriver_script::ScriptResult::err(e, 0, Vec::new()),
      }
    }

    let run = match slot.vm.as_ref() {
      Some(vm) => vm.execute(source, args, options, &context).await,
      // Unreachable: we just ensured `vm` is `Some`. Defensive (no
      // `unwrap`/`expect`) rather than a panic path.
      None => {
        return ferridriver_script::ScriptResult::err(
          ferridriver_script::ScriptError::internal("session vm unexpectedly absent"),
          0,
          Vec::new(),
        );
      },
    };

    // A poisoning fault (timeout interrupt / OOM) left the VM in an
    // untrustworthy state — discard it so the NEXT call rebuilds fresh.
    // The poisoning call still returns its own error result.
    if run.poisoned {
      slot.vm = None;
    }
    slot.last_used = Instant::now();
    run.result
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

  pub fn err(msg: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(msg.to_string(), None)
  }

  /// Build the JSON snapshot returned by the `network` MCP resource.
  /// Extracted from `read_resource` because async lock + per-request
  /// snapshotting pushed that handler over the line-count threshold.
  async fn read_network_resource(&self, context_name: &str, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let handles = self
      .state
      .log_handles_for(context_name)
      .await
      .ok_or_else(|| Self::err(format!("Context '{context_name}' not found")))?;
    let reqs = handles.network.read().await;
    let last: Vec<_> = reqs
      .iter()
      .rev()
      .take(100)
      .cloned()
      .collect::<Vec<_>>()
      .into_iter()
      .rev()
      .collect();
    drop(reqs);
    let mut snapshots = Vec::with_capacity(last.len());
    for r in &last {
      snapshots.push(r.to_diagnostic_json().await);
    }
    let text = serde_json::to_string_pretty(&snapshots).unwrap_or_default();
    Ok(ReadResourceResult::new(vec![
      ResourceContents::text(text, uri.to_string()).with_mime_type("application/json"),
    ]))
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

  /// Ensure a browser instance exists for the context and return its active `AnyPage`.
  ///
  /// Fast path (instance exists): shared read lock -- concurrent reads allowed.
  /// Slow path (cold start): exclusive write lock -- only when launching a new browser.
  async fn ensure_active_page(&self, context: &str) -> Result<AnyPage, ErrorData> {
    {
      let state = self.state.read().await;
      if let Ok(any_page) = state.active_page(context) {
        return Ok(any_page.clone());
      }
    }
    let key = ferridriver::state::SessionKey::parse(context);
    let mut state = self.state.write().await;
    Box::pin(state.ensure_instance(&key.instance))
      .await
      .map_err(Self::err)?;
    if state.active_page(context).is_err() {
      Box::pin(state.open_page_keyed(&key, "about:blank"))
        .await
        .map_err(Self::err)?;
    }
    state.active_page(context).map_err(Self::err).cloned()
  }

  /// Get a `Page` for a context, ensuring the required browser instance exists.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn page(&self, context: &str) -> Result<Arc<Page>, ErrorData> {
    let any_page = Box::pin(self.ensure_active_page(context)).await?;
    // `Page::new` spawns the FrameAttached/Navigated/Detached listener
    // and is sync after the eager `Page.getFrameTree` RTT was dropped
    // (see `PERF_AUDIT` §M.4).
    Ok(Page::new(any_page))
  }

  /// Get raw `AnyPage` (for low-level ops that Page doesn't cover yet).
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn raw_page(&self, context: &str) -> Result<AnyPage, ErrorData> {
    Box::pin(self.ensure_active_page(context)).await
  }

  /// Get a `Page` and `ContextRef` for a session in a single operation.
  ///
  /// This is the primary entry point for BDD integration -- provides both
  /// the page (for DOM interaction) and the context handle (for cookies,
  /// permissions, etc.) on the same live MCP session.  A single
  /// `ensure_active_page` call handles both, avoiding redundant lock
  /// acquisitions.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or accessed.
  pub async fn page_and_context(
    &self,
    context: &str,
  ) -> Result<(Arc<Page>, ferridriver::context::ContextRef), ErrorData> {
    let any_page = Box::pin(self.ensure_active_page(context)).await?;
    let page = Page::new(any_page);
    let ctx_ref = ferridriver::context::ContextRef::new(self.state.state_arc(), context.to_string());
    Ok((page, ctx_ref))
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
  ) -> ferridriver::Result<AnyElement> {
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

#[tool_handler(router = self.tool_router)]
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
        let url = page.url();
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
        let last: Vec<serde_json::Value> = msgs
          .iter()
          .rev()
          .take(100)
          .map(|m| {
            serde_json::json!({
              "type": m.type_str(),
              "text": m.text(),
            })
          })
          .collect::<Vec<_>>()
          .into_iter()
          .rev()
          .collect();
        drop(msgs);
        let text = serde_json::to_string_pretty(&last).unwrap_or_default();
        Ok(ReadResourceResult::new(vec![
          ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
      },
      "network" => self.read_network_resource(&context_name, uri).await,
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
