//! `McpServer` server struct and shared helpers used by all tools.

use arc_swap::ArcSwap;
use base64::Engine;
use dashmap::DashMap;
use ferridriver::Page;
use ferridriver::actions;
use ferridriver::backend::BackendKind;
use ferridriver::backend::{AnyElement, AnyPage};
use ferridriver::state::{BrowserState, ConnectMode, ContextLogHandles};
use rmcp::{
  ErrorData, RoleServer, ServerHandler,
  handler::server::router::tool::ToolRouter,
  model::{
    CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse, GetPromptResult,
    ListPromptsResult, ListResourcesResult, PaginatedRequestParams, Prompt, PromptArgument, PromptMessage,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role,
    ServerCapabilities, ServerInfo,
  },
  service::RequestContext,
  tool_handler,
};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::Instrument;

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

  /// Current generation of the browser instance backing `context`'s
  /// session, or `None` if no such instance is live. Used to detect a
  /// browser-session swap (relaunch/reconnect) so a stale script VM is
  /// discarded rather than left holding handles into a dead session.
  pub(crate) async fn instance_generation(&self, context: &str) -> Option<u64> {
    let state = self.inner.read().await;
    let key = state.session_key(context);
    state.instance_generation(key.instance.as_ref())
  }

  /// Parse a session key against the state's configured instance names.
  pub(crate) async fn session_key(&self, raw: &str) -> ferridriver::state::SessionKey {
    self.inner.read().await.session_key(raw)
  }

  /// The configured instance names, for a caller that must parse several
  /// keys without retaking the lock each time.
  pub(crate) async fn known_instances(&self) -> ferridriver::state::KnownInstances {
    self.inner.read().await.known_instances()
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
  ///
  /// The per-context serialization lock deliberately survives: dropping
  /// it while a tool call holds the guard means the next caller mints a
  /// fresh mutex and runs concurrently with the guard holder, which is
  /// exactly what the lock exists to prevent. Cold-starting a context
  /// invalidates from inside such a guard, so this was reachable. The
  /// entries are one small `Arc` per distinct session name.
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

  /// Ceiling on the total size of [`Self::artifacts_root`], in bytes.
  ///
  /// A server that stays up for days accumulates screenshots and traces
  /// from calls whose results were read and forgotten. When set, each call
  /// that writes an artifact sweeps the directory back under the ceiling by
  /// deleting least-recently-modified files — never the ones it just wrote.
  ///
  /// Default: no ceiling.
  fn artifacts_max_bytes(&self) -> Option<u64> {
    None
  }

  /// Values that must not reach a caller verbatim, as `name -> value`.
  ///
  /// Every response the server renders — a returned value, a console line, a
  /// page URL, echoed code — has these replaced by `<secret>NAME</secret>`,
  /// and echoed code reads them from the environment instead. A convenience
  /// rather than a security boundary: only declared values are matched.
  ///
  /// Default: none, so nothing is redacted.
  fn secrets(&self) -> ferridriver::response::Secrets {
    ferridriver::response::Secrets::default()
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

  /// Launch settings for a specific browser instance: extra arguments,
  /// profile directory, executable, headless, backend and environment.
  ///
  /// Called before launching a browser for the given instance name,
  /// which comes from the composite session key
  /// `"<instance>:<context>"`. Override to inject per-instance DNS
  /// resolver rules, a persistent profile, or an environment.
  ///
  /// # Errors
  ///
  /// Return `Err` to ABORT the launch (e.g. the instance name maps to no
  /// environment). Launching anyway would put the caller on a browser
  /// that looks configured and is not.
  fn instance_overrides(&self, _instance: &str) -> Result<ferridriver::options::InstanceOverrides, String> {
    Ok(ferridriver::options::InstanceOverrides::default())
  }

  /// Instance names this config defines, so a bare session key that
  /// names one selects that instance instead of being read as a context
  /// on `default`.
  fn instance_names(&self) -> Vec<String> {
    Vec::new()
  }

  /// Section-level launch settings, resolved WITHOUT running any
  /// operator command.
  ///
  /// Read once while the server is being constructed, which is a
  /// synchronous path on the async runtime — [`Self::instance_overrides`]
  /// may shell out for up to its command timeout and would stall the
  /// reactor before the server ever serves.
  fn base_overrides(&self) -> ferridriver::options::InstanceOverrides {
    ferridriver::options::InstanceOverrides::default()
  }

  /// Default viewport for new pages.
  fn default_viewport(&self) -> Option<ferridriver::options::ViewportConfig> {
    None
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

  /// Validate that an instance can be started, before launching a browser.
  ///
  /// Called on the cold-start path with the instance parsed from the session
  /// key. Return `Err(message)` to abort with an actionable error instead of
  /// launching (e.g. the session key resolved to a bogus instance). Default
  /// allows everything.
  ///
  /// # Errors
  ///
  /// Returns `Err` with a user-facing message when `instance` must not be
  /// launched (surfaced as the tool error).
  fn instance_health(&self, _instance: &str) -> Result<(), String> {
    Ok(())
  }

  /// Server name for MCP `get_info`.
  fn server_name(&self) -> &str {
    DEFAULT_SERVER_NAME
  }

  /// Server instructions for MCP `get_info`.
  fn server_instructions(&self) -> &str {
    DEFAULT_INSTRUCTIONS
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
for outputs), no runner-side network except via `request.*` (HttpClient), no \
`process` / `require` / bare `import`. Caller-controlled data MUST be passed via the \
`args` array, never interpolated into the `source` string — the engine does not protect \
against source-level injection.";

/// Default config for standalone ferridriver (no customization).
pub struct DefaultConfig;
impl McpServerConfig for DefaultConfig {}

/// The loaded extension set: the registry AND the tool list built from
/// it, published together.
///
/// One `ArcSwap`, not two. Held separately, a `call_tool` landing between
/// the two stores saw the new registry with the previous listing (or the
/// reverse), so a reload could advertise a tool the registry no longer
/// had — which is exactly the window a reload exists to close.
#[derive(Default)]
pub(crate) struct LoadedExtensions {
  pub registry: crate::extension::ExtensionRegistry,
  /// The `tools/list` entries, kept beside the static tool router rather
  /// than inside it: `ToolRouter` routes are fixed once the server is
  /// serving, and an authoring loop needs the promoted set to change
  /// without a restart. Dispatch for these names goes straight to
  /// [`McpServer::invoke_extension_tool`], the same path the router's
  /// dynamic route used.
  pub promoted: Vec<rmcp::model::Tool>,
}

// ── McpServer ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct McpServer {
  pub(crate) state: SharedState,
  /// One cached `Arc<Page>` wrapper per context, validated against the
  /// currently-active backend page on every lookup. Tool handlers used
  /// to mint a fresh wrapper per call, which silently reset all
  /// wrapper-level state (default timeouts, `emulateMedia` merge state)
  /// between MCP tool calls. The cache keeps a context's wrapper alive
  /// until its underlying browser page changes (new page / relaunch).
  page_wrappers: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, Arc<Page>>>>,
  /// The composed tool router. Public so consumers can list tools or dispatch directly.
  pub tool_router: ToolRouter<Self>,
  /// Configuration trait object for customizing server behavior.
  pub config: Arc<dyn McpServerConfig>,
  /// Typed slot for consumer-specific state (e.g. Jira clients),
  /// attached via [`Self::with_extension`]. Unrelated to the extension
  /// (extension) system — this is host-embedding state.
  custom_ext: Arc<dyn std::any::Any + Send + Sync>,
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
  /// Ceiling on the artifacts root, swept after each call that writes one.
  /// `None` lets the directory grow without bound.
  pub(crate) artifacts_budget: Option<ferridriver::response::OutputBudget>,
  /// Resolved once at construction: reading a dotenv file per tool call
  /// would put a filesystem read on every response's hot path.
  pub(crate) secrets: ferridriver::response::Secrets,
  /// All live script sessions: one persistent `QuickJS` VM + its
  /// session-scoped `vars` + the browser generation it was built
  /// against, per session name, behind one lock each. Shared by
  /// `run_script` and extension calls so `globalThis`/`vars` persist
  /// REPL-style; a browser relaunch under the same name discards the VM
  /// (stale handles) but keeps `vars`.
  pub(crate) sessions: Arc<ferridriver_script::SessionTable>,
  /// Resolved scripting sandbox relaxations (env allow-list / node
  /// compat). Default = locked down; set by [`McpServer::with_script_caps`]
  /// from the operator's `[scripting]` config.
  pub(crate) script_caps: ferridriver_script::ScriptCaps,
  /// Extensions discovered + parsed at startup, swappable so a reload
  /// can replace the whole set from a `&self` tool handler. Empty by
  /// default; populated by [`McpServer::load_extensions`] and replaced by
  /// [`McpServer::reload_extensions`].
  pub(crate) extension_registry: Arc<arc_swap::ArcSwap<LoadedExtensions>>,
  /// Serialises reloads. Two concurrent `action: "reload"` calls would
  /// otherwise both bundle and both publish, and the loser's set could
  /// land last — advertising tools built from a source tree nobody asked
  /// for.
  reload_lock: Arc<tokio::sync::Mutex<()>>,
  /// Resolved `[test]` config (feature/step globs, browser, workers,
  /// retries, reporters, ...). Default = `TestConfig::default()`; set by
  /// [`McpServer::with_test_config`] from the operator's `ferridriver.toml`.
  /// Used as the base config for the `run_bdd` tool so an MCP-driven BDD
  /// run inherits the same `[test]` settings as the `ferridriver bdd` CLI.
  pub(crate) test_config: ferridriver_test::config::TestConfig,
  /// Top-level `extensions` specs (files/dirs/packages) recorded at
  /// startup by [`McpServer::load_extensions`]. The `run_bdd` tool bundles
  /// these alongside step globs so one extension serves MCP tools AND BDD
  /// step definitions, exactly like the CLI.
  pub(crate) extension_specs: Vec<ferridriver_config::ExtensionSpec>,
  /// The single cached BDD step engine for the `run_bdd` JS path — loaded
  /// once, reused across calls and browser sessions, reloaded on a step-set
  /// or source change. The lock is the build+run guard. See
  /// [`crate::bdd_engine`].
  pub(crate) bdd_engine: Arc<Mutex<crate::bdd_engine::BddEngine>>,
  /// Built-in BDD step registry, built once (immutable) and shared by
  /// every built-in-only `run_bdd` call. `StepRegistry::build()` walks
  /// `inventory` + compiles cucumber expressions, so rebuilding it per
  /// call is pure waste.
  pub(crate) builtin_steps: Arc<std::sync::OnceLock<Arc<ferridriver_bdd::registry::StepRegistry>>>,
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
      BackendKind::WebKit => ferridriver::options::BrowserKind::WebKit,
      _ => ferridriver::options::BrowserKind::Chromium,
    };
    // The base plan carries every launch setting the config expresses.
    // `executable_path` and `viewport` were config keys the server never
    // read, so both were silently inert.
    //
    // `args` stays EMPTY: `instance_overrides` returns the complete arg
    // set for an instance (section `chromeArgs` + proxy flags + the
    // instance's own + whatever the args command adds), and the launch
    // path concatenates the plan's args with the callback's. Passing
    // `chrome_args()` here too put every base flag in the command line
    // twice — invisible for last-wins switches, wrong for repeatable ones
    // like `--host-resolver-rules`.
    let base = config.base_overrides();
    let mut browser_state = BrowserState::with_plan(
      mode,
      ferridriver::options::LaunchPlan {
        backend,
        kind,
        headless,
        args: Vec::new(),
        executable_path: base.executable_path.clone(),
        user_data_dir: base.user_data_dir.clone(),
        default_viewport: config.default_viewport(),
        ..Default::default()
      },
    );

    // A bare session key that names a configured instance must select
    // that instance rather than a context on `default`.
    browser_state.set_known_instances(config.instance_names());

    // Wire per-instance launch settings from the config trait.
    let config_clone = Arc::clone(&config);
    browser_state.set_instance_overrides_fn(Arc::new(move |instance| config_clone.instance_overrides(instance)));
    // Wire per-instance connection resolver from config trait.
    let config_clone = Arc::clone(&config);
    browser_state.set_instance_resolver_fn(Arc::new(move |instance| config_clone.resolve_instance(instance)));
    let state = SharedState::new(browser_state);

    // Scripting engine + sandbox. The sandbox needs an existing canonical
    // directory; we create the configured root up front and log (not panic)
    // if initialisation fails so the rest of the server still works.
    let script_engine = Arc::new(ferridriver_script::ScriptEngine::new(config.script_engine_config()));
    let sessions = Arc::new(ferridriver_script::SessionTable::new(
      script_engine.config().max_session_vms,
      script_engine.config().session_idle_ttl,
    ));
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

    let artifacts_budget = config
      .artifacts_max_bytes()
      .map(ferridriver::response::OutputBudget::new);
    let secrets = config.secrets();

    Self {
      state,
      page_wrappers: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      tool_router: Self::tool_router(),
      config,
      custom_ext: Arc::new(NoExtensions),
      script_engine,
      script_sandbox,
      artifacts_sandbox,
      artifacts_budget,
      secrets,
      sessions,
      script_caps: ferridriver_script::ScriptCaps::default(),
      extension_registry: Arc::new(arc_swap::ArcSwap::from_pointee(LoadedExtensions::default())),
      reload_lock: Arc::new(tokio::sync::Mutex::new(())),
      test_config: ferridriver_test::config::TestConfig::default(),
      extension_specs: Vec::new(),
      bdd_engine: Arc::new(Mutex::new(crate::bdd_engine::BddEngine::new())),
      builtin_steps: Arc::new(std::sync::OnceLock::new()),
    }
  }

  /// Discover and load every configured extension file as MCP tools.
  /// `specs` come from the top-level `extensions` config: each is a
  /// source file, source directory, package directory, or ESM package
  /// specifier resolved from `node_modules`.
  ///
  /// Failed extensions are logged and skipped -- one broken file should
  /// not prevent the server from starting. Successfully loaded tools are
  /// stored in the extension registry and become available as `run_script`
  /// bindings (and, when `exposeAsMcpTool`, as MCP tools).
  pub async fn load_extensions(&mut self, specs: &[ferridriver_config::ExtensionSpec]) {
    // Record the specs so the BDD path can re-bundle them as step sources
    // even though run_script consumes them as already-loaded extensions.
    self.extension_specs = specs.to_vec();
    if specs.is_empty() {
      return;
    }
    self.load_extension_specs().await;
  }

  /// Re-run discovery + load for the specs recorded at startup, replace
  /// the registry and the promoted tool set, and discard every live
  /// session VM so open sessions pick the new bytecode up on their next
  /// call (their durable `vars` / processes / cookies survive).
  ///
  /// Editing an extension previously meant restarting the MCP client,
  /// which drops every browser session with it — the authoring loop cost
  /// far more than the edit.
  ///
  /// Returns `(tool_count, dropped_vm_count)`.
  pub async fn reload_extensions(&self) -> (usize, usize) {
    // Serialised: concurrent reloads would both bundle and both publish,
    // and whichever finished last would win regardless of which source
    // tree the caller meant.
    let _serial = self.reload_lock.lock().await;
    self.load_extension_specs().await;
    let dropped = self.sessions.drop_all_vms().await;
    let count = self.extensions().registry.tool_count();
    tracing::info!(tools = count, dropped_vms = dropped, "reloaded extensions");
    (count, dropped)
  }

  /// The currently-loaded extension set: registry and advertised tools,
  /// as one consistent snapshot.
  pub(crate) fn extensions(&self) -> Arc<LoadedExtensions> {
    self.extension_registry.load_full()
  }

  /// The base args the launch path starts from, so a test can assert the
  /// plan and the per-instance callback do not both contribute them.
  #[cfg(test)]
  pub(crate) fn launch_plan_args_for_test(&self) -> Vec<String> {
    self
      .state
      .inner
      .try_read()
      .map(|s| s.extra_args.clone())
      .unwrap_or_default()
  }

  /// Publish a registry directly, promoting its tools the same way a load
  /// would. Test-only: the real path always goes through
  /// [`Self::load_extension_specs`].
  #[cfg(test)]
  pub(crate) fn publish_extensions_for_test(&self, registry: crate::extension::ExtensionRegistry) {
    let promoted = self.promoted_tool_list(&registry);
    self
      .extension_registry
      .store(Arc::new(LoadedExtensions { registry, promoted }));
  }

  /// Replace the registry (and the promoted tool set) from
  /// `self.extension_specs`. Shared by startup and reload so the two can
  /// never resolve, gate or promote differently.
  async fn load_extension_specs(&self) {
    let specs = self.extension_specs.clone();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut policy_warnings: Vec<(String, String)> = Vec::new();

    // One resolve + gate for every host (`ferridriver_script::extension_load`):
    // a package states its preconditions in `package.json`
    // (`ferridriver.requires` / `ferridriver.settings`), and the gate is
    // what turns a missing binary, an unlisted `allowEnv` name, a host
    // outside the operator ceiling, an undeclared sidecar or a mistyped
    // settings key into one message naming the package and the config
    // key that fixes it — instead of a failure on the first call.
    let sidecar_names: Vec<String> = self
      .script_engine
      .config()
      .sidecars
      .iter()
      .map(|s| s.name.clone())
      .collect();
    let env = ferridriver_script::RequirementEnv::from_caps(&self.script_caps, &sidecar_names);
    let gated = ferridriver_script::gate(&specs, &env);
    for (spec, e) in &gated.resolve_errors {
      tracing::warn!(extension = %spec, error = %e.message, "extension discovery failed; skipping path");
      failures.push((spec.clone(), e.message.clone()));
    }
    for issue in &gated.issues {
      if issue.blocking {
        tracing::error!(source = %issue.source, "extension package requirement unmet: {}", issue.message);
        failures.push((issue.source.clone(), issue.message.clone()));
      } else {
        tracing::warn!(source = %issue.source, "{}", issue.message);
        policy_warnings.push((issue.source.clone(), issue.message.clone()));
      }
    }
    let files = gated.files.clone();

    let (loaded, errors) = if files.is_empty() {
      (Vec::new(), Vec::new())
    } else {
      crate::extension::load_all(&files, &self.script_caps.extension_policy).await
    };
    for e in errors {
      tracing::warn!(error = %e, "extension load failed; skipping");
      failures.push((e.source_label(), e.to_string()));
    }
    for lp in &loaded {
      let tool_names: Vec<&str> = lp.tools.iter().map(|t| t.name.as_str()).collect();
      tracing::info!(path = %lp.path.display(), tools = ?tool_names, "loaded extension file");
      if lp.tools.is_empty() {
        // Legitimate for an entry that only contributes BDD steps or
        // script-host globals — and the only signal when a `defineTool`
        // call did not run (a guard that was never true, a top-level
        // throw before it).
        let message = "declares no tools; loaded for its BDD steps / script-host contributions. \
                       If it was meant to register a tool, its defineTool(...) call never ran."
          .to_string();
        tracing::warn!(path = %lp.path.display(), "{message}");
        policy_warnings.push((lp.path.display().to_string(), message));
      }
    }

    let mut warnings = self.policy_conflicts(&loaded);
    for (source, message) in &warnings {
      tracing::warn!(source = %source, "{message}");
    }
    warnings.append(&mut policy_warnings);
    let registry = crate::extension::ExtensionRegistry::with_warnings(loaded, failures, warnings);
    let promoted = self.promoted_tool_list(&registry);
    // Registry and listing published in ONE store, so no call can observe
    // a listing that disagrees with the registry behind it.
    self
      .extension_registry
      .store(Arc::new(LoadedExtensions { registry, promoted }));
  }

  /// Lint every loaded manifest against the operator extension policy
  /// (`[extensions.policy]`, threaded in via [`Self::with_script_caps`],
  /// so call that first). Enforcement happens at session registration
  /// (`defineTool` clamps `allow.net`; a command-ceiling violation fails
  /// the tool); this pass makes each conflict visible at startup and
  /// through `ferridriver_extensions` instead of only at first dispatch.
  fn policy_conflicts(&self, loaded: &[crate::extension::LoadedExtension]) -> Vec<(String, String)> {
    use ferridriver_config::ExtensionCommandsCeiling as Ceiling;
    let policy = &self.script_caps.extension_policy;
    let mut warnings = Vec::new();
    for lp in loaded {
      let source = lp.path.display().to_string();
      for t in &lp.tools {
        if let Some(ceiling) = policy.net.as_deref() {
          let dropped: Vec<&str> = t
            .allow
            .net
            .iter()
            .map(String::as_str)
            .filter(|d| !ferridriver_script::net_entry_subsumed(d, ceiling))
            .collect();
          if !dropped.is_empty() {
            warnings.push((
              source.clone(),
              format!(
                "tool `{}`: allow.net entries {dropped:?} are outside the operator ceiling \
                 ([extensions.policy] net) and will be dropped from the effective grant",
                t.name
              ),
            ));
          }
        }
        let shell_form: Vec<&str> = t
          .allow
          .commands
          .iter()
          .filter(|(_, spec)| matches!(spec.run, ferridriver_script::CommandRun::Shell(_)))
          .map(|(name, _)| name.as_str())
          .collect();
        match policy.commands {
          Ceiling::ArgvOnly if !shell_form.is_empty() => warnings.push((
            source.clone(),
            format!(
              "tool `{}`: commands {shell_form:?} are shell-string specs, but the operator policy \
               permits only argv-array specs (`commands = \"argvOnly\"`) — the tool will fail to register",
              t.name
            ),
          )),
          Ceiling::None if !t.allow.commands.is_empty() => warnings.push((
            source.clone(),
            format!(
              "tool `{}` declares allow.commands, but the operator policy forbids command \
               declarations (`commands = \"none\"`) — the tool will fail to register",
              t.name
            ),
          )),
          Ceiling::Any | Ceiling::ArgvOnly | Ceiling::None => {},
        }
      }
    }
    warnings
  }

  /// The `tools/list` entries for every extension manifest that declares
  /// `exposeAsMcpTool: true`. Name, description and schemas come from the
  /// manifest; dispatch goes to [`Self::invoke_extension_tool`], so the
  /// tool path and the `run_script` binding path share one handler.
  ///
  /// Built as a plain list (not `ToolRouter` routes) because the set has
  /// to be replaceable while the server is serving — see
  /// [`LoadedExtensions::promoted`].
  fn promoted_tool_list(&self, registry: &crate::extension::ExtensionRegistry) -> Vec<rmcp::model::Tool> {
    use rmcp::model::Tool;

    let promoted: Vec<_> = registry
      .promoted_tools()
      .map(|t| {
        let name = t.name.clone();
        // Some MCP clients enforce tool-name patterns (commonly
        // `[a-zA-Z0-9_-]`); a dotted namespace or exotic character may
        // be rejected client-side even though the server accepts it.
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
          tracing::warn!(
            name = %name,
            "promoted tool name contains characters outside [a-zA-Z0-9_-]; some MCP clients reject such names"
          );
        }
        let as_schema_obj = |schema: Option<serde_json::Value>, label: &str| {
          let schema = schema?;
          if let serde_json::Value::Object(m) = schema {
            Some(Arc::new(m))
          } else {
            // tools/list would advertise nothing while invocation
            // validates against the real (non-object) schema — surface
            // the divergence to the author instead of leaving it silent.
            tracing::warn!(
              name = %name,
              "{label} is not a JSON object; omitting it from tools/list (calls still validate against the declared one)"
            );
            None
          }
        };
        let input_schema = as_schema_obj(t.input_schema.clone(), "inputSchema")
          .unwrap_or_else(|| Arc::new(serde_json::Map::new()));
        let output_schema = as_schema_obj(t.output_schema.clone(), "outputSchema");
        let mut tool = Tool::new(name.clone(), t.description.clone().unwrap_or_default(), input_schema);
        tool.title.clone_from(&t.title);
        tool.output_schema = output_schema;
        tool.annotations.clone_from(&t.annotations);
        tool
      })
      .collect();

    let mut out: Vec<Tool> = Vec::with_capacity(promoted.len());
    for tool in promoted {
      let name = tool.name.to_string();
      // `register_tool` already rejects duplicate names within a load
      // batch; this guards the remaining collision: an extension name
      // that shadows a built-in tool. Skip + warn rather than shadowing
      // a built-in, which would silently change what `navigate` means.
      if self.tool_router.has_route(&name) {
        tracing::warn!(name = %name, "extension tool name collides with a built-in tool; not promoting");
        continue;
      }
      if out.iter().any(|t| t.name == tool.name) {
        tracing::warn!(name = %name, "duplicate promoted extension tool name; keeping the first");
        continue;
      }
      tracing::info!(name = %name, "promoted extension to MCP tool");
      out.push(tool);
    }
    out
  }

  /// The promoted extension tool advertised under `name`, if any.
  fn promoted_extension_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
    self.extensions().promoted.iter().find(|t| t.name == name).cloned()
  }

  /// Names of the currently-advertised extension tools, in list order.
  pub(crate) fn promoted_tool_names(&self) -> Vec<String> {
    self.extensions().promoted.iter().map(|t| t.name.to_string()).collect()
  }

  /// Invoke a extension by manifest name with the given argument object.
  /// Backs both the `exposeAsMcpTool` registration and any direct caller
  /// that wants to dispatch a extension without writing JS by hand.
  ///
  /// `args_obj` is wrapped into a single positional `args[0]` for the
  /// underlying script run. The extension's `session` argument (if present)
  /// is honoured for browser context selection.
  ///
  /// # Errors
  ///
  /// Returns an [`ErrorData`] if the extension name is unknown, scripting
  /// is disabled (no usable script root), the underlying browser
  /// session cannot be established, or the final result fails to
  /// serialise.
  pub async fn invoke_extension_tool(
    &self,
    tool_name: &str,
    args_obj: serde_json::Value,
  ) -> Result<rmcp::model::CallToolResult, ErrorData> {
    use rmcp::model::CallToolResult;

    let loaded = self.extensions();
    let registry = &loaded.registry;
    if registry.get_tool(tool_name).is_none() {
      return Err(Self::err(format!("unknown extension: {tool_name}")));
    }
    // Enforce the declared inputSchema before doing any work (browser
    // launch, session lock). A non-conforming call is the caller's bug,
    // surfaced as a tool error so the model can correct and retry. The
    // validator was compiled once at load ([`crate::extension::ExtensionRegistry::new`]);
    // an invalid schema is the extension author's bug and is surfaced
    // loudly rather than silently skipped.
    if let Some(compiled) = registry.validator(tool_name) {
      match compiled {
        Err(msg) => return Ok(CallToolResult::error(vec![self.text(msg.clone())])),
        Ok(validator) => {
          // `session` is the reserved routing key (browser-session
          // selection), not part of the tool's declared contract —
          // validate against the args without it so a strict schema
          // (`additionalProperties: false`) does not reject session
          // routing. The handler still receives the full object.
          let validate_target = match &args_obj {
            serde_json::Value::Object(m) if m.contains_key("session") => {
              let mut m = m.clone();
              m.remove("session");
              std::borrow::Cow::Owned(serde_json::Value::Object(m))
            },
            other => std::borrow::Cow::Borrowed(other),
          };
          if let Err(msg) = validate_tool_args(tool_name, validator, &validate_target) {
            return Ok(CallToolResult::error(vec![self.text(msg)]));
          }
        },
      }
    }

    let session = args_obj
      .get("session")
      .and_then(|v| v.as_str())
      .map_or_else(|| "default".into(), str::to_string);
    // Serialize per-session tool calls so concurrent run_script and extension
    // invocations on the same session don't race against each other's
    // browser state (cookies, navigation, page identity). Matches the
    // pattern other tool routers use.
    let guard = self.session_guard(&session).await;
    let context = self.mcp_run_context(&session).await?;

    let result = self
      .run_tool_on_session_vm(
        &session,
        &guard,
        tool_name,
        args_obj,
        ferridriver_script::RunOptions::default(),
        context,
      )
      .await;

    self.extension_tool_result(tool_name, &result)
  }

  /// Shape a finished extension run into the MCP tool reply.
  ///
  /// A handler failure surfaces as an MCP error result (`is_error`) so
  /// the model can distinguish it from success, not a "success"
  /// carrying an error blob (this deliberately differs from
  /// `run_script`, whose contract is "always succeed, inspect
  /// `status`"). On success, a declared `outputSchema` is the symmetric
  /// half of the schema contract: the handler's return value must
  /// conform (a violation is the AUTHOR's bug, surfaced as a tool
  /// error) and ships as MCP `structuredContent` alongside the text
  /// payload.
  fn extension_tool_result(
    &self,
    tool_name: &str,
    result: &ferridriver_script::ScriptResult,
  ) -> Result<rmcp::model::CallToolResult, ErrorData> {
    use rmcp::model::CallToolResult;

    let json = serde_json::to_string_pretty(result).map_err(|e| Self::err(format!("serialize result: {e}")))?;
    let mut contents = vec![self.text(json)];
    let success = match &result.outcome {
      ferridriver_script::Outcome::Error { error } => {
        let summary = format!("[{:?}] {} ({}ms)", error.kind, error.message, result.duration_ms);
        contents.insert(0, self.text(summary));
        return Ok(CallToolResult::error(contents));
      },
      ferridriver_script::Outcome::Ok { success } => success,
    };
    let mut out = CallToolResult::success(contents);
    let loaded = self.extensions();
    let registry = &loaded.registry;
    if let Some(compiled) = registry.output_validator(tool_name) {
      match compiled {
        Err(msg) => return Ok(CallToolResult::error(vec![self.text(msg.clone())])),
        Ok(validator) => {
          if let Some(messages) = schema_violations(validator, &success.value) {
            return Ok(CallToolResult::error(vec![self.text(format!(
              "`{tool_name}` returned a value that does not match its declared outputSchema \
               (the extension author's bug):\n- {messages}"
            ))]));
          }
          out.structured_content = Some(success.value.clone());
        },
      }
    }
    Ok(out)
  }

  /// Snapshot the loaded extension registry into the script-engine binding
  /// shape. Shared by `run_script` and `invoke_extension_tool` so the mapping
  /// lives in exactly one place.
  pub(crate) fn extension_bindings(&self) -> Vec<ferridriver_script::ExtensionBinding> {
    let loaded = self.extensions();
    let registry = &loaded.registry;
    registry
      .files()
      .iter()
      .map(|f| ferridriver_script::ExtensionBinding {
        bytecode: f.bytecode.clone(),
        name: f.path.display().to_string(),
        source_map: f.source_map.clone(),
      })
      .collect()
  }

  /// Assemble the `RunContext` an MCP script/extension call needs: live
  /// page/context/request/browser handles for `session`, the script and
  /// artifacts sandboxes, and the loaded extension bytecode. Shared by
  /// `run_script` and `invoke_extension_tool` so the wiring lives in one place.
  ///
  /// `vars` is a throwaway here: a session's `vars` is the durable tier
  /// owned by the [`ferridriver_script::SessionTable`] entry (survives
  /// VM rebuild + cap eviction for the session's lifetime), so
  /// `run_on_session_vm` swaps in that store. The field stays required
  /// because the one-shot/CLI/BDD constructors legitimately supply their
  /// own; making it optional is a wider ripple, deliberately not done.
  pub(crate) async fn mcp_run_context(&self, session: &str) -> Result<ferridriver_script::RunContext, ErrorData> {
    let Some(sandbox) = self.script_sandbox.clone() else {
      return Err(Self::err(
        "scripting is disabled: the configured script_root could not be prepared at server startup.",
      ));
    };
    let (page, ctx_ref) = Box::pin(self.page_and_context(session)).await?;
    let browser = Arc::new(ferridriver::Browser::from_shared_state(self.state.state_arc()));
    Ok(ferridriver_script::RunContext {
      vars: Arc::new(ferridriver_script::InMemoryVars::new()),
      sandbox,
      artifacts: self.artifacts_sandbox.clone(),
      page: Some(page),
      browser_context: Some(Arc::new(ctx_ref)),
      // Placeholder like `vars`: the run_*_on_session_vm entry points
      // swap in the session slot's durable client (cookie jar +
      // connection pool live for the logical session, not one call).
      request: None,
      browser: Some(browser),
      extensions: self.extension_bindings(),
      host: ferridriver_script::ExtensionHost::Mcp,
      caps: self.script_caps.clone(),
      // Already resolved to `<instance>:<context>`, so the script host
      // never has to know this server's instance vocabulary to split it.
      session: Some(self.state.session_key(session).await.to_composite()),
    })
  }

  /// Run `source` on `session`'s persistent VM via the
  /// [`ferridriver_script::SessionTable`], which owns VM creation,
  /// warm-VM cap + idle-TTL reaping, browser-swap and poison rebuild.
  ///
  /// `_guard` is the per-context serialization lock, taken by reference
  /// purely to make "the caller already holds the context guard" a
  /// compile-time requirement instead of a comment.
  ///
  /// `context.vars` is replaced with the session's own durable store
  /// (vars belong to the session, not the call), and the browser
  /// instance generation is read so a relaunch under the same session
  /// name rebuilds the VM (its `globalThis` may hold dead page handles)
  /// while `vars` survive.
  pub(crate) async fn run_on_session_vm(
    &self,
    session: &str,
    _guard: &tokio::sync::OwnedMutexGuard<()>,
    source: &str,
    args: &[serde_json::Value],
    options: ferridriver_script::RunOptions,
    mut context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = self.sessions.acquire(session);
    let mut bs = slot.lock().await;
    context.vars = bs.vars();
    context.request = Some(bs.request());
    let epoch = self.state.instance_generation(session).await;
    bs.run(
      self.script_engine.config().clone(),
      source,
      args,
      options,
      context,
      epoch,
    )
    .await
  }

  /// Like [`run_on_session_vm`], but runs a precompiled bundled ES module
  /// (the TypeScript / `import` path) on the session VM. The run's result
  /// is the module's `default` export.
  pub(crate) async fn run_module_on_session_vm(
    &self,
    session: &str,
    _guard: &tokio::sync::OwnedMutexGuard<()>,
    bundle: &ferridriver_script::CompiledBundle,
    args: &[serde_json::Value],
    options: ferridriver_script::RunOptions,
    mut context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = self.sessions.acquire(session);
    let mut bs = slot.lock().await;
    context.vars = bs.vars();
    context.request = Some(bs.request());
    let epoch = self.state.instance_generation(session).await;
    bs.run_module(
      self.script_engine.config().clone(),
      bundle,
      args,
      options,
      context,
      epoch,
    )
    .await
  }

  /// Like [`Self::run_on_session_vm`], but natively invokes a registered
  /// extension tool by manifest name — no synthesized one-liner, no
  /// per-call compile. Backs `invoke_extension_tool` and the promoted-tool
  /// routes.
  pub(crate) async fn run_tool_on_session_vm(
    &self,
    session: &str,
    _guard: &tokio::sync::OwnedMutexGuard<()>,
    name: &str,
    tool_args: serde_json::Value,
    options: ferridriver_script::RunOptions,
    mut context: ferridriver_script::RunContext,
  ) -> ferridriver_script::ScriptResult {
    let slot = self.sessions.acquire(session);
    let mut bs = slot.lock().await;
    context.vars = bs.vars();
    context.request = Some(bs.request());
    let epoch = self.state.instance_generation(session).await;
    bs.run_tool(
      self.script_engine.config().clone(),
      name,
      tool_args,
      options,
      context,
      epoch,
    )
    .await
  }

  /// Add extra tool routers (merges with built-in browser tools).
  #[must_use]
  pub fn with_extra_tools(mut self, extra: ToolRouter<Self>) -> Self {
    self.tool_router += extra;
    self
  }

  /// Set the scripting sandbox relaxations (resolved from the
  /// operator's `[scripting]` config). Without this the sandbox stays
  /// fully locked down (`process.env` empty).
  #[must_use]
  pub fn with_script_caps(mut self, caps: ferridriver_script::ScriptCaps) -> Self {
    self.script_caps = caps;
    self
  }

  /// Set the base `[test]` config used by the `run_bdd` tool. Without this
  /// the tool falls back to `TestConfig::default()`. Pass the operator's
  /// resolved `FerridriverConfig::test` so MCP-driven BDD runs honour the
  /// same feature/step globs, browser, workers, and retries as the CLI.
  #[must_use]
  pub fn with_test_config(mut self, test_config: ferridriver_test::config::TestConfig) -> Self {
    self.test_config = test_config;
    self
  }

  /// The built-in BDD step registry, built once and shared. Immutable, so
  /// every built-in-only `run_bdd` reuses the same `Arc` instead of
  /// re-walking `inventory` + recompiling expressions per call.
  pub(crate) fn builtin_registry(&self) -> Arc<ferridriver_bdd::registry::StepRegistry> {
    Arc::clone(
      self
        .builtin_steps
        .get_or_init(|| Arc::new(ferridriver_bdd::registry::StepRegistry::build())),
    )
  }

  /// Declare the sidecar processes scripts may `sidecars.connect(name)`.
  /// Rebuilds the scripting engine with the specs merged into its config
  /// (the engine was constructed with `config.script_engine_config()`,
  /// which has no access to the top-level `[[sidecars]]` table). Connecting
  /// is by declared name only — a script can never spawn an arbitrary
  /// process.
  #[must_use]
  pub fn with_sidecars(mut self, sidecars: Vec<ferridriver_script::sidecar::SidecarSpec>) -> Self {
    let mut cfg = self.script_engine.config().clone();
    cfg.sidecars = sidecars;
    self.script_engine = Arc::new(ferridriver_script::ScriptEngine::new(cfg));
    self
  }

  /// Attach custom state accessible from tool handlers via `extension()`.
  #[must_use]
  pub fn with_extension<T: Send + Sync + 'static>(mut self, ext: Arc<T>) -> Self {
    self.custom_ext = ext;
    self
  }

  /// Access a typed extension stored on the server.
  #[must_use]
  pub fn extension<T: Send + Sync + 'static>(&self) -> Option<&T> {
    self.custom_ext.downcast_ref::<T>()
  }

  /// Build the error a tool handler returns when the operation it was
  /// asked to perform failed.
  ///
  /// Carries `INTERNAL_ERROR` only as an in-process marker: [`call_tool`]
  /// turns it into a `CallToolResult` with `isError: true` before it
  /// reaches the wire, per the MCP split between protocol errors (the
  /// request could not be processed) and tool execution errors (the
  /// tool ran and failed). Handlers keep using `?`, and the model still
  /// gets to see — and react to — the failure.
  ///
  /// [`call_tool`]: McpServer::call_tool
  pub fn err(msg: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(msg.to_string(), None)
  }

  /// Report a failed operation as a tool execution error.
  ///
  /// A navigation timeout or a missing element is not "the server
  /// broke": it is a result the model can act on (retry, re-snapshot,
  /// pick another selector). Sending it as JSON-RPC `-32603` told the
  /// host the server malfunctioned and, in hosts that abort a turn on
  /// protocol errors, denied the model the chance to recover.
  pub(crate) fn tool_failure(error: &ErrorData) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(error.message.to_string())])
  }

  /// Drop every cache keyed by `context`, including the page wrapper.
  /// Prefer this over `state.invalidate_context` from tool code: the
  /// wrapper cache lives on the server, so invalidating only the state
  /// caches leaves a wrapper pointing at a page that is going away.
  pub(crate) fn invalidate_context(&self, context: &str) {
    self.state.invalidate_context(context);
    self
      .page_wrappers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(context);
  }

  /// Drop every per-context cache, including page wrappers. Used after
  /// a change that can invalidate more contexts than the caller can
  /// name (closing a whole instance, shutting the server down).
  pub(crate) fn invalidate_all_caches(&self) {
    self.state.invalidate_all();
    self
      .page_wrappers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
  }

  /// Release everything the server holds for `context` after its
  /// browser-side state is gone: the caches, and the script session
  /// (its `QuickJS` VM, `vars`, and HTTP client). Without the session
  /// drop, closing a context left a warm VM alive until the LRU cap or
  /// the idle TTL got to it.
  pub(crate) fn release_context(&self, context: &str) {
    self.invalidate_context(context);
    self.sessions.remove(context);
  }

  /// Close every browser this server launched and drop all per-context
  /// caches. Idempotent, and safe to call while tool calls are in
  /// flight — they fail with `TargetClosed` rather than hanging.
  ///
  /// The transports are pipes for `cdp-pipe` / `webkit`, so those
  /// browsers exit on their own when the process dies. A `cdp-raw`
  /// Chrome or a `bidi` Firefox does not: it is reparented to pid 1 and
  /// stays, which is why every exit path has to come through here.
  pub async fn shutdown_browsers(&self) {
    self.state.write().await.shutdown().await;
    self.invalidate_all_caches();
    self.sessions.clear();
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

  /// Serve an `artifact://<relpath>` resource: raw bytes of a file the
  /// scripting layer wrote under `artifacts_root`. Binary payloads
  /// (screenshots, PDFs, traces) ship as base64 blobs; UTF-8 text ships as
  /// text. The sandbox rejects traversal / symlink escapes.
  async fn read_artifact_resource(&self, rel: &str, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let sandbox = self
      .artifacts_sandbox
      .as_ref()
      .ok_or_else(|| Self::err("artifacts are disabled: artifacts_root could not be prepared at startup"))?;
    let resolved = sandbox
      .resolve_read(rel)
      .map_err(|e| Self::err(format!("artifact path: {}", e.message)))?;
    let bytes = tokio::fs::read(&resolved)
      .await
      .map_err(|e| Self::err(format!("read artifact {rel}: {e}")))?;
    let mime = mime_for_path(rel);
    let contents = match String::from_utf8(bytes) {
      Ok(text) if mime.starts_with("text/") || mime == "application/json" => {
        ResourceContents::text(text, uri.to_string()).with_mime_type(mime)
      },
      Ok(text) => {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        ResourceContents::blob(b64, uri.to_string()).with_mime_type(mime)
      },
      Err(e) => {
        let b64 = base64::engine::general_purpose::STANDARD.encode(e.as_bytes());
        ResourceContents::blob(b64, uri.to_string()).with_mime_type(mime)
      },
    };
    Ok(ReadResourceResult::new(vec![contents]))
  }

  /// Write `bytes` to `rel` under `artifacts_root` and return an
  /// `artifact://<rel>` resource link for it. `None` when artifacts are
  /// disabled or the write fails — artifact persistence is best-effort and
  /// never fails the caller's primary result.
  pub(crate) async fn persist_artifact(&self, rel: &str, bytes: &[u8], mime: &str) -> Option<ContentBlock> {
    let sandbox = self.artifacts_sandbox.as_ref()?;
    let resolved = sandbox.resolve_write(rel).ok()?;
    if let Some(parent) = resolved.parent() {
      tokio::fs::create_dir_all(parent).await.ok()?;
    }
    tokio::fs::write(&resolved, bytes).await.ok()?;
    // Sweep after the write, protecting exactly the file just written: the
    // link handed back below must still resolve when the caller follows it.
    if let Some(budget) = self.artifacts_budget {
      let keep = std::collections::BTreeSet::from([resolved.clone()]);
      let evicted = budget.enforce(sandbox.root(), &keep).await;
      if evicted.files > 0 {
        tracing::info!(
          files = evicted.files,
          bytes = evicted.bytes,
          "artifacts budget: evicted least-recently-modified outputs"
        );
      }
    }
    let uri = format!("artifact://{rel}");
    let link = Resource::new(uri, rel)
      .with_mime_type(mime.to_string())
      .with_size(bytes.len() as u64);
    Some(ContentBlock::ResourceLink(link))
  }

  /// Enumerate files under `artifacts_root` (bounded) as `artifact://`
  /// resources for `resources/list`. Skips directories; caps the count so a
  /// runaway output dir can't blow up the listing. Entries are ordered
  /// newest-first before the cap is applied — `read_dir` order is
  /// arbitrary, and truncating an arbitrary order silently drops the
  /// artifacts a client just created once the directory outgrows the cap.
  async fn list_artifact_resources(&self, out: &mut Vec<Resource>) {
    const MAX: usize = 200;
    /// Scan bound: enough to sort recency over years of accumulated
    /// outputs without letting a runaway directory stall the listing.
    const SCAN_MAX: usize = 4096;
    let Some(sandbox) = self.artifacts_sandbox.as_ref() else {
      return;
    };
    let root = sandbox.root().to_path_buf();
    let mut found: Vec<(std::time::SystemTime, Resource)> = Vec::new();
    let mut stack = vec![root.clone()];
    'walk: while let Some(dir) = stack.pop() {
      let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        continue;
      };
      while let Ok(Some(entry)) = entries.next_entry().await {
        if found.len() >= SCAN_MAX {
          break 'walk;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type().await else { continue };
        if ft.is_dir() {
          stack.push(path);
          continue;
        }
        let Ok(rel) = path.strip_prefix(&root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let meta = entry.metadata().await.ok();
        let modified = meta
          .as_ref()
          .and_then(|m| m.modified().ok())
          .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let mut r = Resource::new(format!("artifact://{rel}"), rel.clone()).with_mime_type(mime_for_path(&rel));
        if let Some(size) = meta.map(|m| m.len()) {
          r = r.with_size(size);
        }
        found.push((modified, r));
      }
    }
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    out.extend(found.into_iter().take(MAX).map(|(_, r)| r));
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
    // Cold start: validate the target instance before launching so a bad session
    // key (e.g. a bare env name resolving to the 'default' instance) fails loudly
    // instead of silently launching an unmapped browser on the wrong environment.
    // Parse through `SessionKey` rather than splitting by hand: a bare
    // key that names a configured instance selects that instance, and a
    // hand-rolled split silently sent every bare key to `default`.
    let key = self.state.session_key(context).await;
    // `instance_health` may run the operator's args command, which shells
    // out under its own timeout; off the reactor so a cold start does not
    // stall every other session's tasks on this worker.
    let config = Arc::clone(&self.config);
    let instance = key.instance.to_string();
    tokio::task::spawn_blocking(move || config.instance_health(&instance))
      .await
      .map_err(|e| Self::err(format!("instance health check failed: {e}")))?
      .map_err(Self::err)?;
    let ctx_ref = ferridriver::context::ContextRef::new(self.state.state_arc(), context.to_string());
    let page = Box::pin(ctx_ref.new_page()).await.map_err(Self::err)?;
    self.invalidate_context(context);
    Ok(page.inner().clone())
  }

  /// Get a `Page` for a context, ensuring the required browser instance exists.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance cannot be launched or the active page
  /// for the given context cannot be retrieved.
  pub async fn page(&self, context: &str) -> Result<Arc<Page>, ErrorData> {
    let any_page = Box::pin(self.ensure_active_page(context)).await?;
    Ok(self.wrapper_for(context, any_page))
  }

  /// Cached-or-fresh `Page` wrapper for `context` over `any_page`.
  ///
  /// Returns the cached wrapper when it still wraps the same underlying
  /// browser page; otherwise builds one (binding the context so
  /// `page.context()` resolves to the live `BrowserContext` — Playwright
  /// parity) and caches it. Reusing the wrapper keeps wrapper-level
  /// state (default timeouts, `emulateMedia` merge state) alive across
  /// tool calls instead of resetting it per call. `Page::with_context`
  /// is sync and its frame-event listener spawn is latched per backend
  /// page, so the occasional rebuild is cheap.
  fn wrapper_for(&self, context: &str, any_page: AnyPage) -> Arc<Page> {
    let mut cache = self
      .page_wrappers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = cache.get(context)
      && cached.inner().same_backend_page(&any_page)
    {
      return Arc::clone(cached);
    }
    let ctx_ref = ferridriver::context::ContextRef::new(self.state.state_arc(), context.to_string());
    let page = Page::with_context(any_page, ctx_ref);
    cache.insert(context.to_string(), Arc::clone(&page));
    page
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
    let ctx_ref = ferridriver::context::ContextRef::new(self.state.state_arc(), context.to_string());
    let page = self.wrapper_for(context, any_page);
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
    let snap_fut = page.snapshot_for_ai();
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
    Ok(self.ok_text(format!("{msg}\n\n{snap}")))
  }

  /// Resolve `session`, hold its lock for the whole call, and hand the live
  /// page to `f`.
  ///
  /// Every page-driving tool needs these three steps in this order, and the
  /// order is load-bearing: resolving the page before taking the guard races
  /// a concurrent call on the same context, which opens a second page on a
  /// cold one. Doing it once here is what keeps that true across every
  /// handler instead of in each of them.
  ///
  /// The page future is boxed because it is large enough to trip
  /// `clippy::large_futures` at the call site — once here rather than at
  /// each of them.
  ///
  /// # Errors
  ///
  /// Propagates a failure to launch or attach the context's browser, and
  /// whatever `f` returns.
  pub(crate) async fn on_page<F, Fut, T>(&self, session: Option<&String>, f: F) -> Result<T, ErrorData>
  where
    F: FnOnce(Arc<Page>, String) -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorData>>,
  {
    let context = sess(session).to_string();
    let _guard = self.session_guard(&context).await;
    let page = Box::pin(self.page(&context)).await?;
    f(page, context).await
  }

  /// Resolve `session` and hold its lock for the whole call, without
  /// opening a page.
  ///
  /// [`Self::on_page`] for the tools that drive one; this for the tools that
  /// act on the context itself (listing, closing, reading its logs), where
  /// resolving a page would launch a browser the call does not need.
  ///
  /// # Errors
  ///
  /// Whatever `f` returns.
  pub(crate) async fn on_session<F, Fut, T>(&self, session: Option<&String>, f: F) -> Result<T, ErrorData>
  where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorData>>,
  {
    let context = sess(session).to_string();
    let _guard = self.session_guard(&context).await;
    f(context).await
  }

  /// A text content block with declared secrets redacted.
  ///
  /// Tools build their replies through this rather than `ContentBlock::text`
  /// directly: redaction that covers only some replies is not redaction, and
  /// a page's own text (an evaluated value, a snapshot, a search hit) is a
  /// routine place for a credential to surface.
  pub(crate) fn text(&self, body: impl Into<String>) -> ContentBlock {
    ContentBlock::text(self.secrets.redact(&body.into()).into_owned())
  }

  /// A success reply carrying one redacted text block.
  pub(crate) fn ok_text(&self, body: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![self.text(body)])
  }

  /// Emit an MCP progress notification for the in-flight tool call (SEP-2575).
  ///
  /// `token` is the caller's `_meta.progressToken`; a `None` (client did not
  /// opt into progress) or a transport hiccup is silently ignored — progress
  /// is strictly best-effort and never fails the tool. `progress`/`total`
  /// use the same units the tool documents; `message` is a short human note.
  pub(crate) async fn emit_progress(
    peer: &rmcp::service::Peer<rmcp::RoleServer>,
    token: Option<&rmcp::model::ProgressToken>,
    progress: f64,
    total: Option<f64>,
    message: &str,
  ) {
    let Some(token) = token else { return };
    let mut param = rmcp::model::ProgressNotificationParam::new(token.clone(), progress).with_message(message);
    if let Some(total) = total {
      param = param.with_total(total);
    }
    let _ = peer.notify_progress(param).await;
  }
}

