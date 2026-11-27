//! `McpServer` struct, constructors, builders, and shared submodule re-exports.

mod config;
mod extensions;
mod handler;
mod resources;
mod session;
mod state;

pub use config::{
  DEFAULT_INSTRUCTIONS, DEFAULT_SERVER_NAME, DEFAULT_SETTLE_TIMEOUT_MS, DefaultConfig, McpServerConfig,
};
pub(crate) use extensions::LoadedExtensions;
pub use state::{SharedState, State, ctx, sess};

use ferridriver::Page;
use ferridriver::backend::BackendKind;
use ferridriver::state::{BrowserState, ConnectMode};
use rmcp::{
  ErrorData,
  handler::server::router::tool::ToolRouter,
  model::{CallToolResult, ContentBlock},
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Browser automation MCP server.
#[derive(Clone)]
pub struct McpServer {
  pub(crate) state: SharedState,
  /// One cached `Arc<Page>` wrapper per context, validated against the
  /// currently-active backend page on every lookup. Tool handlers used
  /// to mint a fresh wrapper per call, which silently reset all
  /// wrapper-level state (default timeouts, `emulateMedia` merge state)
  /// between MCP tool calls. The cache keeps a context's wrapper alive
  /// until its underlying browser page changes (new page / relaunch).
  pub(super) page_wrappers: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, Arc<Page>>>>,
  /// The composed tool router. Public so consumers can list tools or dispatch directly.
  pub tool_router: ToolRouter<Self>,
  /// Configuration trait object for customizing server behavior.
  pub config: Arc<dyn McpServerConfig>,
  /// Typed slot for consumer-specific state (e.g. Jira clients),
  /// attached via [`Self::with_extension`]. Unrelated to the extension
  /// system — this is host-embedding state.
  custom_ext: Arc<dyn std::any::Any + Send + Sync>,
  /// `QuickJS` scripting engine -- fresh context per `run_script` invocation.
  pub(crate) script_engine: Arc<ferridriver_script::ScriptEngine>,
  /// Filesystem artifacts for scripts (`None` if the configured root could not
  /// be created or canonicalised; `run_script` will return an error).
  pub(crate) script_root: Option<std::path::PathBuf>,
  /// Filesystem artifacts for script outputs, exposed as the `artifacts`
  /// global. `None` if the configured artifacts root could not be prepared;
  /// in that case scripts just don't get an `artifacts` binding and must
  /// use `fs` for output (which pollutes the script source directory).
  pub(crate) artifacts_dir: Option<Arc<ferridriver_script::OutputDir>>,
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
  /// Resolved scripting artifacts relaxations (env allow-list / node
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
  pub(super) reload_lock: Arc<tokio::sync::Mutex<()>>,
  /// Resolved `[test]` config (feature/step globs, browser, workers,
  /// retries, reporters, ...). Default = `TestConfig::default()`; set by
  /// [`McpServer::with_test_config`] from the operator's `ferridriver.toml`.
  /// Used as the base config for the `run_bdd` tool so an MCP-driven BDD
  /// run inherits the same `[test]` settings as the `ferridriver bdd` CLI.
  pub(crate) test_config: Box<ferridriver_test::config::TestConfig>,
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

/// Build the browser state an [`McpServerConfig`] describes: the base launch
/// plan, the instance names it knows, and the per-instance override and
/// connection resolvers.
///
/// Shared so every host that provisions a configured instance -- the MCP
/// server and `ferridriver run --instance` -- resolves it identically. A
/// second copy of this wiring is how one host silently stops honouring a
/// config key the other still reads.
#[must_use]
pub fn browser_state_for(
  mode: ConnectMode,
  backend: BackendKind,
  headless: bool,
  config: &Arc<dyn McpServerConfig>,
) -> BrowserState {
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
  let kind = base.kind.unwrap_or(kind);
  let backend = base.backend.unwrap_or(backend);
  let mut browser_state = BrowserState::with_plan(
    mode,
    ferridriver::options::LaunchPlan {
      backend,
      kind,
      headless,
      args: Vec::new(),
      executable_path: base.executable_path,
      user_data_dir: base.user_data_dir,
      default_viewport: config.default_viewport(),
      ..Default::default()
    },
  );

  browser_state.set_context_defaults(config.default_context_options());

  // A bare session key that names a configured instance must select
  // that instance rather than a context on `default`.
  browser_state.set_known_instances(config.instance_names());

  // Wire per-instance launch settings from the config trait.
  let config_clone = Arc::clone(config);
  browser_state.set_instance_overrides_fn(Arc::new(move |instance| config_clone.instance_overrides(instance)));
  // Wire per-instance connection resolver from config trait.
  let config_clone = Arc::clone(config);
  browser_state.set_instance_resolver_fn(Arc::new(move |instance| config_clone.resolve_instance(instance)));
  browser_state
}

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
    let browser_state = browser_state_for(mode, backend, headless, &config);
    let state = SharedState::new(browser_state);

    // Scripting engine + artifacts. The artifacts needs an existing canonical
    // directory; we create the configured root up front and log (not panic)
    // if initialisation fails so the rest of the server still works.
    let script_engine = Arc::new(ferridriver_script::ScriptEngine::new(config.script_engine_config()));
    let sessions = Arc::new(ferridriver_script::SessionTable::new(
      script_engine.config().max_session_vms,
      script_engine.config().session_idle_ttl,
    ));
    let script_root = config.script_root();
    let script_root = match std::fs::create_dir_all(&script_root)
      .map_err(|e| format!("{e}"))
      .and_then(|()| std::fs::canonicalize(&script_root).map_err(|e| format!("{e}")))
    {
      Ok(root) => Some(root),
      Err(msg) => {
        tracing::warn!(
          script_root = %script_root.display(),
          error = %msg,
          "scripting disabled: failed to prepare script_root; run_script will return an error"
        );
        None
      },
    };

    // Artifacts directory — separate from the script root so outputs do
    // not pollute the source tree. If it
    // fails to prepare we log and disable the `artifacts` global only;
    // `run_script` itself keeps working.
    let artifacts_root = config.artifacts_root();
    let artifacts_dir = match std::fs::create_dir_all(&artifacts_root)
      .map_err(|e| format!("{e}"))
      .and_then(|()| ferridriver_script::OutputDir::new(&artifacts_root).map_err(|e| e.message))
    {
      Ok(sb) => Some(Arc::new(sb)),
      Err(msg) => {
        tracing::warn!(
          artifacts_root = %artifacts_root.display(),
          error = %msg,
          "artifacts binding disabled: failed to prepare artifacts_root; scripts can still write via fs"
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
      script_root,
      artifacts_dir,
      artifacts_budget,
      secrets,
      sessions,
      script_caps: ferridriver_script::ScriptCaps::default(),
      extension_registry: Arc::new(arc_swap::ArcSwap::from_pointee(LoadedExtensions::default())),
      reload_lock: Arc::new(tokio::sync::Mutex::new(())),
      test_config: Box::default(),
      extension_specs: Vec::new(),
      bdd_engine: Arc::new(Mutex::new(crate::bdd_engine::BddEngine::new())),
      builtin_steps: Arc::new(std::sync::OnceLock::new()),
    }
  }

  /// Add extra tool routers (merges with built-in browser tools).
  #[must_use]
  pub fn with_extra_tools(mut self, extra: ToolRouter<Self>) -> Self {
    self.tool_router += extra;
    self
  }

  /// Set the scripting artifacts relaxations (resolved from the
  /// operator's `[scripting]` config). Without this the artifacts stays
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
  pub fn with_test_config(mut self, test_config: Box<ferridriver_test::config::TestConfig>) -> Self {
    self.test_config = test_config;
    self
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
  /// Carries `INTERNAL_ERROR` only as an in-process marker: `call_tool`
  /// turns it into a `CallToolResult` with `isError: true` before it
  /// reaches the wire, per the MCP split between protocol errors (the
  /// request could not be processed) and tool execution errors (the
  /// tool ran and failed). Handlers keep using `?`, and the model still
  /// gets to see — and react to — the failure.
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
}