/// Validate a extension call's arguments against the tool's pre-compiled
/// `inputSchema` validator (built once at load by
/// [`crate::extension::ExtensionRegistry::new`]).
/// Every violation of `validator` in `value`, joined for display —
/// `None` when the value conforms.
fn schema_violations(validator: &jsonschema::Validator, value: &serde_json::Value) -> Option<String> {
  let mut messages: Vec<String> = validator
    .iter_errors(value)
    .map(|e| {
      let path = e.instance_path().to_string();
      if path.is_empty() {
        e.to_string()
      } else {
        format!("{path}: {e}")
      }
    })
    .take(20)
    .collect();
  if messages.is_empty() {
    return None;
  }
  messages.sort();
  messages.dedup();
  Some(messages.join("\n- "))
}

fn validate_tool_args(
  extension: &str,
  validator: &jsonschema::Validator,
  args: &serde_json::Value,
) -> Result<(), String> {
  let Some(messages) = schema_violations(validator, args) else {
    return Ok(());
  };
  Err(format!(
    "invalid arguments for `{extension}` (does not match inputSchema):\n- {messages}"
  ))
}

/// Best-effort MIME type from a file extension, for `artifact://` resources.
/// Covers the formats the scripting layer actually emits (screenshots, PDFs,
/// traces, text); everything else falls back to `application/octet-stream`.
fn mime_for_path(path: &str) -> String {
  let ext = std::path::Path::new(path)
    .extension()
    .and_then(|e| e.to_str())
    .unwrap_or("")
    .to_ascii_lowercase();
  match ext.as_str() {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "webp" => "image/webp",
    "gif" => "image/gif",
    "svg" => "image/svg+xml",
    "pdf" => "application/pdf",
    "json" | "trace" => "application/json",
    "txt" | "log" => "text/plain",
    "html" | "htm" => "text/html",
    "csv" => "text/csv",
    "zip" => "application/zip",
    _ => "application/octet-stream",
  }
  .to_string()
}

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
// list_prompts / get_prompt are async by the ServerHandler trait contract;
// they currently have no internal await but must keep the trait's signature.
impl ServerHandler for McpServer {
  fn get_info(&self) -> ServerInfo {
    ServerInfo::new(
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
    .with_instructions(self.config.server_instructions().to_string())
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
    if !self.tool_router.has_route(request.name.as_ref())
      && self.promoted_extension_tool(request.name.as_ref()).is_some()
    {
      let name = request.name.to_string();
      let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
      return match self.invoke_extension_tool(&name, args).instrument(span).await {
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
  async fn list_tools(
    &self,
    _request: Option<PaginatedRequestParams>,
    context: RequestContext<RoleServer>,
  ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
    let mut tools = self.tool_router.list_all();
    tools.extend(self.extensions().promoted.iter().cloned());
    let result = rmcp::model::ListToolsResult::with_all_items(tools);
    // The listing is worth caching — the built-ins are compiled in and an
    // extension reload publishes `tools/list_changed`, which invalidates the
    // client's copy — but the hints are only spec-legal for a peer that
    // negotiated 2026-07-28.
    Ok(if supports_cache_hints(&context) {
      result
        .with_ttl_ms(TOOL_LIST_TTL_MS)
        .with_cache_scope(rmcp::model::CacheScope::Public)
    } else {
      result
    })
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

  async fn list_prompts(
    &self,
    _request: Option<PaginatedRequestParams>,
    _context: RequestContext<RoleServer>,
  ) -> Result<ListPromptsResult, ErrorData> {
    Ok(ListPromptsResult::with_all_items(Self::prompt_definitions()))
  }

  async fn get_prompt(
    &self,
    request: GetPromptRequestParams,
    _context: RequestContext<RoleServer>,
  ) -> Result<GetPromptResponse, ErrorData> {
    Self::prompt_messages(&request).map(Into::into)
  }
}

impl McpServer {
  /// The body of [`ServerHandler::read_resource`], kept as an inherent method
  /// so every arm can build a plain [`ReadResourceResult`]; the trait boundary
  /// widens it to the MRTR response enum once.
  async fn read_resource_contents(&self, request: ReadResourceRequestParams) -> Result<ReadResourceResult, ErrorData> {
    let uri = &request.uri;
    if let Some(rel) = uri.strip_prefix("artifact://") {
      return self.read_artifact_resource(rel, uri).await;
    }
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
        let bytes = page.screenshot().await.map_err(Self::err)?;
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

  /// The prompts this server advertises. Compiled in, so the listing is the
  /// same on every call.
  fn prompt_definitions() -> Vec<Prompt> {
    vec![
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
    ]
  }

  /// The body of [`ServerHandler::get_prompt`]: builds the messages for one
  /// named prompt, which the trait boundary widens to the MRTR response enum.
  fn prompt_messages(request: &GetPromptRequestParams) -> Result<GetPromptResult, ErrorData> {
    let args = request.arguments.clone().unwrap_or_default();
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
          Role::User,
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
          Role::User,
          format!(
            "Test the form on {url}:\n1. Navigate to the page\n2. Take a snapshot to identify form fields\n3. Fill all required fields with realistic test data\n4. Click {submit}\n5. Verify the form submitted successfully\n6. Report the result"
          ),
        )]))
      },
      "audit-accessibility" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
        Role::User,
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
          Role::User,
          format!(
            "Compare {url} between two sessions:\n1. Open the page in session='{sa}' and session='{sb}'\n2. Take a snapshot of each\n3. Compare: visible content differences, available navigation, form fields, cookies\n4. Report what differs between the two sessions"
          ),
        )]))
      },
      _ => Err(Self::err(format!("Unknown prompt: {}", request.name))),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::validate_tool_args;

  #[test]
  fn schema_validation_accepts_conforming_and_rejects_bad() {
    let schema = serde_json::json!({
      "type": "object",
      "properties": { "user": { "type": "string" }, "n": { "type": "integer" } },
      "required": ["user"],
      "additionalProperties": false
    });
    let validator = jsonschema::validator_for(&schema).expect("valid schema");

    assert!(validate_tool_args("t", &validator, &serde_json::json!({ "user": "a", "n": 3 })).is_ok());

    let missing = validate_tool_args("t", &validator, &serde_json::json!({ "n": 3 })).unwrap_err();
    assert!(missing.contains("invalid arguments for `t`"), "{missing}");

    let wrong_type = validate_tool_args("t", &validator, &serde_json::json!({ "user": 1 })).unwrap_err();
    assert!(wrong_type.contains("invalid arguments for `t`"), "{wrong_type}");

    let extra = validate_tool_args("t", &validator, &serde_json::json!({ "user": "a", "x": 1 })).unwrap_err();
    assert!(extra.contains("invalid arguments for `t`"), "{extra}");
  }

  #[test]
  fn an_invalid_schema_is_reported_by_the_registry_at_load() {
    // Compilation of the declared schema happens once, at
    // `ExtensionRegistry::new`; the stored error is what `invoke_extension_tool`
    // returns on every call to that tool.
    let registry = crate::extension::ExtensionRegistry::new(
      vec![crate::extension::LoadedExtension {
        tools: vec![
          serde_json::from_value(serde_json::json!({
            "name": "bad",
            "inputSchema": { "type": "not-a-real-type" }
          }))
          .expect("manifest"),
        ],
        bytecode: std::sync::Arc::from(Vec::new().into_boxed_slice()),
        path: std::path::PathBuf::from("bad.js"),
        source_map: None,
      }],
      Vec::new(),
    );
    let compiled = registry.validator("bad").expect("schema present");
    let err = compiled.as_ref().expect_err("schema must be invalid");
    assert!(err.contains("invalid inputSchema"), "{err}");
  }

  fn loaded_extension(manifest: serde_json::Value) -> crate::extension::LoadedExtension {
    crate::extension::LoadedExtension {
      tools: vec![serde_json::from_value(manifest).expect("manifest")],
      bytecode: std::sync::Arc::from(Vec::new().into_boxed_slice()),
      path: std::path::PathBuf::from("ext.js"),
      source_map: None,
    }
  }

  fn test_server() -> super::McpServer {
    super::McpServer::with_options(
      ferridriver::state::ConnectMode::Launch,
      ferridriver::backend::BackendKind::CdpPipe,
      true,
      std::sync::Arc::new(ferridriver_config::mcp::McpConfig::default()),
    )
  }

  #[test]
  fn policy_conflicts_flags_net_and_command_violations() {
    let server = test_server().with_script_caps(ferridriver_script::ScriptCaps::default().with_extension_policy(
      ferridriver_config::ExtensionPolicyConfig {
        net: Some(vec!["*.acme.com".into()]),
        commands: ferridriver_config::ExtensionCommandsCeiling::ArgvOnly,
        ..ferridriver_config::ExtensionPolicyConfig::default()
      },
    ));
    let loaded = vec![loaded_extension(serde_json::json!({
      "name": "t",
      "allow": {
        "net": ["api.acme.com", "evil.example"],
        "commands": { "sh": "echo hi", "ok": { "run": ["echo", "hi"] } }
      }
    }))];
    let warnings = server.policy_conflicts(&loaded);
    assert_eq!(warnings.len(), 2, "one net + one command warning: {warnings:?}");
    assert!(
      warnings
        .iter()
        .any(|(_, m)| m.contains("evil.example") && !m.contains("api.acme.com")),
      "only the out-of-ceiling entry is flagged: {warnings:?}"
    );
    assert!(
      warnings
        .iter()
        .any(|(_, m)| m.contains("\"sh\"") && m.contains("argvOnly")),
      "the shell-form command is flagged: {warnings:?}"
    );
  }

  #[test]
  fn policy_conflicts_is_silent_without_a_ceiling() {
    let server = test_server();
    let loaded = vec![loaded_extension(serde_json::json!({
      "name": "t",
      "allow": { "net": ["anywhere.example"], "commands": { "sh": "echo hi" } }
    }))];
    assert!(server.policy_conflicts(&loaded).is_empty());
  }

  #[test]
  fn extension_tool_result_validates_output_schema_and_ships_structured_content() {
    let server = test_server();
    server.publish_extensions_for_test(crate::extension::ExtensionRegistry::new(
      vec![loaded_extension(serde_json::json!({
        "name": "typed",
        "outputSchema": {
          "type": "object",
          "properties": { "ok": { "type": "boolean" } },
          "required": ["ok"],
          "additionalProperties": false
        }
      }))],
      Vec::new(),
    ));

    let good = ferridriver_script::ScriptResult::ok(serde_json::json!({ "ok": true }), 3, Vec::new());
    let reply = server.extension_tool_result("typed", &good).expect("reply");
    assert_ne!(reply.is_error, Some(true), "conforming output is a success");
    assert_eq!(
      reply.structured_content,
      Some(serde_json::json!({ "ok": true })),
      "conforming output ships as structuredContent"
    );

    let bad = ferridriver_script::ScriptResult::ok(serde_json::json!({ "ok": "yes" }), 3, Vec::new());
    let reply = server.extension_tool_result("typed", &bad).expect("reply");
    assert_eq!(reply.is_error, Some(true), "non-conforming output is a tool error");

    let untyped = ferridriver_script::ScriptResult::ok(serde_json::json!("anything"), 3, Vec::new());
    let reply = server.extension_tool_result("absent", &untyped).expect("reply");
    assert_ne!(reply.is_error, Some(true));
    assert_eq!(
      reply.structured_content, None,
      "no declared outputSchema, no structuredContent"
    );
  }

  #[test]
  fn output_schema_compilation_errors_are_stored_per_tool() {
    let registry = crate::extension::ExtensionRegistry::new(
      vec![loaded_extension(serde_json::json!({
        "name": "bad-out",
        "outputSchema": { "type": "not-a-real-type" }
      }))],
      Vec::new(),
    );
    let compiled = registry.output_validator("bad-out").expect("schema present");
    let err = compiled.as_ref().expect_err("schema must be invalid");
    assert!(err.contains("invalid outputSchema"), "{err}");
  }

  #[test]
  fn manifest_accepts_title_output_schema_and_annotations() {
    let m: crate::extension::ToolManifest = serde_json::from_value(serde_json::json!({
      "name": "meta",
      "title": "Meta Tool",
      "outputSchema": { "type": "object" },
      "annotations": { "readOnlyHint": true, "openWorldHint": false }
    }))
    .expect("manifest");
    assert_eq!(m.title.as_deref(), Some("Meta Tool"));
    assert!(m.output_schema.is_some());
    let a = m.annotations.expect("annotations");
    assert_eq!(a.read_only_hint, Some(true));
    assert_eq!(a.open_world_hint, Some(false));
  }

  #[test]
  fn traceparent_parses_valid_and_rejects_malformed() {
    let (trace, parent) =
      super::parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").expect("valid");
    assert_eq!(trace, "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(parent, "00f067aa0ba902b7");
    // all-zero trace id is invalid per the trace-context spec
    assert!(super::parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none());
    // all-zero parent id is invalid
    assert!(super::parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01").is_none());
    assert!(super::parse_traceparent("garbage").is_none());
    assert!(super::parse_traceparent("00-tooShort-00f067aa0ba902b7-01").is_none());
    // non-hex characters in the trace id
    assert!(super::parse_traceparent("00-ZZf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none());
  }

  #[test]
  fn mime_for_path_maps_known_and_falls_back() {
    assert_eq!(super::mime_for_path("screenshots/a.png"), "image/png");
    assert_eq!(super::mime_for_path("x.PDF"), "application/pdf");
    assert_eq!(super::mime_for_path("run.trace"), "application/json");
    assert_eq!(super::mime_for_path("notes.txt"), "text/plain");
    assert_eq!(super::mime_for_path("blob.bin"), "application/octet-stream");
    assert_eq!(super::mime_for_path("noext"), "application/octet-stream");
  }

  #[test]
  fn builtin_tools_carry_titles_and_annotations() {
    let tools = super::McpServer::tool_router().list_all();
    let by = |n: &str| {
      tools
        .iter()
        .find(|t| t.name == n)
        .unwrap_or_else(|| panic!("tool {n} not found"))
    };
    let ro = |t: &rmcp::model::Tool| t.annotations.as_ref().and_then(|a| a.read_only_hint);
    let ow = |t: &rmcp::model::Tool| t.annotations.as_ref().and_then(|a| a.open_world_hint);
    let de = |t: &rmcp::model::Tool| t.annotations.as_ref().and_then(|a| a.destructive_hint);

    let snap = by("snapshot");
    assert_eq!(snap.title.as_deref(), Some("Accessibility Snapshot"));
    assert_eq!(ro(snap), Some(true));
    assert_eq!(ow(snap), Some(false));

    let nav = by("navigate");
    assert_eq!(nav.title.as_deref(), Some("Navigate"));
    assert_eq!(ro(nav), Some(false));
    assert_eq!(ow(nav), Some(true));

    let page = by("page");
    assert_eq!(de(page), Some(true));

    let script = by("run_script");
    assert_eq!(script.title.as_deref(), Some("Run Browser Script"));
    assert_eq!(ro(script), Some(false));

    // Every built-in tool should carry a human title.
    for t in &tools {
      assert!(t.title.is_some(), "tool {} is missing a title", t.name);
    }
  }

  // The two tools whose payload is a documented JSON object publish it as an
  // `outputSchema`, so a client can validate the structured content instead of
  // trusting the prose in the description.
  #[test]
  fn the_json_returning_tools_publish_an_output_schema() {
    let tools = super::McpServer::tool_router().list_all();
    for (name, required_key) in [("run_script", "duration_ms"), ("run_bdd", "scenarios")] {
      let tool = tools
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("tool {name} not found"));
      let schema = tool
        .output_schema
        .as_ref()
        .unwrap_or_else(|| panic!("tool {name} must declare an output schema"));
      let rendered = serde_json::to_string(schema).expect("schema serializes");
      assert!(
        rendered.contains(required_key),
        "{name} output schema must describe {required_key}: {rendered}"
      );
    }
  }

  struct TmpArtifactsConfig(std::path::PathBuf);
  impl super::McpServerConfig for TmpArtifactsConfig {
    fn script_root(&self) -> std::path::PathBuf {
      self.0.join("scripts")
    }
    fn artifacts_root(&self) -> std::path::PathBuf {
      self.0.join("artifacts")
    }
  }

  #[tokio::test]
  async fn artifact_persist_and_read_roundtrip() {
    use base64::Engine as _;
    let base = std::env::temp_dir().join(format!("ferri-artifact-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let server = super::McpServer::with_config(
      ferridriver::state::ConnectMode::Launch,
      ferridriver::backend::BackendKind::CdpPipe,
      std::sync::Arc::new(TmpArtifactsConfig(base.clone())),
    );

    let png = b"\x89PNG\r\n\x1a\nfake";
    let link = server
      .persist_artifact("screenshots/x.png", png, "image/png")
      .await
      .expect("artifact persisted → resource link");
    match link {
      rmcp::model::ContentBlock::ResourceLink(res) => {
        assert_eq!(res.uri, "artifact://screenshots/x.png");
        assert_eq!(res.mime_type.as_deref(), Some("image/png"));
        assert_eq!(res.size, Some(png.len() as u64));
      },
      other => panic!("expected ResourceLink, got {other:?}"),
    }

    // The persisted bytes come back through the artifact:// resource reader as a base64 blob.
    let read = server
      .read_artifact_resource("screenshots/x.png", "artifact://screenshots/x.png")
      .await
      .expect("artifact readable");
    let blob = match read.contents.first().expect("one content") {
      rmcp::model::ResourceContents::BlobResourceContents { blob, mime_type, .. } => {
        assert_eq!(mime_type.as_deref(), Some("image/png"));
        blob.clone()
      },
      other => panic!("expected blob contents, got {other:?}"),
    };
    let decoded = base64::engine::general_purpose::STANDARD
      .decode(blob)
      .expect("valid base64");
    assert_eq!(decoded, png);

    // A traversal attempt is rejected by the sandbox, not served.
    assert!(
      server
        .read_artifact_resource("../secret", "artifact://../secret")
        .await
        .is_err()
    );

    // list_resources surfaces the persisted artifact as an artifact:// resource.
    let mut listed = Vec::new();
    server.list_artifact_resources(&mut listed).await;
    assert!(
      listed.iter().any(|r| r.uri == "artifact://screenshots/x.png"),
      "artifact should appear in resource listing: {listed:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
  }
}
