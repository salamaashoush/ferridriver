//! Browser state management with instance→context→page hierarchy.
//!
//! Design principles:
//! - Instance = Chrome process (owns chrome flags)
//! - Context = isolated browser context within an instance (isolated cookies, storage)
//! - Page = tab within a context
//! - Composite session key: `"<instance>:<context>"` (backwards compat: bare name = default instance)
//! - No global "active page" -- every tool call specifies its session key
//! - No races possible: there is no shared mutable selection state

use std::sync::Arc;

use crate::backend::{AnyBrowser, AnyPage, BackendKind};
use crate::context::BrowserContext;
use crate::error::{FerriError, Result};
use rustc_hash::FxHashMap as HashMap;

pub(crate) mod allocation;

/// Playwright's default viewport, applied unless a caller asks for a
/// size of its own or opts out with `viewport: null`
/// (`server/browserContext.ts::validateBrowserContextOptions`).
///
/// The one definition every layer resolves to -- core options, the
/// config schema and the MCP server all read it here, so the default
/// cannot drift between the host that launches a browser and the host
/// that emulates a page in it.
pub const DEFAULT_VIEWPORT_WIDTH: i64 = 1280;
pub const DEFAULT_VIEWPORT_HEIGHT: i64 = 720;

// Re-export log types from context (they live there now).
pub use crate::console_message::ConsoleMessage;
pub use crate::context::DialogEvent;
pub use crate::network::Request;

/// Retention caps for the per-context diagnostics logs. The backend
/// listeners append on every event; without a cap a long-lived session
/// on a chatty page grows without bound. Sized for the MCP diagnostics
/// tools' "recent activity" reads, not full-session capture (use
/// routing / HAR for that).
pub const CONSOLE_LOG_CAP: usize = 500;
pub const NETWORK_LOG_CAP: usize = 1000;
pub const DIALOG_LOG_CAP: usize = 100;

/// Append to a diagnostics log, evicting the oldest entries past `cap`.
pub fn push_capped<T>(log: &mut Vec<T>, entry: T, cap: usize) {
  log.push(entry);
  if log.len() > cap {
    let overflow = log.len() - cap;
    log.drain(..overflow);
  }
}

/// Arc handles to a context's log collections, usable without holding the `BrowserState` lock.
#[derive(Clone)]
pub struct ContextLogHandles {
  pub console: std::sync::Arc<tokio::sync::RwLock<Vec<ConsoleMessage>>>,
  pub network: std::sync::Arc<tokio::sync::RwLock<Vec<Request>>>,
  pub dialog: std::sync::Arc<tokio::sync::RwLock<Vec<DialogEvent>>>,
}

// ── SessionKey ──────────────────────────────────────────────────────────────

/// Parsed composite session key: `"<instance>:<context>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey {
  pub instance: Arc<str>,
  pub context: Arc<str>,
}

/// The configured instance names a bare session key may name.
///
/// Owned by [`BrowserState`], not by a process global: parsing that
/// depends on install order is untestable without a lock, and one
/// process may drive two states (a test runner and an embedded MCP
/// server) whose configs define different instances.
pub type KnownInstances = Arc<[Box<str>]>;

impl SessionKey {
  /// Parse a composite key string, with no instance vocabulary.
  ///
  /// - `"default"` → instance="default", context="default"
  /// - `"staging:admin"` → instance="staging", context="admin"
  /// - `"myctx"` → instance="default", context="myctx"
  ///
  /// A bare name is a CONTEXT here. Use
  /// [`BrowserState::session_key`] (or [`Self::parse_with`]) wherever the
  /// configured instance names are reachable, so a bare key that names
  /// one selects that instance.
  #[must_use]
  pub fn parse(raw: &str) -> Self {
    Self::parse_with(raw, &[])
  }

  /// [`Self::parse`], resolving a bare name against the configured
  /// instance names.
  ///
  /// Without the vocabulary a bare key was ALWAYS read as a context on
  /// the `default` instance, so `session: "staging"` silently drove an
  /// unconfigured browser while looking like it had selected the staging
  /// environment — a mistake so common that consumers resorted to
  /// documenting it in their server instructions.
  #[must_use]
  pub fn parse_with(raw: &str, known_instances: &[Box<str>]) -> Self {
    if let Some((inst, ctx)) = raw.split_once(':') {
      return SessionKey {
        instance: Arc::from(inst),
        context: Arc::from(ctx),
      };
    }
    if raw == "default" {
      return SessionKey {
        instance: Arc::from("default"),
        context: Arc::from("default"),
      };
    }
    // A bare name that NAMES a configured instance means that
    // instance, not a context on `default`. Anything else keeps the
    // original "bare name is a context" behaviour.
    if known_instances.iter().any(|known| &**known == raw) {
      return SessionKey {
        instance: Arc::from(raw),
        context: Arc::from("default"),
      };
    }
    SessionKey {
      instance: Arc::from("default"),
      context: Arc::from(raw),
    }
  }

  /// Reconstruct the composite key string.
  #[must_use]
  pub fn to_composite(&self) -> String {
    format!("{}:{}", self.instance, self.context)
  }
}

// ── BrowserInstance ─────────────────────────────────────────────────────────

/// A single Chrome process and its isolated contexts.
struct BrowserInstance {
  browser: AnyBrowser,
  contexts: HashMap<String, BrowserContext>,
  /// Monotonic id assigned when this instance was (re)created. A
  /// consumer that caches state derived from a browser session (e.g. the
  /// MCP per-session script VM, which may hold JS handles into pages)
  /// stores the generation it built against and discards that state when
  /// the generation changes — a relaunch/reconnect under the same
  /// instance name is a *different* browser session.
  generation: u64,
}

enum PendingOwner {
  Browser(Box<BrowserInstance>),
  Allocation {
    generation: u64,
    owner: Arc<allocation::Allocation>,
  },
}

impl PendingOwner {
  fn generation(&self) -> u64 {
    match self {
      Self::Browser(instance) => instance.generation,
      Self::Allocation { generation, .. } => *generation,
    }
  }

  fn instance(&self) -> Option<&BrowserInstance> {
    match self {
      Self::Browser(instance) => Some(instance),
      Self::Allocation { .. } => None,
    }
  }

  fn instance_mut(&mut self) -> Option<&mut BrowserInstance> {
    match self {
      Self::Browser(instance) => Some(instance),
      Self::Allocation { .. } => None,
    }
  }
}

impl From<BrowserInstance> for PendingOwner {
  fn from(instance: BrowserInstance) -> Self {
    Self::Browser(Box::new(instance))
  }
}

#[derive(Clone)]
pub struct PageOpenPlan {
  pub browser: AnyBrowser,
  pub viewport: Option<crate::options::ViewportConfig>,
  pub browser_context_id: Option<String>,
}

impl BrowserInstance {
  fn context(&self, name: &str) -> Result<&BrowserContext> {
    self
      .contexts
      .get(name)
      .ok_or_else(|| FerriError::invalid_argument("context", format!("'{name}' not found in this instance")))
  }

  fn context_mut(&mut self, name: &str) -> &mut BrowserContext {
    self
      .contexts
      .entry(name.to_string())
      .or_insert_with(|| BrowserContext::new(name.to_string()))
  }

  fn context_mut_checked(&mut self, name: &str) -> Result<&mut BrowserContext> {
    self
      .contexts
      .get_mut(name)
      .ok_or_else(|| FerriError::invalid_argument("context", format!("'{name}' not found")))
  }

  fn remove_context(&mut self, name: &str) {
    // Dropping the context drops its `Vec<AnyPage>`, but a page's
    // listener tasks are detached: they park on transport-wide taps and
    // hold `Arc` clones of the transport, so dropping the handle does not
    // stop them. Release them explicitly, or every context that closes
    // leaves ~10 tasks awake for the life of the process and keeps the
    // transport (and its fd) alive after the browser is gone.
    if let Some(ctx) = self.contexts.remove(name) {
      for page in &ctx.pages {
        page.dispose_local();
      }
    }
  }
}

// ── BrowserState ────────────────────────────────────────────────────────────

/// Callback type for per-instance launch settings.
///
/// Returns the full override set for an instance (args, profile
/// directory, binary, headless, backend, environment,
/// `ignoreDefaultArgs`) or an error that ABORTS the launch. Aborting
/// matters: a consumer whose environment lookup fails for an instance
/// name is saying "this instance is not real", and launching a browser
/// anyway lands the caller on an unconfigured environment that looks
/// like the right one.
///
/// `Arc`, not `Box`: the launch path clones it out from under the state
/// lock and runs it on the blocking pool. Implementations shell out
/// (`instanceArgsCommand`), and calling that while holding the state's
/// write guard froze every other session for the duration.
pub type InstanceOverridesFn =
  Arc<dyn Fn(&str) -> std::result::Result<crate::options::InstanceOverrides, String> + Send + Sync>;

/// Callback type for resolving how to connect to a browser instance.
///
/// When an instance is requested, this resolver is called first. If it returns
/// `Some(ConnectMode)`, that mode is used instead of the default `connect_mode`.
/// This allows consumers to route certain instances to existing browsers
/// (e.g. "staging" -> connect to a browser already running with debugging enabled)
/// while launching fresh browsers for others.
///
/// Return `None` to fall through to the default `connect_mode`.
///
/// `Arc` for the same reason as [`InstanceOverridesFn`]: resolution probes
/// TCP endpoints and may shell out, so it runs off the state lock.
pub type InstanceResolverFn = Arc<dyn Fn(&str) -> Option<ConnectMode> + Send + Sync>;

/// Per-context WebSocket-route registry map: composite session key →
/// `context.routeWebSocket` handlers in registration order.
pub type ContextWsRoutes = HashMap<String, Vec<(crate::url_matcher::UrlMatcher, crate::web_socket_route::WsHandler)>>;

/// Per-context route registry map: composite session key →
/// `context.route` / `context.routeFromHAR` registrations in order.
/// Entries are cloned onto every page of the context (clones share the
/// `times` budget), so one context-wide counter governs all pages.
pub type ContextRoutes = HashMap<String, Vec<crate::route::RegisteredRoute>>;

/// Per-context init-script registry map: composite session key →
/// `(registry id, lowered script source)` in registration order.
/// Applied to every page of the context — current pages at
/// registration time, future pages by `ContextRef::new_page`.
pub type ContextInitScripts = HashMap<String, Vec<(u64, String)>>;

/// All browser state -- manages multiple Chrome instances, each with contexts and pages.
pub struct BrowserState {
  instances: HashMap<String, BrowserInstance>,
  pending_cleanup: Vec<(String, PendingOwner)>,
  shutdown_generation: u64,
  disposed: bool,
  /// Instance name → browser generation whose popup pump is running
  /// (see [`Self::claim_popup_pump`]).
  popup_pumps: HashMap<String, u64>,
  /// Per-instance launch serialization, read through a shared guard by
  /// [`Self::ensure_instance_shared`]. Sync mutex so the permit can be
  /// taken while only holding the state's read lock.
  launch_permits: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
  /// Monotonic source for [`BrowserInstance::generation`]. Bumped on
  /// every instance (re)creation so consumers can detect a browser
  /// session swap under a reused instance name.
  instance_generation_counter: u64,
  chromium_path: String,
  connect_mode: ConnectMode,
  backend_kind: BackendKind,
  browser_kind: crate::options::BrowserKind,
  launch_timeout: Option<u64>,
  connection_headers: Option<rustc_hash::FxHashMap<String, String>>,
  launch_env: rustc_hash::FxHashMap<String, String>,
  firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  /// Base Chrome flags applied to ALL instances.
  pub extra_args: Vec<String>,
  /// The switch list `launch({ ignoreDefaultArgs })` asked to drop. A
  /// per-instance override replaces it rather than adding to it, the
  /// way every other launch knob here layers.
  pub base_ignore_default_args: Option<crate::options::IgnoreDefaultArgs>,
  /// Proxy every instance this state launches is pointed at
  /// (`browserType.launch({ proxy })`). A context's own `proxy` overrides it,
  /// exactly as it does in Playwright.
  pub proxy: Option<crate::options::ProxyConfig>,
  /// Per-instance additional chrome args. Called with instance name when launching.
  instance_overrides_fn: Option<InstanceOverridesFn>,
  /// Per-instance connect mode resolver. Called before launching to check if
  /// an existing browser should be connected to instead.
  instance_resolver_fn: Option<InstanceResolverFn>,
  /// Instance names a bare session key may select. Empty ⇒ every bare
  /// key is a context on `default`.
  known_instances: KnownInstances,
  /// Whether to run headless.
  pub headless: bool,
  /// Custom user data directory.
  pub user_data_dir: Option<String>,
  /// Default viewport for new pages.
  pub default_viewport: Option<crate::options::ViewportConfig>,
  /// Where a recording writes its loose trace files (Playwright's
  /// `browserType.launch({ tracesDir })`). Unset means a temporary
  /// directory that is discarded once the trace has been exported; a
  /// runner that wants its live traces readable while a test runs points
  /// this at a directory it serves.
  pub traces_dir: Option<std::path::PathBuf>,
  /// Per-context override of [`Self::traces_dir`], keyed by composite
  /// session key.
  ///
  /// A parallel test runner reuses ONE browser across its workers, so
  /// "which directory do this recording's files go in" cannot be a
  /// property of the browser: each worker owns a different artifacts
  /// directory and its live traces have to land there. Sync mutex for
  /// the same reason as `record_video` — the setter is not async.
  pub context_traces_dir: Arc<std::sync::Mutex<HashMap<String, std::path::PathBuf>>>,
  /// Reason passed to the most recent `Browser::close({ reason })` call,
  /// surfaced on `TargetClosed` errors emitted after shutdown.
  close_reason: Option<String>,
  /// Per-context-name event emitter registry. Every [`crate::ContextRef`]
  /// constructed with the same composite session key must share the
  /// same `ContextEventEmitter` so that `context.on('weberror', cb)`
  /// and the per-page → per-context `PageError` → `WebError` bridge
  /// dispatch through the same broadcast channel. The registry is a
  /// sync `std::sync::Mutex` (not tokio) so `ContextRef::new` can
  /// lazily init the entry without needing to own the tokio `RwLock`
  /// guard — `get_or_create_context_events` is called on every
  /// `ContextRef::new` for its composite key.
  pub context_events: Arc<std::sync::Mutex<HashMap<String, crate::events::ContextEventEmitter>>>,
  /// Per-context closed-flag registry. Mirrors `context_events`: every
  /// `ContextRef` constructed with the same composite key shares one
  /// `Arc<AtomicBool>` so `context.close()` on one handle is observed by
  /// `context.isClosed()` on any clone. The flag starts `false` at
  /// context-handle creation (before any page is opened, matching
  /// Playwright's `_closingStatus === 'none'`) and flips `true` on
  /// `ContextRef::close`. Sync mutex so `ContextRef::new` can init the
  /// entry without owning the tokio `RwLock` guard.
  pub context_closed: Arc<std::sync::Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
  /// `testIdAttribute` per composite session key, shared by every
  /// `ContextRef` clone that names the same context — two handles to one
  /// context must not disagree about what `getByTestId` reads. Absent
  /// until something overrides the process default.
  pub context_test_id_attribute: Arc<std::sync::Mutex<HashMap<String, TestIdAttributeSlot>>>,
  /// Per-context `recordVideo` configuration registry. Mirrors
  /// `context_events` above: sync `std::sync::Mutex` so the
  /// non-async setter (`ContextRef::set_record_video`) can write
  /// without the tokio `RwLock` and `register_opened_page` can read
  /// without awaiting. Populated by `ContextRef::set_record_video`;
  /// consumed by `register_opened_page` when attaching a
  /// [`crate::video::Video`] handle to the new page. §4.1's
  /// `BrowserContextOptions` bag will fold this into a single
  /// options struct.
  pub record_video: Arc<std::sync::Mutex<HashMap<String, crate::options::RecordVideoOptions>>>,
  /// Per-context `BrowserContextOptions` registry. Populated by
  /// [`Self::set_context_options`] when the caller constructs a
  /// context via `Browser::new_context(Some(options))`; consumed by
  /// `ContextRef::new_page` to apply emulation/permissions/headers on
  /// every fresh page. Sync mutex so non-async construction paths
  /// can write without owning a tokio guard.
  pub context_options: Arc<std::sync::Mutex<HashMap<String, crate::options::BrowserContextOptions>>>,
  /// Per-context active HAR recorder registry (`tracing.startHar` →
  /// `stopHar`). Keyed by composite session key; an entry exists only
  /// while a recording is in flight. Sync mutex so the start/stop paths
  /// can register/take without owning the tokio `RwLock` guard.
  pub har_recorders: Arc<std::sync::Mutex<HashMap<String, crate::tracing::HarRecorder>>>,
  /// Per-context `routeFromHAR(update: true)` recorder registry. Unlike
  /// `har_recorders` (one explicit `startHar` per context), a context
  /// can accumulate several update recordings; all are flushed by
  /// `ContextRef::close`.
  pub context_har_updates: Arc<std::sync::Mutex<HashMap<String, Vec<crate::tracing::HarRecorder>>>>,
  /// Per-context binding registry — `exposeBinding` / `exposeFunction`
  /// callbacks registered on a [`crate::ContextRef`]. Keyed by composite
  /// session key, then by binding name so a context-level binding
  /// applies to every page in the context (current + future). Consumed
  /// by `ContextRef::new_page` to inject the binding onto each fresh
  /// page. `tokio::sync::RwLock` (not the sync mutex used by the
  /// emitter/options registries) because the stored
  /// [`crate::events::ExposedBinding`] is invoked across `.await`
  /// points and read during async page-open.
  pub context_bindings: Arc<tokio::sync::RwLock<HashMap<String, HashMap<String, crate::events::ExposedBinding>>>>,
  /// Per-context WebSocket-route registry — `context.routeWebSocket`
  /// handlers keyed by composite session key, in registration order.
  /// Consumed by `ContextRef::new_page` so a context-level WS route
  /// applies to every page in the context (current + future), matching
  /// Playwright's context-scoped interception patterns.
  pub context_ws_routes: Arc<tokio::sync::RwLock<ContextWsRoutes>>,
  /// Per-context route registry — `context.route` / `context.routeFromHAR`
  /// registrations keyed by composite session key. Consumed by
  /// `ContextRef::new_page` so a context-level route applies to every
  /// page in the context (current + future) with one shared `times`
  /// budget, matching Playwright's context-scoped `_routes` list.
  pub context_routes: Arc<tokio::sync::RwLock<ContextRoutes>>,
  /// Per-context init-script registry — `context.addInitScript`
  /// registrations keyed by composite session key. Consumed by
  /// `ContextRef::new_page` so a context-level init script reaches
  /// pages created after registration (Playwright context init scripts
  /// are current + future). Also carries the fake-clock engine + call
  /// log, which every new document must replay.
  pub context_init_scripts: Arc<tokio::sync::RwLock<ContextInitScripts>>,
  /// Monotonic id source for `context_init_scripts` entries.
  pub context_init_script_counter: Arc<std::sync::atomic::AtomicU64>,
  /// Composite session keys whose fake clock (`context.clock`) engine
  /// has been installed (`clock.install`-family calls auto-install on
  /// first use, mirroring `server/clock.ts::_installIfNeeded`).
  pub clock_installed: Arc<std::sync::Mutex<rustc_hash::FxHashSet<String>>>,
  /// Sync-readable connection flag mirroring `!instances.is_empty()`.
  /// Set true when an instance is ensured, false on `shutdown`, so
  /// `Browser::is_connected()` stays sync like Playwright's.
  pub connected: Arc<std::sync::atomic::AtomicBool>,
  /// Per-context `storageState` hydration flag. Set the first time a
  /// page opens in a context whose options bag carries a
  /// `storageState` — subsequent pages in the same context skip the
  /// hydration (cookies are context-scoped; localStorage persists per
  /// origin across subsequent pages). Mirrors Playwright's
  /// "set storage state once at context creation".
  pub storage_state_hydrated: Arc<std::sync::Mutex<rustc_hash::FxHashSet<String>>>,
  /// Contexts whose `recordHar` recorder has been installed. Same
  /// once-per-context reason as `storage_state_hydrated`: the option
  /// bag is applied on every page, and the archive covers the context.
  pub record_har_installed: Arc<std::sync::Mutex<rustc_hash::FxHashSet<String>>>,
  /// Set by [`crate::BrowserType::launch_persistent_context`] to mark
  /// this `BrowserState` as backing a persistent-context launch.
  /// When the persistent default context closes, the whole browser
  /// must shut down — Playwright's contract:
  /// "Closing this context will automatically close the browser."
  /// (`/tmp/playwright/packages/playwright-core/types/types.d.ts:15199`).
  pub persistent_context: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectMode {
  Device {
    target: crate::device::DeviceTarget,
    headless: bool,
  },
  /// Launch a new browser (default)
  Launch,
  /// Connect to browser at explicit ws:// or http:// URL
  ConnectUrl(String),
  /// Create a W3C `WebDriver` session and use its negotiated protocol.
  WebDriver {
    endpoint: String,
    browser_name: String,
    protocol: crate::backend::webdriver::WebDriverProtocol,
    capabilities: Option<serde_json::Value>,
    headers: Option<rustc_hash::FxHashMap<String, String>>,
    timeout: Option<u64>,
  },
  /// Auto-connect to running Chrome by reading `DevToolsActivePort` file
  AutoConnect {
    channel: String,
    user_data_dir: Option<String>,
  },
}

/// One context's `testIdAttribute` override, shared by every handle to
/// that context. `None` = whatever the process default is.
pub type TestIdAttributeSlot = Arc<std::sync::RwLock<Option<String>>>;

impl BrowserState {
  /// Construct a `BrowserState` from an internal
  /// [`LaunchPlan`](crate::options::LaunchPlan). This is the single
  /// construction path — there used to be a parallel `new(mode,
  /// backend)` shortcut that hard-coded `headless = false`, which
  /// silently launched full Google Chrome for MCP servers even when
  /// the CLI passed `--headless`. Full Chrome inherits the macOS
  /// system appearance (including `prefers-color-scheme: dark` on
  /// dark-mode hosts), which broke `emulateMedia` reset behaviour
  /// tested via `run_script`. Funneling everyone through `LaunchPlan`
  /// guarantees binary resolution stays aligned with the caller's
  /// headless intent.
  ///
  /// `LaunchPlan` is the internal sister of the public Playwright-
  /// shaped [`crate::options::LaunchOptions`]; the
  /// [`crate::BrowserType`] factory is the only place that builds a
  /// plan from public options.
  #[must_use]
  pub fn with_plan(connect_mode: ConnectMode, plan: crate::options::LaunchPlan) -> Self {
    let chromium_path = plan
      .executable_path
      .unwrap_or_else(|| browser_executable(plan.kind, plan.headless));
    Self {
      instances: HashMap::default(),
      pending_cleanup: Vec::new(),
      shutdown_generation: 0,
      disposed: false,
      instance_generation_counter: 0,
      chromium_path,
      connect_mode,
      backend_kind: plan.backend,
      browser_kind: plan.kind,
      launch_timeout: plan.timeout,
      connection_headers: plan.connection_headers,
      launch_env: plan.env.unwrap_or_default(),
      firefox_user_prefs: plan.firefox_user_prefs,
      extra_args: plan.args,
      base_ignore_default_args: plan.ignore_default_args,
      proxy: plan.proxy,
      instance_overrides_fn: None,
      instance_resolver_fn: None,
      known_instances: Arc::from(Vec::new()),
      headless: plan.headless,
      user_data_dir: plan.user_data_dir,
      default_viewport: plan.default_viewport,
      traces_dir: plan.traces_dir,
      context_traces_dir: Arc::new(std::sync::Mutex::new(HashMap::default())),
      close_reason: None,
      context_events: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_closed: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_test_id_attribute: Arc::new(std::sync::Mutex::new(HashMap::default())),
      record_video: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_options: Arc::new(std::sync::Mutex::new(HashMap::default())),
      har_recorders: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_har_updates: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_bindings: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_ws_routes: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_routes: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_init_scripts: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_init_script_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      clock_installed: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashSet::default())),
      connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      storage_state_hydrated: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashSet::default())),
      record_har_installed: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashSet::default())),
      persistent_context: false,
      popup_pumps: HashMap::default(),
      launch_permits: Arc::new(std::sync::Mutex::new(HashMap::default())),
    }
  }

  /// Whether `instance`'s popup pump is already running for its current
  /// browser generation. Lets the page-open path skip the write lock in
  /// the steady state: every page open used to take the global write
  /// guard purely to re-answer this question, which serialized parallel
  /// workers against each other on a map lookup.
  #[must_use]
  pub(crate) fn popup_pump_claimed(&self, instance: &str) -> bool {
    let Some(inst) = self.instances.get(instance) else {
      return false;
    };
    self.popup_pumps.get(instance) == Some(&inst.generation)
  }

  /// Claim the popup pump for `instance`'s CURRENT browser generation.
  /// Returns the browser handle exactly once per generation — the
  /// caller spawns `context::spawn_popup_pump` with it; later calls
  /// (and calls before the instance exists) return `None`. A relaunch
  /// under the same name bumps the generation, so the next page open
  /// starts a fresh pump against the new browser.
  pub(crate) fn claim_popup_pump(&mut self, instance: &str) -> Option<AnyBrowser> {
    let inst = self.instances.get(instance)?;
    let generation = inst.generation;
    let browser = inst.browser.clone();
    match self.popup_pumps.get(instance) {
      Some(claimed) if *claimed == generation => None,
      _ => {
        self.popup_pumps.insert(instance.to_string(), generation);
        Some(browser)
      },
    }
  }

  /// Resolve which context of `instance` a backend context id belongs
  /// to: `None` → the default context; `Some(id)` → the context whose
  /// registered backend id matches. `None` result = unknown context
  /// (already closed, or created outside this state).
  pub(crate) fn context_name_for_backend_id(&self, instance: &str, backend_ctx_id: Option<&str>) -> Option<String> {
    let inst = self.instances.get(instance)?;
    match backend_ctx_id {
      None => Some("default".to_string()),
      Some(id) => inst
        .contexts
        .iter()
        .find(|(_, ctx)| ctx.cdp_context_id.as_deref() == Some(id))
        .map(|(name, _)| name.clone()),
    }
  }

  /// Find the public wrapper of an open page in `instance` by its
  /// backend target id (CDP targetId / `BiDi` context id / `WebKit`
  /// pageProxyId). Used to wire `page.opener()` for popups.
  pub(crate) fn find_page_by_backend_id(&self, instance: &str, backend_id: &str) -> Option<Arc<crate::page::Page>> {
    let inst = self.instances.get(instance)?;
    inst
      .contexts
      .values()
      .flat_map(|ctx| ctx.pages.iter())
      .find(|page| page.backend_target_id() == backend_id)
      .and_then(|page| page.page_backref_handle().upgrade())
  }

  /// The backend kind this state was constructed with. Cached at
  /// `with_plan` time and never mutated, so callers don't need to
  /// take the outer `RwLock` read guard to ask the question.
  #[must_use]
  pub fn backend_kind(&self) -> BackendKind {
    self.backend_kind
  }

  pub(crate) fn engine_name(&self) -> &'static str {
    match self.browser_kind {
      crate::options::BrowserKind::Chromium => "chromium",
      crate::options::BrowserKind::Firefox => "firefox",
      crate::options::BrowserKind::WebKit | crate::options::BrowserKind::Safari => "webkit",
    }
  }

  /// Claim the one-time `recordHar` registration for a context.
  ///
  /// Context options are applied per page, and the recorder covers the
  /// whole context, so only the first page may install it; a second
  /// would write the same archive twice.
  #[must_use]
  pub fn claim_record_har(&self, composite_key: &str) -> bool {
    let mut set = match self.record_har_installed.lock() {
      Ok(g) => g,
      Err(p) => p.into_inner(),
    };
    set.insert(composite_key.to_string())
  }

  /// Mark a context composite-key as having had its storageState
  /// hydrated. Returns `true` if this is the first call (hydration
  /// should run); `false` if already hydrated.
  #[must_use]
  pub fn claim_storage_state_hydration(&self, composite_key: &str) -> bool {
    let mut set = match self.storage_state_hydrated.lock() {
      Ok(g) => g,
      Err(p) => p.into_inner(),
    };
    set.insert(composite_key.to_string())
  }

  /// Install the [`crate::options::BrowserContextOptions`] bag for a
  /// composite session key. Any fields that also live in the older
  /// per-field registries (currently `record_video`) are mirrored
  /// there too so existing consumers keep working.
  pub fn set_context_options(&self, composite_key: &str, opts: crate::options::BrowserContextOptions) {
    if let Some(ref rv) = opts.record_video {
      self.set_record_video(composite_key, rv.clone());
    }
    let mut map = match self.context_options.lock() {
      Ok(g) => g,
      Err(p) => p.into_inner(),
    };
    map.insert(composite_key.to_string(), opts);
  }

  /// Fetch a clone of the options bag for a composite key, if any.
  #[must_use]
  pub fn get_context_options(&self, composite_key: &str) -> Option<crate::options::BrowserContextOptions> {
    let map = self.context_options.lock().ok()?;
    map.get(composite_key).cloned()
  }

  /// Enable `recordVideo` for every page opened under `composite_key`
  /// (format: `"<instance>:<context>"`). Playwright equivalent:
  /// `browser.newContext({ recordVideo: { dir, size? } })`. Calls
  /// after the setter propagate to pages opened thereafter; pages
  /// already opened in the context do NOT retroactively start
  /// recording (matches Playwright's behaviour of binding the
  /// option at context-creation time).
  pub fn set_record_video(&self, composite_key: &str, opts: crate::options::RecordVideoOptions) {
    let mut map = match self.record_video.lock() {
      Ok(g) => g,
      Err(p) => p.into_inner(),
    };
    map.insert(composite_key.to_string(), opts);
  }

  /// Fetch the `recordVideo` configuration for a composite key, if
  /// any. Returns a clone — the stored options are rarely
  /// mutated after the initial set.
  #[must_use]
  pub fn get_record_video(&self, composite_key: &str) -> Option<crate::options::RecordVideoOptions> {
    let map = self.record_video.lock().ok()?;
    map.get(composite_key).cloned()
  }

  /// Send `composite_key`'s recordings to `dir` instead of the browser's
  /// `tracesDir`. See [`Self::context_traces_dir`].
  pub fn set_context_traces_dir(&self, composite_key: &str, dir: std::path::PathBuf) {
    let mut map = match self.context_traces_dir.lock() {
      Ok(guard) => guard,
      Err(poisoned) => poisoned.into_inner(),
    };
    map.insert(composite_key.to_string(), dir);
  }

  /// Where `composite_key`'s recordings go: its own override, else the
  /// browser's `tracesDir`, else nowhere in particular (a temporary
  /// directory the recorder owns).
  #[must_use]
  pub fn traces_dir_for(&self, composite_key: &str) -> Option<std::path::PathBuf> {
    let scoped = self
      .context_traces_dir
      .lock()
      .map(|map| map.get(composite_key).cloned())
      .unwrap_or_default();
    scoped.or_else(|| self.traces_dir.clone())
  }

  /// Shared handle to the per-context binding registry. Cheap clone of
  /// the `Arc`; callers take the async lock themselves. Used by
  /// `ContextRef::expose_binding` to register and by
  /// `ContextRef::new_page` to re-apply bindings onto a fresh page.
  #[must_use]
  pub fn context_bindings_handle(
    &self,
  ) -> Arc<tokio::sync::RwLock<HashMap<String, HashMap<String, crate::events::ExposedBinding>>>> {
    self.context_bindings.clone()
  }

  /// Shared handle to the per-context WebSocket-route registry. Used by
  /// `ContextRef::route_web_socket` to register and by
  /// `ContextRef::new_page` to apply context-level routes onto a fresh
  /// page.
  #[must_use]
  pub fn context_ws_routes_handle(&self) -> Arc<tokio::sync::RwLock<ContextWsRoutes>> {
    self.context_ws_routes.clone()
  }

  /// Shared handle to the per-context route registry. Used by
  /// `ContextRef::route` / `route_from_har` to register and by
  /// `ContextRef::new_page` to apply context-level routes onto a fresh
  /// page.
  #[must_use]
  pub fn context_routes_handle(&self) -> Arc<tokio::sync::RwLock<ContextRoutes>> {
    self.context_routes.clone()
  }

  /// Look up (or lazily create) the `ContextEventEmitter` for a
  /// composite session key. All `ContextRef` clones with the same key
  /// receive the same emitter so `context.on('weberror', cb)`
  /// observers and the per-page page-error bridge dispatch through
  /// the same broadcast channel.
  #[must_use]
  pub fn get_or_create_context_events(&self, key: &str) -> crate::events::ContextEventEmitter {
    let mut map = match self.context_events.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    map
      .entry(key.to_string())
      .or_insert_with(crate::events::ContextEventEmitter::new)
      .clone()
  }

  /// The shared `testIdAttribute` slot for a composite session key.
  /// See [`Self::context_test_id_attribute`].
  #[must_use]
  pub fn get_or_create_context_test_id_attribute(&self, key: &str) -> TestIdAttributeSlot {
    let mut map = match self.context_test_id_attribute.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    map
      .entry(key.to_string())
      .or_insert_with(|| Arc::new(std::sync::RwLock::new(None)))
      .clone()
  }

  /// Look up (or lazily create) the shared closed-flag for a context's
  /// composite key. See [`Self::context_closed`].
  #[must_use]
  pub fn get_or_create_context_closed(&self, key: &str) -> Arc<std::sync::atomic::AtomicBool> {
    let mut map = match self.context_closed.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    map
      .entry(key.to_string())
      .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
      .clone()
  }

  /// Record the reason given to `Browser::close({ reason })` so downstream
  /// `TargetClosed` errors can carry it through to consumers.
  pub fn set_close_reason(&mut self, reason: String) {
    self.close_reason = Some(reason);
  }

  /// Current close reason, if any.
  #[must_use]
  pub fn close_reason(&self) -> Option<&str> {
    self.close_reason.as_deref()
  }

  /// Set a callback for per-instance launch settings (args, profile
  /// directory, binary, headless, backend, environment). Called with
  /// the instance name before a browser is launched for it; an `Err`
  /// aborts the launch.
  pub fn set_instance_overrides_fn(&mut self, f: InstanceOverridesFn) {
    self.instance_overrides_fn = Some(f);
  }

  /// Set a callback to resolve how to connect to a specific instance.
  ///
  /// When `ensure_instance("name")` is called, the resolver runs first.
  /// If it returns `Some(ConnectMode)`, that mode is used instead of launching.
  /// This decouples browser discovery from ferridriver -- the consumer provides
  /// the discovery logic (reading `DevToolsActivePort` files, querying a registry, etc.).
  pub fn set_instance_resolver_fn(&mut self, f: InstanceResolverFn) {
    self.instance_resolver_fn = Some(f);
  }

  /// Declare the instance names a bare session key may select.
  pub fn set_known_instances(&mut self, names: impl IntoIterator<Item = String>) {
    self.known_instances = names.into_iter().map(String::into_boxed_str).collect();
  }

  /// The declared instance names, for diagnostics and for hosts that
  /// must parse a session key away from the state lock.
  #[must_use]
  pub fn known_instances(&self) -> KnownInstances {
    Arc::clone(&self.known_instances)
  }

  /// Parse a session key against THIS state's instance vocabulary.
  #[must_use]
  pub fn session_key(&self, raw: &str) -> SessionKey {
    SessionKey::parse_with(raw, &self.known_instances)
  }

  // ── Instance management ─────────────────────────────────────────────────

  /// Everything a launch needs from the state, snapshotted so the
  /// launch itself can run without holding the lock.
  fn launch_spec(&self) -> LaunchSpec {
    LaunchSpec {
      backend_kind: self.backend_kind,
      browser_kind: self.browser_kind,
      launch_timeout: self.launch_timeout,
      connection_headers: self.connection_headers.clone(),
      launch_env: self.launch_env.clone(),
      firefox_user_prefs: self.firefox_user_prefs.clone(),
      headless: self.headless,
      chromium_path: self.chromium_path.clone(),
      user_data_dir: self.user_data_dir.clone(),
      connect_mode: self.connect_mode.clone(),
      base_args: self.extra_args.clone(),
      base_ignore_default_args: self.base_ignore_default_args.clone(),
      proxy: self.proxy.clone(),
      default_viewport: self.default_viewport.clone(),
      overrides_fn: self.instance_overrides_fn.clone(),
      resolver_fn: self.instance_resolver_fn.clone(),
    }
  }

  /// Per-instance launch permit. Two sessions racing to first-use the
  /// same instance must not each spawn a browser, and the loser has to
  /// wait for the winner rather than for the global write lock.
  fn launch_permit(&self, instance: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = self
      .launch_permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
      map
        .entry(instance.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
  }

  /// Whether `instance` exists and its browser is still reachable.
  #[must_use]
  pub fn instance_is_live(&self, instance: &str) -> bool {
    self.instances.get(instance).is_some_and(|i| i.browser.is_alive())
  }

  /// Ensure `instance` is up, doing the launch off the state lock.
  ///
  /// [`Self::ensure_instance`] holds the caller's write guard across
  /// the whole launch — process spawn, protocol handshake, and any
  /// configured args/discovery subprocess — which stalls every other
  /// session in the server, including ones on a different browser. This
  /// takes the write lock to register ownership and install the finished instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start or the
  /// connection fails.
  pub async fn ensure_instance_shared(state: &Arc<tokio::sync::RwLock<Self>>, instance: &str) -> Result<()> {
    let permit = {
      let guard = state.read().await;
      guard.ensure_not_disposed()?;
      if guard.instance_is_live(instance) {
        return Ok(());
      }
      guard.launch_permit(instance)
    };
    let held = Arc::clone(&permit).lock_owned().await;
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| FerriError::backend(error.to_string()))?;
    let allocation = {
      let mut guard = state.write().await;
      guard.ensure_not_disposed()?;
      if !Arc::ptr_eq(&permit, &guard.launch_permit(instance)) {
        return Err(allocation::Allocation::closed_error());
      }
      if guard.instance_is_live(instance) {
        return Ok(());
      }
      if guard.instances.contains_key(instance) {
        tracing::warn!(
          target: "ferridriver::state",
          instance,
          "browser instance is gone; relaunching",
        );
      }
      guard.check_cleanup_pending(instance)?;
      guard.prepare_allocation(instance)
    };
    allocation
      .spawn(&runtime, Arc::clone(state), instance.to_owned(), Some(held))
      .await
      .map_err(|error| FerriError::backend(format!("Browser initialization task stopped: {error}")))?
  }

  fn prepare_allocation(&mut self, instance: &str) -> allocation::Start {
    let (generation, owner, completion) = self.begin_allocation(instance);
    allocation::Start {
      spec: self.launch_spec(),
      shutdown_generation: self.shutdown_generation,
      generation,
      owner,
      completion,
    }
  }

  pub(crate) fn start_owned(
    mut self,
    resources: Option<&crate::BrowserResources>,
  ) -> Result<impl std::future::Future<Output = Result<Arc<tokio::sync::RwLock<Self>>>> + Send + 'static> {
    self.ensure_not_disposed()?;
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| FerriError::backend(error.to_string()))?;
    let allocation = self.prepare_allocation("default");
    let state = Arc::new(tokio::sync::RwLock::new(self));
    if let Some(resources) = resources {
      resources.retain(Arc::clone(&state))?;
    }
    let completed = allocation.spawn(&runtime, Arc::clone(&state), "default".into(), None);
    Ok(async move {
      completed
        .await
        .map_err(|error| FerriError::backend(format!("Browser initialization task stopped: {error}")))??;
      Ok(state)
    })
  }

  pub(crate) fn ensure_not_disposed(&self) -> Result<()> {
    if self.disposed {
      Err(FerriError::target_closed(Some(
        "browser resources have been disposed".into(),
      )))
    } else {
      Ok(())
    }
  }

  fn begin_allocation(&mut self, instance: &str) -> (u64, Arc<allocation::Allocation>, allocation::Completion) {
    let generation = self.next_instance_generation();
    let (allocation, completion) = allocation::Allocation::new();
    self.pending_cleanup.push((
      instance.to_owned(),
      PendingOwner::Allocation {
        generation,
        owner: Arc::clone(&allocation),
      },
    ));
    (generation, allocation, completion)
  }

  async fn finish_allocation(
    state: &Arc<tokio::sync::RwLock<Self>>,
    instance: &str,
    shutdown_generation: u64,
    generation: u64,
    allocation: &allocation::Allocation,
    created: Result<ConnectMode>,
  ) -> Result<()> {
    let mut guard = state.write().await;
    if guard.shutdown_generation != shutdown_generation || allocation.is_closing() {
      return Err(allocation::Allocation::closed_error());
    }
    Box::pin(guard.install_allocation(instance, generation, allocation, created)).await
  }

  async fn install_allocation(
    &mut self,
    instance: &str,
    generation: u64,
    allocation: &allocation::Allocation,
    created: Result<ConnectMode>,
  ) -> Result<()> {
    let index = self.pending_instance_index(generation)?;
    let mode = match created {
      Ok(mode) => mode,
      Err(error) => {
        if !allocation.has_partial_resources() {
          self.pending_cleanup.swap_remove(index);
        }
        return Err(error);
      },
    };
    self.pending_cleanup[index].1 = BrowserInstance {
      browser: allocation.take_browser().await?,
      contexts: HashMap::default(),
      generation,
    }
    .into();
    let install = Box::pin(self.finish_install_instance(
      instance,
      generation,
      !matches!(mode, ConnectMode::Launch),
      matches!(mode, ConnectMode::WebDriver { .. } | ConnectMode::Device { .. }),
    ));
    if let Some(budget) = allocation.connection_budget() {
      budget
        .scope(budget.wait(install))
        .await
        .map_err(|error| error.error("adopting browser pages"))?
    } else {
      install.await
    }
  }

  /// Evict a dead entry, adopt existing pages when connecting, and
  /// register `browser` as `instance_name`.
  fn install_instance(
    &mut self,
    instance_name: &str,
    browser: AnyBrowser,
    adopt_pages: bool,
    preserve_viewport: bool,
  ) -> impl std::future::Future<Output = Result<()>> {
    let generation = self.next_instance_generation();
    // Registration must survive even when the returned future is never polled.
    self.pending_cleanup.push((
      instance_name.to_owned(),
      BrowserInstance {
        browser,
        contexts: HashMap::default(),
        generation,
      }
      .into(),
    ));
    self.finish_install_instance(instance_name, generation, adopt_pages, preserve_viewport)
  }

  async fn finish_install_instance(
    &mut self,
    instance_name: &str,
    generation: u64,
    adopt_pages: bool,
    preserve_viewport: bool,
  ) -> Result<()> {
    if let Some(instance) = self.instances.remove(instance_name) {
      self.pending_cleanup.push((instance_name.to_owned(), instance.into()));
    }
    self
      .connected
      .store(!self.instances.is_empty(), std::sync::atomic::Ordering::Relaxed);
    self
      .cleanup_instances_except(Some(instance_name), Some(generation))
      .await?;
    if adopt_pages {
      let viewport = if preserve_viewport {
        None
      } else {
        self.default_viewport.clone()
      };
      let index = self.pending_instance_index(generation)?;
      let instance = self.pending_cleanup[index]
        .1
        .instance_mut()
        .ok_or_else(|| FerriError::backend("browser is not ready for adoption"))?;
      if let Err(error) = Self::adopt_instance_pages(instance_name, instance, viewport.as_ref()).await {
        return match self.cleanup_instances(Some(instance_name)).await {
          Ok(()) => Err(error),
          Err(cleanup) => Err(FerriError::backend(format!(
            "{error}; browser cleanup failed: {cleanup}"
          ))),
        };
      }
    }
    let index = self.pending_instance_index(generation)?;
    let (name, owner) = self.pending_cleanup.swap_remove(index);
    match owner {
      PendingOwner::Browser(instance) => {
        self.instances.insert(name, *instance);
      },
      owner @ PendingOwner::Allocation { .. } => {
        self.pending_cleanup.push((name, owner));
        return Err(FerriError::backend("browser is not ready for installation"));
      },
    }
    self.connected.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
  }

  fn pending_instance_index(&self, generation: u64) -> Result<usize> {
    self
      .pending_cleanup
      .iter()
      .position(|(_, instance)| instance.generation() == generation)
      .ok_or_else(|| FerriError::backend("incoming browser disappeared during installation"))
  }

  async fn adopt_instance_pages(
    instance_name: &str,
    inst: &mut BrowserInstance,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<()> {
    // Adopt existing pages into the "default" context of this instance.
    // For launch mode, pages are created on demand by the caller (the
    // test runner creates isolated contexts, MCP creates pages lazily).
    //
    // An adopted page is emulated with the configured viewport, exactly
    // as a page this instance opened would be. The browser on the other
    // end of a discover command is one this config declares and both
    // sides share, not a stranger's: a browser reached through a
    // persistent profile comes up at whatever window bounds Chrome
    // restored from the profile's last run, so adopting a page without
    // emulating anything hands every later call that stale size.
    // `viewport: null` is how a config asks to leave it alone; the
    // explicit `connect` tool ([`Self::connect_to_url`]) never touches
    // it, matching Playwright's `connectOverCDP`.
    let existing_pages = Box::pin(inst.browser.pages()).await?;
    let ctx = inst.context_mut("default");
    for page in existing_pages {
      page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
      ctx.pages.push(page);
    }
    if let Some(vp) = viewport {
      for page in &inst.context_mut("default").pages {
        if let Err(e) = page.emulate_viewport(vp).await {
          tracing::warn!(
            target: "ferridriver::state",
            instance = instance_name,
            error = %e,
            "could not emulate the configured viewport on an adopted page",
          );
        }
      }
    }
    Ok(())
  }

  fn check_cleanup_pending(&self, name: &str) -> Result<()> {
    if self.pending_cleanup.iter().any(|(pending, _)| pending == name) {
      return Err(FerriError::backend(format!(
        "instance '{name}' has unfinished cleanup; retry closing it"
      )));
    }
    Ok(())
  }

  async fn evict_instance(&mut self, instance_name: &str) -> Result<()> {
    if let Some(instance) = self.instances.remove(instance_name) {
      self.pending_cleanup.push((instance_name.to_owned(), instance.into()));
    }
    self.cleanup_instances(Some(instance_name)).await
  }

  async fn cleanup_instances(&mut self, only: Option<&str>) -> Result<()> {
    self.cleanup_instances_except(only, None).await
  }

  async fn cleanup_instances_except(&mut self, only: Option<&str>, retained_generation: Option<u64>) -> Result<()> {
    let mut errors = Vec::new();
    let mut index = 0;
    while index < self.pending_cleanup.len() {
      let (name, instance) = &self.pending_cleanup[index];
      if only.is_some_and(|only| only != name) || retained_generation == Some(instance.generation()) {
        index += 1;
        continue;
      }
      let mut composites = Vec::new();
      if let Some(instance) = instance.instance() {
        composites.extend(instance.contexts.keys().map(|context| format!("{name}:{context}")));
        for context in instance.contexts.values() {
          for page in &context.pages {
            page.dispose_local();
          }
        }
      }
      for composite in composites {
        self.purge_context_registries(&composite).await;
      }
      let (name, instance) = &mut self.pending_cleanup[index];
      let result = match instance {
        PendingOwner::Browser(instance) => {
          instance.contexts.clear();
          instance.browser.close().await
        },
        PendingOwner::Allocation { owner, .. } => owner.close().await,
      };
      match result {
        Ok(()) => {
          self.pending_cleanup.swap_remove(index);
        },
        Err(error) => {
          errors.push(format!("{name}: {error}"));
          index += 1;
        },
      }
    }
    if errors.is_empty() {
      Ok(())
    } else {
      Err(FerriError::backend(errors.join("; ")))
    }
  }
}

/// Immutable snapshot of the launch-relevant state, taken under a read
/// guard so the launch runs unlocked.
struct LaunchSpec {
  backend_kind: BackendKind,
  browser_kind: crate::options::BrowserKind,
  launch_timeout: Option<u64>,
  connection_headers: Option<rustc_hash::FxHashMap<String, String>>,
  launch_env: rustc_hash::FxHashMap<String, String>,
  firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  headless: bool,
  chromium_path: String,
  user_data_dir: Option<String>,
  connect_mode: ConnectMode,
  base_args: Vec<String>,
  base_ignore_default_args: Option<crate::options::IgnoreDefaultArgs>,
  proxy: Option<crate::options::ProxyConfig>,
  default_viewport: Option<crate::options::ViewportConfig>,
  overrides_fn: Option<InstanceOverridesFn>,
  resolver_fn: Option<InstanceResolverFn>,
}

/// One instance's launch inputs after per-instance overrides are applied
/// to the state's base plan.
#[derive(Debug)]
struct EffectiveLaunch {
  firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  backend_kind: BackendKind,
  browser_kind: crate::options::BrowserKind,
  launch_timeout: Option<u64>,
  headless: bool,
  chromium_path: String,
  user_data_dir: Option<String>,
  args: Vec<String>,
  env: rustc_hash::FxHashMap<String, String>,
  ignore_default_args: Option<crate::options::IgnoreDefaultArgs>,
  proxy: Option<crate::options::ProxyConfig>,
}

impl EffectiveLaunch {
  /// Refuse `ignoreDefaultArgs` on a backend that has no default switch
  /// list to drop.
  ///
  /// Only the Chromium launch path injects a switch set
  /// ([`CHROMIUM_SWITCHES`]); the Firefox and `WebKit` paths pass just the
  /// structural transport flags (`--remote-debugging-port`, `--profile`,
  /// `--inspector-pipe`, `--user-data-dir`), which are not defaults a
  /// caller may drop without breaking the connection. Saying so is the
  /// point: accepting the option and applying it nowhere is how a caller
  /// believes a switch was dropped when it never was.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Unsupported`] naming the backend.
  fn reject_ignore_default_args(&self, backend: &str) -> Result<()> {
    if self.ignore_default_args.is_none() {
      return Ok(());
    }
    Err(FerriError::unsupported(format!(
      "ignoreDefaultArgs is not supported on the {backend} backend: ferridriver injects no default \
       switch list there, only the transport flags the connection needs. Drop the option, or set it \
       on the Chromium instances only."
    )))
  }
}

impl LaunchSpec {
  /// Resolve the connect mode and the effective launch inputs for
  /// `instance`.
  ///
  /// Both callbacks are operator-supplied and routinely spawn a
  /// subprocess or probe a TCP port, so they run on the blocking pool:
  /// on an async worker they stall unrelated tasks, and the args
  /// command for an operator-supplied gateway instance takes hundreds of ms.
  ///
  /// # Errors
  ///
  /// Propagates an overrides-callback error, which aborts the launch
  /// instead of silently starting an unconfigured browser.
  async fn resolve_off_lock(&self, instance: &str) -> Result<(ConnectMode, EffectiveLaunch)> {
    let overrides_fn = self.overrides_fn.clone();
    let resolver_fn = self.resolver_fn.clone();
    let name = instance.to_string();

    // With no callbacks configured (the test runner, plain `launch()`)
    // there is nothing to run off-thread; skip the blocking-pool hop.
    let (overrides, mode) = if overrides_fn.is_none() && resolver_fn.is_none() {
      (crate::options::InstanceOverrides::default(), None)
    } else {
      let joined = tokio::task::spawn_blocking(move || {
        let overrides = overrides_fn.map_or_else(|| Ok(crate::options::InstanceOverrides::default()), |f| f(&name));
        let mode = resolver_fn.and_then(|f| f(&name));
        (overrides, mode)
      })
      .await;
      match joined {
        Ok((Ok(overrides), mode)) => (overrides, mode),
        Ok((Err(message), _)) => return Err(FerriError::backend(message)),
        Err(e) => {
          return Err(FerriError::backend(format!(
            "instance overrides/resolver task failed for '{instance}': {e}"
          )));
        },
      }
    };

    let mode = mode.unwrap_or_else(|| self.connect_mode.clone());
    let selection = crate::options::BrowserSelection::resolve(
      overrides
        .kind
        .or_else(|| overrides.backend.is_none().then_some(self.browser_kind)),
      overrides
        .backend
        .or_else(|| overrides.kind.is_none().then_some(self.backend_kind)),
    )?;
    let browser_kind = selection.browser;
    let headless = overrides.headless.unwrap_or(self.headless);
    let mut args = self.base_args.clone();
    args.extend(overrides.args);

    // Match the browser window to the viewport unless the caller
    // already pinned a size.
    if !matches!(&mode, ConnectMode::Device { .. })
      && browser_kind != crate::options::BrowserKind::Safari
      && selection.backend != BackendKind::WebKit
      && !args.iter().any(|a| a.starts_with("--window-size"))
      && let Some(ref vp) = self.default_viewport
    {
      args.push(format!("--window-size={},{}", vp.width, vp.height));
    }

    let mut env = self.launch_env.clone();
    env.extend(overrides.env);
    let effective = EffectiveLaunch {
      firefox_user_prefs: self.firefox_user_prefs.clone(),
      backend_kind: selection.backend,
      browser_kind,
      launch_timeout: self.launch_timeout,
      headless,
      chromium_path: overrides.executable_path.unwrap_or_else(|| {
        if browser_kind == self.browser_kind {
          self.chromium_path.clone()
        } else {
          browser_executable(browser_kind, headless)
        }
      }),
      user_data_dir: overrides.user_data_dir.or_else(|| self.user_data_dir.clone()),
      args,
      env,
      ignore_default_args: overrides
        .ignore_default_args
        .or_else(|| self.base_ignore_default_args.clone()),
      proxy: self.proxy.clone(),
    };
    Ok((mode, effective))
  }

  async fn launch_browser(&self, eff: &EffectiveLaunch) -> Result<AnyBrowser> {
    if eff.browser_kind == crate::options::BrowserKind::Safari {
      return launch_safari(eff).await;
    }
    // `launch({ proxy })` is a per-process setting on every engine that has
    // one, so it is lowered here rather than at context creation: a context
    // that names no proxy of its own inherits it.
    let proxy_flags = eff
      .proxy
      .as_ref()
      .map(|proxy| proxy.launch_flags(eff.backend_kind))
      .unwrap_or_default();

    Ok(match eff.backend_kind {
      BackendKind::WebDriver => {
        return Err(FerriError::unsupported(
          "Local Classic WebDriver launching currently requires Safari; use a WebDriver endpoint for other installed drivers",
        ));
      },
      BackendKind::CdpPipe => {
        use crate::backend::cdp::{CdpBrowser, pipe::PipeTransport};
        let mut args = eff.args.clone();
        args.extend(proxy_flags);
        let flags = chrome_flags_with(eff.headless, &args, eff.ignore_default_args.as_ref());
        let browser = match &eff.user_data_dir {
          Some(dir) => {
            CdpBrowser::<PipeTransport>::launch_with_flags_in_dir(
              &eff.chromium_path,
              &flags,
              std::path::Path::new(dir),
              &eff.env,
            )
            .await?
          },
          None => CdpBrowser::<PipeTransport>::launch_with_flags(&eff.chromium_path, &flags, &eff.env).await?,
        };
        AnyBrowser::CdpPipe(browser)
      },
      BackendKind::CdpWs => {
        use crate::backend::cdp::{CdpBrowser, ws::WsTransport};
        let mut args = eff.args.clone();
        args.extend(proxy_flags);
        let flags = chrome_flags_with(eff.headless, &args, eff.ignore_default_args.as_ref());
        let browser = match &eff.user_data_dir {
          Some(dir) => {
            Box::pin(CdpBrowser::<WsTransport>::launch_with_flags_in_dir(
              &eff.chromium_path,
              &flags,
              std::path::Path::new(dir),
              &eff.env,
            ))
            .await?
          },
          None => CdpBrowser::<WsTransport>::launch_with_flags(&eff.chromium_path, &flags, &eff.env).await?,
        };
        AnyBrowser::CdpWs(browser)
      },
      BackendKind::WebKit => {
        use crate::backend::webkit::{LaunchConfig, WebKitBrowser};
        eff.reject_ignore_default_args("webkit")?;
        let config = LaunchConfig {
          executable_path: (!eff.chromium_path.is_empty()).then(|| std::path::PathBuf::from(&eff.chromium_path)),
          headless: eff.headless,
          env: eff.env.clone(),
          user_data_dir: eff.user_data_dir.as_ref().map(std::path::PathBuf::from),
          extra_args: eff.args.clone(),
          proxy_server: eff.proxy.as_ref().map(|p| p.server.clone()),
          proxy_bypass_list: eff.proxy.as_ref().and_then(|p| p.bypass.clone()),
        };
        AnyBrowser::WebKit(Box::pin(WebKitBrowser::launch(&config)).await?)
      },
      BackendKind::Bidi => {
        use crate::backend::bidi::BidiBrowser;
        eff.reject_ignore_default_args("bidi")?;
        let mut flags = eff.args.clone();
        if eff.headless {
          flags.push("--headless".into());
        }
        // Firefox takes no proxy switch: the proxy is a session capability,
        // so it goes into `session.new` rather than onto the command line.
        AnyBrowser::Bidi(
          Box::pin(BidiBrowser::launch_with_flags(
            &eff.chromium_path,
            &flags,
            &eff.env,
            eff.user_data_dir.as_deref().map(std::path::Path::new),
            eff.proxy.as_ref(),
            eff.firefox_user_prefs.as_ref(),
          ))
          .await?,
        )
      },
    })
  }
}

async fn launch_safari(eff: &EffectiveLaunch) -> Result<AnyBrowser> {
  eff.reject_ignore_default_args("safari")?;
  if eff.headless || eff.user_data_dir.is_some() || !eff.args.is_empty() || eff.proxy.is_some() {
    return Err(FerriError::unsupported(
      "Safari does not support headless mode, custom profiles, browser arguments, or a launch proxy",
    ));
  }
  if eff.backend_kind == BackendKind::WebDriver {
    Ok(AnyBrowser::WebDriver(
      Box::pin(crate::backend::webdriver::browser::WebDriverBrowser::launch_safari(
        &eff.env,
        eff.launch_timeout,
      ))
      .await?,
    ))
  } else {
    Ok(AnyBrowser::Bidi(
      Box::pin(crate::backend::bidi::BidiBrowser::launch_safari(
        &eff.env,
        eff.launch_timeout,
      ))
      .await?,
    ))
  }
}

fn browser_executable(kind: crate::options::BrowserKind, headless: bool) -> String {
  match kind {
    crate::options::BrowserKind::Chromium => resolve_chromium(headless),
    crate::options::BrowserKind::Firefox => std::env::var("FIREFOX_PATH")
      .ok()
      .or_else(|| detect_firefox().ok())
      .unwrap_or_else(|| "firefox".to_owned()),
    crate::options::BrowserKind::Safari => "/usr/bin/safaridriver".to_owned(),
    crate::options::BrowserKind::WebKit => String::new(),
  }
}

/// Ask a resolver about the full session key, then about the instance
/// half of a composite key (`"staging:default"` → `"staging"`).
fn resolve_with_prefix(resolver: &InstanceResolverFn, instance_name: &str) -> Option<ConnectMode> {
  if let Some(mode) = resolver(instance_name) {
    return Some(mode);
  }
  let prefix = instance_name.split(':').next()?;
  if prefix == instance_name {
    return None;
  }
  resolver(prefix)
}

async fn connect_classic(mode: &ConnectMode) -> Result<AnyBrowser> {
  let browser = match mode {
    ConnectMode::WebDriver {
      endpoint,
      browser_name,
      capabilities,
      headers,
      timeout,
      ..
    } => {
      crate::backend::webdriver::browser::WebDriverBrowser::connect(
        endpoint,
        browser_name,
        capabilities.as_ref(),
        headers.as_ref(),
        *timeout,
      )
      .await?
    },
    ConnectMode::ConnectUrl(endpoint) => {
      crate::backend::webdriver::browser::WebDriverBrowser::connect(endpoint, "safari", None, None, None).await?
    },
    _ => {
      return Err(FerriError::invalid_argument(
        "endpoint",
        "Classic WebDriver requires an HTTP endpoint",
      ));
    },
  };
  Ok(AnyBrowser::WebDriver(browser))
}

/// Attach to a browser someone else is running. CDP uses discovery for HTTP
/// endpoints; `BiDi` accepts either its direct WebSocket endpoint or a W3C
/// `WebDriver` endpoint that negotiates `webSocketUrl` during session creation.
async fn connect_browser(
  mode: &ConnectMode,
  backend_kind: BackendKind,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  args: &[String],
) -> Result<AnyBrowser> {
  use crate::backend::cdp::{CdpBrowser, ws::WsTransport};

  if let ConnectMode::Device { target, headless } = mode {
    return match target {
      crate::device::DeviceTarget::Android(options) => crate::android::launch(options, *headless, args).await,
      crate::device::DeviceTarget::Ios(options) => Box::pin(crate::ios::launch(options, *headless)).await,
    };
  }
  if let ConnectMode::WebDriver {
    endpoint,
    browser_name,
    protocol,
    capabilities,
    headers,
    timeout,
  } = mode
  {
    return Box::pin(crate::backend::webdriver::connect(
      endpoint,
      browser_name,
      capabilities.as_ref(),
      headers.as_ref(),
      *timeout,
      *protocol,
    ))
    .await;
  }
  if backend_kind == BackendKind::WebDriver {
    return connect_classic(mode).await;
  }
  if backend_kind == BackendKind::Bidi {
    let endpoint = match mode {
      ConnectMode::ConnectUrl(endpoint) if endpoint.starts_with("http://") || endpoint.starts_with("https://") => {
        return Ok(AnyBrowser::Bidi(
          Box::pin(crate::backend::bidi::BidiBrowser::connect_webdriver(
            endpoint, "firefox", None, None, None,
          ))
          .await?,
        ));
      },
      ConnectMode::ConnectUrl(endpoint) => endpoint,
      _ => return Err(FerriError::unsupported("WebDriver BiDi requires a WebSocket endpoint")),
    };
    return Ok(AnyBrowser::Bidi(
      Box::pin(crate::backend::bidi::BidiBrowser::connect_with_headers(
        endpoint, headers,
      ))
      .await?,
    ));
  }
  let ws_url = match mode {
    ConnectMode::ConnectUrl(url) if url.starts_with("ws://") || url.starts_with("wss://") => url.clone(),
    ConnectMode::ConnectUrl(url) => discover_ws_with_headers(url, headers).await?,
    ConnectMode::AutoConnect { channel, user_data_dir } => discover_chrome_ws(channel, user_data_dir.as_deref())?,
    ConnectMode::Device { .. } | ConnectMode::WebDriver { .. } => {
      return Err(FerriError::unsupported(
        "WebDriver HTTP sessions require the BiDi backend",
      ));
    },
    ConnectMode::Launch => return Err(FerriError::backend("connect_browser called with Launch mode")),
  };
  Ok(AnyBrowser::CdpWs(
    Box::pin(CdpBrowser::<WsTransport>::connect_with_headers(
      &ws_url,
      &headers
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect(),
    ))
    .await?,
  ))
}

impl BrowserState {
  /// Ensure a browser instance is launched. If it already exists, no-op.
  ///
  /// Holds the caller's `&mut` borrow for the whole launch; server code
  /// that shares the state behind an `RwLock` should call
  /// [`Self::ensure_instance_shared`] instead.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start or connection fails.
  pub async fn ensure_instance(&mut self, instance_name: &str) -> Result<()> {
    self.ensure_not_disposed()?;
    self.check_cleanup_pending(instance_name)?;
    if self.instance_is_live(instance_name) {
      return Ok(());
    }
    if self.instances.contains_key(instance_name) {
      // The browser died (crash, OOM-kill, external SIGKILL). Without this
      // the entry stays and every later session routed to this name gets
      // `TargetClosed` forever, because the only thing that ever removed
      // an instance was an explicit shutdown.
      tracing::warn!(
        target: "ferridriver::state",
        instance = instance_name,
        "browser instance is gone; relaunching",
      );
    }

    let runtime = tokio::runtime::Handle::try_current().map_err(|error| FerriError::backend(error.to_string()))?;
    let spec = self.launch_spec();
    let (generation, allocation, completion) = self.begin_allocation(instance_name);
    let owner = Arc::clone(&allocation);
    let name = instance_name.to_owned();
    let (reply, completed) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
      let result = owner.produce(spec, &name).await;
      drop(completion);
      if let Err(Err(error)) = reply.send(result) {
        tracing::debug!(%error, instance = name, "Browser allocation finished after its caller disconnected");
      }
    });
    let created = completed
      .await
      .map_err(|error| FerriError::backend(format!("Browser allocation task stopped: {error}")))?;
    Box::pin(self.install_allocation(instance_name, generation, &allocation, created)).await
  }

  /// Backwards-compat: ensure the "default" instance.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start.
  pub async fn ensure_browser(&mut self) -> Result<()> {
    Box::pin(self.ensure_instance("default")).await
  }

  /// Connect to a running browser at the given WebSocket or HTTP URL.
  /// Creates a new instance with the given name using `CdpWs` backend.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection or page discovery fails.
  pub async fn connect_to_url(&mut self, instance_name: &str, url: &str) -> Result<usize> {
    use crate::backend::cdp::{CdpBrowser, ws::WsTransport};
    self.ensure_not_disposed()?;

    // Drop existing instance if any. A bare `remove` would orphan a browser this
    // server launched, leaving the process alive with no handle to close it.
    self.evict_instance(instance_name).await?;

    let ws_url = if url.starts_with("ws://") || url.starts_with("wss://") {
      url.to_string()
    } else {
      discover_ws_from_http(url).await?
    };

    let browser = AnyBrowser::CdpWs(Box::pin(CdpBrowser::<WsTransport>::connect(&ws_url)).await?);
    // Skip viewport override for existing pages — connect_to_url attaches to a
    // user-managed browser whose window size should not be touched.
    Box::pin(self.install_instance(instance_name, browser, true, true)).await?;
    Ok(self.instance(instance_name)?.context("default")?.pages.len())
  }

  /// Auto-discover and connect to a running Chrome instance.
  ///
  /// Checks the instance resolver first (allowing consumers to route
  /// `instance_name` to a managed browser, e.g. connecting to an
  /// environment-specific browser launched by another tool). Falls back
  /// to reading Chrome's `DevToolsActivePort` file for the given
  /// channel/profile.
  ///
  /// The resolver is tried with the full `instance_name` first, then with
  /// the prefix before `:` (supporting composite keys like `"staging:default"`
  /// where only the first segment identifies the browser instance).
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome discovery or connection fails.
  pub async fn connect_auto(
    &mut self,
    instance_name: &str,
    channel: &str,
    user_data_dir: Option<&str>,
  ) -> Result<usize> {
    if let Some(resolved) = self.resolve_via_instance_fn_off_thread(instance_name).await {
      return self.connect_with_resolved_mode(instance_name, resolved).await;
    }

    let ws_url = discover_chrome_ws(channel, user_data_dir)?;
    Box::pin(self.connect_to_url(instance_name, &ws_url)).await
  }

  /// [`Self::resolve_via_instance_fn`] on the blocking pool.
  ///
  /// The resolver shells out (a discover command may poll for a browser)
  /// and TCP-probes endpoints. The launch path already moved it off the
  /// reactor; this path called it inline, so one cold `connect` stalled
  /// every other task on that worker thread.
  async fn resolve_via_instance_fn_off_thread(&self, instance_name: &str) -> Option<ConnectMode> {
    let resolver = self.instance_resolver_fn.clone()?;
    let name = instance_name.to_string();
    match tokio::task::spawn_blocking(move || resolve_with_prefix(&resolver, &name)).await {
      Ok(mode) => mode,
      Err(e) => {
        tracing::warn!(target: "ferridriver::state", error = %e, "instance resolver task failed");
        None
      },
    }
  }

  /// Connect using a resolved mode from the instance resolver.
  async fn connect_with_resolved_mode(&mut self, instance_name: &str, mode: ConnectMode) -> Result<usize> {
    match mode {
      ConnectMode::ConnectUrl(url) => Box::pin(self.connect_to_url(instance_name, &url)).await,
      ConnectMode::Device { .. } | ConnectMode::WebDriver { .. } => Err(FerriError::unsupported(
        "connect_to_url cannot be used with a WebDriver session mode",
      )),
      ConnectMode::AutoConnect { channel, user_data_dir } => {
        let ws_url = discover_chrome_ws(&channel, user_data_dir.as_deref())?;
        Box::pin(self.connect_to_url(instance_name, &ws_url)).await
      },
      // Launch mode is not meaningful for connect_auto -- the caller should
      // launch a browser separately and use ConnectUrl for the result.
      ConnectMode::Launch => Err(FerriError::Backend(format!(
        "Instance resolver returned Launch mode for '{instance_name}', expected ConnectUrl"
      ))),
    }
  }

  // ── Routing helpers ─────────────────────────────────────────────────────

  fn instance(&self, name: &str) -> Result<&BrowserInstance> {
    self.instances.get(name).ok_or_else(|| {
      FerriError::invalid_argument(
        "instance",
        format!("'{name}' not found. It will be created on first use."),
      )
    })
  }

  /// Access the default instance's backend browser handle. Used by
  /// `Browser::version()` to read the cached CDP `Browser.getVersion().product`.
  pub(crate) fn default_browser(&self) -> Option<&AnyBrowser> {
    self.instance_browser("default")
  }

  pub(crate) fn instance_browser(&self, instance: &str) -> Option<&AnyBrowser> {
    self.instances.get(instance).map(|i| &i.browser)
  }

  fn instance_mut(&mut self, name: &str) -> Result<&mut BrowserInstance> {
    self
      .instances
      .get_mut(name)
      .ok_or_else(|| FerriError::invalid_argument("instance", format!("'{name}' not found")))
  }

  // ── Public methods (all parse composite keys) ───────────────────────────

  /// Open a new page in a context. `context` is a composite key like `"staging:admin"`.
  ///
  /// # Errors
  ///
  /// Returns an error if the instance or page creation fails.
  /// Create a new page in the given context. Returns the `AnyPage` directly
  /// (no second lookup needed).
  pub async fn open_page(&mut self, context: &str, url: &str) -> Result<AnyPage> {
    let key = self.session_key(context);
    Box::pin(self.open_page_keyed(&key, url)).await
  }

  /// Snapshot the immutable data needed to open a page without holding the
  /// global browser-state write lock across protocol round-trips.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance does not exist.
  pub fn page_open_plan(&self, key: &SessionKey) -> Result<PageOpenPlan> {
    self.ensure_not_disposed()?;
    let inst = self.instance(&key.instance)?;
    let browser_context_id = if &*key.context == "default" {
      None
    } else {
      inst
        .contexts
        .get(&*key.context)
        .and_then(|ctx| ctx.cdp_context_id.clone())
    };

    Ok(PageOpenPlan {
      browser: inst.browser.clone(),
      viewport: self.default_viewport.clone(),
      browser_context_id,
    })
  }

  /// Register a newly created page back into the state after off-lock backend work.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance or context does not exist.
  pub fn register_opened_page(
    &mut self,
    key: &SessionKey,
    page: AnyPage,
    browser_context_id: Option<String>,
  ) -> Result<()> {
    if let Err(error) = self.ensure_not_disposed() {
      page.dispose_local();
      return Err(error);
    }
    // Pull the context-event emitter for this session key BEFORE
    // taking the mutable instance borrow, so we can hand it to the
    // per-page → per-context `PageError` → `WebError` bridge spawned
    // below. Every `ContextRef` cloned with the same composite key
    // receives the same emitter via the registry, so listeners
    // registered anywhere (NAPI `context.on('weberror')`, QuickJS
    // `context.waitForEvent('weberror')`, etc.) all observe events
    // fanned out here.
    let composite = key.to_composite();
    let context_events = self.get_or_create_context_events(&composite);

    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut(&key.context);
    // Opening a tab is the one moment a `&mut` on the context is
    // guaranteed, so it is where the tabs the browser already closed get
    // dropped. Otherwise a long session's `pages` only ever grows and
    // every command scans past the corpses.
    ctx.prune_closed_pages();
    if let Some(id) = browser_context_id {
      ctx.cdp_context_id = Some(id);
    }
    page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());

    // Spawn the page→context bridge exactly once per registered page.
    // Runs independently of any `ContextRef` or `Page` wrapper
    // lifetime — forwards as long as the page's emitter stays alive.
    // Holds only the subscription and the weak backref slot: a strong
    // `AnyPage` clone here would pin the page's `EventEmitter`, so
    // this task's own `recv` could never observe the close and the
    // whole backend page (CDP session, managers, listener tasks)
    // leaked per open/close cycle.
    let mut rx = page.events().subscribe();
    let backref = page.page_backref_handle();
    tokio::spawn(async move {
      use crate::events::{ContextEvent, PageEvent};
      while let Some((event, sequence)) = rx.recv_stamped().await {
        let emit = |event| context_events.emit_stamped(event, sequence);
        // Frame- and page-lifecycle mirror events need the public
        // wrapper `Arc<Page>` so the binding can mint a `Frame` /
        // deliver a `Page`. Upgrade lazily; skip if every wrapper has
        // been dropped (page is going away).
        let upgrade = || backref.upgrade();
        match event {
          PageEvent::PageError(err) => emit(ContextEvent::WebError(err)),
          PageEvent::Download(d) => emit(ContextEvent::Download(d)),
          PageEvent::FrameAttached(info) => {
            if let Some(page) = upgrade() {
              emit(ContextEvent::FrameAttached {
                page,
                frame_id: info.frame_id,
              });
            }
          },
          PageEvent::FrameDetached { frame_id } => {
            if let Some(page) = upgrade() {
              emit(ContextEvent::FrameDetached { page, frame_id });
            }
          },
          PageEvent::FrameNavigated(info) => {
            if let Some(page) = upgrade() {
              emit(ContextEvent::FrameNavigated {
                page,
                frame_id: info.frame_id,
              });
            }
          },
          PageEvent::Close => {
            if let Some(page) = upgrade() {
              emit(ContextEvent::PageClose(page));
            }
            // The page is gone — exit instead of waiting on a channel
            // whose senders (backend listener tasks) may outlive it.
            break;
          },
          PageEvent::Load => {
            if let Some(page) = upgrade() {
              emit(ContextEvent::PageLoad(page));
            }
          },
          _ => {},
        }
      }
    });

    ctx.pages.push(page);
    ctx.active_page_idx = ctx.pages.len() - 1;
    Ok(())
  }

  /// Same as `open_page` but accepts a pre-parsed `SessionKey` (avoids re-parsing).
  ///
  /// # Errors
  ///
  /// Returns an error if the browser instance or page creation fails.
  pub async fn open_page_keyed(&mut self, key: &SessionKey, url: &str) -> Result<AnyPage> {
    if !self.instances.contains_key(&*key.instance) {
      Box::pin(self.ensure_instance(&key.instance)).await?;
    }

    let plan = self.page_open_plan(key)?;
    let (page, cdp_ctx_id) = if &*key.context == "default" {
      (
        Box::pin(
          plan
            .browser
            .new_page(url, plan.browser_context_id.as_deref(), plan.viewport.as_ref()),
        )
        .await?,
        None,
      )
    } else if let Some(existing_ctx_id) = plan.browser_context_id.clone() {
      (
        Box::pin(
          plan
            .browser
            .new_page(url, Some(&existing_ctx_id), plan.viewport.as_ref()),
        )
        .await?,
        Some(existing_ctx_id),
      )
    } else {
      // Legacy `open_page_keyed` path — no options bag flows through
      // here (used by older MCP call sites that don't go via
      // `ContextRef::new_page`). Proxy wiring happens on the
      // `ContextRef` path exclusively.
      let ctx_id = plan.browser.new_context(None).await?;
      let p = Box::pin(plan.browser.new_page(url, Some(&ctx_id), plan.viewport.as_ref())).await?;
      (p, Some(ctx_id))
    };

    self.register_opened_page(key, page.clone(), cdp_ctx_id)?;
    Ok(page)
  }

  /// # Errors
  ///
  /// Returns an error if the instance, context, or page does not exist.
  pub fn active_page(&self, context: &str) -> Result<&AnyPage> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    // A browser killed from outside leaves its pages in place, and
    // handing one back sends every later command down a dead transport
    // (one 30s timeout per call) instead of relaunching. The instance is
    // evicted and rebuilt by the caller's cold-start path.
    if !inst.browser.is_alive() {
      return Err(FerriError::target_closed(Some(format!(
        "browser for instance '{}' is gone",
        key.instance
      ))));
    }
    let ctx = inst.context(&key.context)?;
    if let Some(page) = ctx.active_page() {
      return Ok(page);
    }
    // Distinguish "never had a page" from "every page has been closed": the
    // latter is a live context whose tabs are gone, and reporting it as an
    // invalid argument sent callers looking for a bad context name.
    if !ctx.pages.is_empty() {
      return Err(FerriError::target_closed(Some(format!(
        "every page in context '{context}' has been closed"
      ))));
    }
    Err(FerriError::invalid_argument(
      "context",
      format!("no pages in context '{context}'"),
    ))
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub fn context(&self, context: &str) -> Result<&BrowserContext> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    inst.context(&key.context)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub fn context_mut_checked(&mut self, context: &str) -> Result<&mut BrowserContext> {
    let key = self.session_key(context);
    let inst = self.instance_mut(&key.instance)?;
    inst.context_mut_checked(&key.context)
  }

  /// Drop every per-context registry entry for `composite`.
  ///
  /// Twelve registries are keyed by composite session key, and context
  /// names come from a process-wide counter that never reuses a value, so
  /// without this each one grows by an entry for every context the process
  /// ever opens. The costs are not uniform: a `storageState` bag holds the
  /// session's cookies and localStorage (dead auth material kept in RAM),
  /// and a `routeFromHAR(update:true)` recorder holds the whole parsed HAR
  /// — megabytes per context.
  ///
  /// Callers must have already drained anything with an exit action:
  /// `ContextRef::close_impl` flushes HAR updates before it gets here.
  /// Live `ContextRef` clones are unaffected — they hold their own `Arc`
  /// clones of the closed-flag and emitter rather than reading the map.
  async fn purge_context_registries(&self, composite: &str) {
    fn drop_key<T>(map: &Arc<std::sync::Mutex<HashMap<String, T>>>, key: &str) {
      map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(key);
    }
    drop_key(&self.context_events, composite);
    drop_key(&self.context_closed, composite);
    drop_key(&self.record_video, composite);
    drop_key(&self.context_options, composite);
    drop_key(&self.har_recorders, composite);
    drop_key(&self.context_har_updates, composite);
    self
      .clock_installed
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self
      .storage_state_hydrated
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self
      .record_har_installed
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(composite);
    self.context_bindings.write().await.remove(composite);
    self.context_ws_routes.write().await.remove(composite);
    self.context_routes.write().await.remove(composite);
    self.context_init_scripts.write().await.remove(composite);
  }

  /// Remove a context. If it has a CDP browser context ID, dispose it
  /// (one CDP call kills the context + all pages, matching Playwright's doClose).
  pub async fn remove_context(&mut self, context: &str) {
    let key = self.session_key(context);
    if let Some(inst) = self.instances.get_mut(&*key.instance) {
      let ctx_id = inst.context(&key.context).ok().and_then(|c| c.cdp_context_id.clone());
      // One call kills a real browser context and all its pages
      // (Playwright's doClose). The default context has no backend
      // context of its own, so its tabs have to be closed one by one —
      // dropping the handles alone left them open in the browser.
      if let Some(id) = ctx_id {
        let _ = inst.browser.dispose_context(&id).await;
      } else {
        let pages: Vec<AnyPage> = inst.context(&key.context).map(|c| c.pages.clone()).unwrap_or_default();
        for page in pages {
          let _ = page.close_page(crate::options::PageCloseOptions::default()).await;
        }
      }
      inst.remove_context(&key.context);
    }
    // Registries are keyed by composite, and callers pass a bare context
    // name — purging with the raw argument would silently match nothing.
    self.purge_context_registries(&key.to_composite()).await;
  }

  /// # Errors
  ///
  /// Returns an error if the context does not exist or the page index is out of range.
  pub fn select_page(&mut self, context: &str, page_idx: usize) -> Result<()> {
    let key = self.session_key(context);
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut_checked(&key.context)?;
    if page_idx >= ctx.pages.len() {
      return Err(FerriError::Backend(format!(
        "Page index {page_idx} out of range (context '{context}' has {} pages)",
        ctx.pages.len()
      )));
    }
    ctx.active_page_idx = page_idx;
    Ok(())
  }

  /// Close one page of a context: ask the browser to close the target,
  /// release the page's local resources, and drop it from the context.
  ///
  /// Dropping the handle alone is not enough — the tab stays open in
  /// the browser and its listener tasks stay parked on the transport,
  /// so a session that opened and "closed" pages accumulated both.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or the index is out of range.
  pub async fn close_page(&mut self, context: &str, page_idx: usize) -> Result<()> {
    let key = self.session_key(context);
    let inst = self.instance_mut(&key.instance)?;
    let ctx = inst.context_mut_checked(&key.context)?;
    if page_idx >= ctx.pages.len() {
      return Err(FerriError::Backend(format!("Page index {page_idx} out of range")));
    }
    let page = ctx.pages.remove(page_idx);
    if ctx.active_page_idx >= ctx.pages.len() {
      ctx.active_page_idx = ctx.pages.len().saturating_sub(1);
    }
    // A page whose target is already gone (crashed, closed from the
    // page itself) still needs its local teardown, so the close failure
    // is logged rather than propagated.
    if let Err(e) = page.close_page(crate::options::PageCloseOptions::default()).await {
      tracing::debug!(target: "ferridriver::state", error = %e, "close_page: backend close failed");
    }
    page.dispose_local();
    Ok(())
  }

  pub async fn list_contexts(&self) -> Vec<ContextInfo> {
    let mut result = Vec::new();
    for (inst_name, inst) in &self.instances {
      for (ctx_name, ctx) in &inst.contexts {
        let mut pages = Vec::new();
        for (i, page) in ctx.pages.iter().enumerate() {
          let url = page.url().await.ok().flatten().unwrap_or_default();
          let title = page.title().await.ok().flatten().unwrap_or_default();
          pages.push(PageInfo {
            index: i,
            url,
            title,
            active: i == ctx.active_page_idx,
          });
        }
        // Use composite name for non-default instances, bare name for default
        let name = if inst_name == "default" {
          ctx_name.clone()
        } else {
          format!("{inst_name}:{ctx_name}")
        };
        result.push(ContextInfo {
          name,
          instance: inst_name.clone(),
          context: ctx_name.clone(),
          pages,
        });
      }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
  }

  /// Store a new ref map for the given context (atomic, no `&mut self` needed).
  pub fn set_ref_map(&self, context: &str, ref_map: HashMap<String, i64>) {
    let key = self.session_key(context);
    if let Some(inst) = self.instances.get(&*key.instance)
      && let Some(ctx) = inst.contexts.get(&*key.context)
    {
      ctx.ref_map.store(std::sync::Arc::new(ref_map));
    }
  }

  #[must_use]
  pub fn ref_map(&self, context: &str) -> HashMap<String, i64> {
    let key = self.session_key(context);
    self
      .instances
      .get(&*key.instance)
      .and_then(|inst| inst.contexts.get(&*key.context))
      .map(|c| (**c.ref_map.load()).clone())
      .unwrap_or_default()
  }

  /// Get an `Arc` handle to a context's ref map `ArcSwap` for lock-free access.
  #[must_use]
  pub fn ref_map_handle(&self, context: &str) -> Option<std::sync::Arc<arc_swap::ArcSwap<HashMap<String, i64>>>> {
    let key = self.session_key(context);
    self
      .instances
      .get(&*key.instance)
      .and_then(|inst| inst.contexts.get(&*key.context))
      .map(|c| std::sync::Arc::clone(&c.ref_map))
  }

  /// Get `Arc` handles to a context's log collections for lock-free access.
  #[must_use]
  pub fn log_handles(&self, context: &str) -> Option<ContextLogHandles> {
    let key = self.session_key(context);
    self
      .instances
      .get(&*key.instance)
      .and_then(|inst| inst.contexts.get(&*key.context))
      .map(|ctx| ContextLogHandles {
        console: std::sync::Arc::clone(&ctx.console_log),
        network: std::sync::Arc::clone(&ctx.network_log),
        dialog: std::sync::Arc::clone(&ctx.dialog_log),
      })
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub async fn console_messages(
    &self,
    context: &str,
    level: Option<&str>,
    limit: usize,
  ) -> Result<Vec<ConsoleMessage>> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    let ctx = inst.context(&key.context)?;
    Ok(ctx.console_messages(level, limit).await)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub async fn network_requests(&self, context: &str, limit: usize) -> Result<Vec<Request>> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    let ctx = inst.context(&key.context)?;
    Ok(ctx.network_requests(limit).await)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist, or page discovery fails.
  pub async fn refresh_pages(&mut self, context: &str) -> Result<usize> {
    let key = self.session_key(context);
    let viewport = self.default_viewport.clone();
    let inst = self.instance_mut(&key.instance)?;
    let current_pages = Box::pin(inst.browser.pages()).await?;
    let owned_pages: rustc_hash::FxHashSet<_> = inst
      .contexts
      .values()
      .flat_map(|context| {
        context
          .pages
          .iter()
          .filter(|page| !page.is_closed())
          .map(AnyPage::page_guid)
      })
      .collect();
    let ctx = inst.context_mut_checked(&key.context)?;
    ctx.prune_closed_pages();

    let mut adopted = Vec::new();
    for page in current_pages {
      if owned_pages.contains(&page.page_guid()) {
        continue;
      }
      page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
      adopted.push(page.clone());
      ctx.pages.push(page);
    }
    let total = ctx.pages.len();

    // A tab that appeared in the browser after the instance was adopted
    // is as new to this context as the ones adopted at connect time, and
    // needs the same emulation — otherwise the one tab a user opened by
    // hand is the one that reports the browser's own window size.
    if let Some(ref vp) = viewport.filter(|_| inst.browser.kind() != BackendKind::WebDriver) {
      for page in &adopted {
        if let Err(e) = page.emulate_viewport(vp).await {
          tracing::warn!(
            target: "ferridriver::state",
            error = %e,
            "could not emulate the configured viewport on a newly adopted page",
          );
        }
      }
    }
    Ok(total)
  }

  /// # Errors
  ///
  /// Returns an error if the instance or context does not exist.
  pub async fn dialog_messages(&self, context: &str, limit: usize) -> Result<Vec<DialogEvent>> {
    let key = self.session_key(context);
    let inst = self.instance(&key.instance)?;
    let ctx = inst.context(&key.context)?;
    Ok(ctx.dialog_messages(limit).await)
  }

  /// # Errors
  /// Retains incomplete instance cleanup and reports its failure for a later retry.
  pub async fn shutdown(&mut self) -> Result<()> {
    self.connected.store(false, std::sync::atomic::Ordering::Relaxed);
    self.shutdown_generation = self.shutdown_generation.wrapping_add(1);
    self
      .launch_permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    self
      .pending_cleanup
      .extend(self.instances.drain().map(|(name, instance)| (name, instance.into())));
    self.popup_pumps.clear();
    self.cleanup_instances(None).await
  }

  pub(crate) async fn dispose_result(&mut self) -> Result<()> {
    self.disposed = true;
    self.shutdown().await
  }

  /// Close one browser instance, leaving the others running. Returns
  /// `false` if no instance by that name is live.
  ///
  /// The next session routed to the name launches a fresh browser, so
  /// this is also how an operator picks up changed per-instance chrome
  /// args without taking down every other instance in the server.
  ///
  /// # Errors
  /// Returns a provider cleanup error while retaining the owner for another close attempt.
  pub async fn close_instance(&mut self, instance: &str) -> Result<bool> {
    self
      .launch_permits
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(instance);
    let exists = self.instances.contains_key(instance) || self.pending_cleanup.iter().any(|(name, _)| name == instance);
    self.popup_pumps.remove(instance);
    let result = self.evict_instance(instance).await;
    if self.instances.is_empty() {
      self.connected.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    result.map(|()| exists)
  }

  #[must_use]
  pub fn is_connected(&self) -> bool {
    !self.instances.is_empty()
  }

  fn next_instance_generation(&mut self) -> u64 {
    self.instance_generation_counter += 1;
    self.instance_generation_counter
  }

  /// Current generation of the named instance, or `None` if no instance
  /// by that name is live. A changed value (including `Some`→`None`→
  /// `Some`) means the browser session was swapped: any state a consumer
  /// cached against the old session is stale.
  #[must_use]
  pub fn instance_generation(&self, instance: &str) -> Option<u64> {
    self.instances.get(instance).map(|i| i.generation)
  }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextInfo {
  pub name: String,
  pub instance: String,
  pub context: String,
  pub pages: Vec<PageInfo>,
}

// Backward-compat alias for code that still references SessionInfo.
pub type SessionInfo = ContextInfo;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PageInfo {
  pub index: usize,
  pub url: String,
  pub title: String,
  pub active: bool,
}

async fn discover_ws_from_http(http_url: &str) -> Result<String> {
  discover_ws_with_headers(http_url, None).await
}

async fn discover_ws_with_headers(
  http_url: &str,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
) -> Result<String> {
  let budget = crate::operation_budget::OperationBudget::current(10_000)?;
  budget.wait(async {
    let mut url = reqwest::Url::parse(http_url).map_err(|error| FerriError::invalid_argument("url", error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
      return Err(FerriError::invalid_argument("url", "CDP discovery requires an HTTP or HTTPS endpoint"));
    }
    url.set_path(&format!("{}/json/version", url.path().trim_end_matches('/')));
    let response = crate::backend::webdriver::http_client(headers)?.get(url.clone()).send().await
      .map_err(|error| FerriError::backend(format!("CDP discovery request failed: {error}")))?;
    if !response.status().is_success() {
      return Err(FerriError::backend(format!(
        "CDP discovery returned HTTP {}. Pass the browser WebSocket URL directly or use the profile's DevToolsActivePort file.", response.status()
      )));
    }
    let peer = response.remote_addr();
    let body: serde_json::Value = response.json().await.map_err(|error| FerriError::backend(format!("Parse /json/version: {error}")))?;
    let endpoint = body["webSocketDebuggerUrl"].as_str().ok_or_else(|| FerriError::backend("No webSocketDebuggerUrl in /json/version"))?;
    let websocket = reqwest::Url::parse(endpoint).map_err(|error| FerriError::backend(format!("Invalid debugger endpoint: {error}")))?;
    // Local Chrome can advertise localhost despite listening on only one IP family.
    if websocket.scheme() == "ws" && websocket.host_str() == Some("localhost")
      && let Some(peer) = peer.filter(|address| address.ip().is_loopback())
      && (url.host_str() == Some("localhost") || url.host_str().is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))) {
      return Ok(pin_ws_authority(endpoint, peer));
    }
    Ok(endpoint.to_owned())
  }).await.map_err(|error| error.error("discovering CDP endpoint"))?
}

/// Replace the host:port authority of a `ws://`/`wss://` URL with `addr`,
/// preserving the scheme and path. Returns the URL unchanged if it does not
/// look like a ws URL with an authority.
fn pin_ws_authority(ws_url: &str, addr: std::net::SocketAddr) -> String {
  for scheme in ["ws://", "wss://"] {
    if let Some(rest) = ws_url.strip_prefix(scheme) {
      let path = rest.find('/').map_or("", |i| &rest[i..]);
      return format!("{scheme}{addr}{path}");
    }
  }
  ws_url.to_string()
}

/// Discover a running Chrome instance by reading its `DevToolsActivePort` file.
fn discover_chrome_ws(channel: &str, explicit_user_data_dir: Option<&str>) -> Result<String> {
  let candidates: Vec<std::path::PathBuf> = if let Some(dir) = explicit_user_data_dir {
    vec![std::path::PathBuf::from(dir)]
  } else {
    chrome_default_user_data_dirs(channel)?
  };

  // A profile directory existing proves only that the browser is installed. Only
  // DevToolsActivePort proves one is running with debugging on, so keep walking
  // the candidates until a readable port file turns up.
  let mut last_err = None;
  for user_data_dir in &candidates {
    let port_file = user_data_dir.join("DevToolsActivePort");
    match std::fs::read_to_string(&port_file) {
      Ok(content) => return parse_devtools_active_port(&content),
      Err(e) => last_err = Some(format!("{}: {e}", port_file.display())),
    }
  }

  let browser = if channel == "chromium" { "Chromium" } else { "Chrome" };
  Err(FerriError::Backend(format!(
    "Cannot read DevToolsActivePort ({}). Ensure {browser} ({channel}) is running with remote debugging \
     enabled at chrome://inspect/#remote-debugging, or pass user_data_dir explicitly.",
    last_err.unwrap_or_else(|| "no candidate profile directories".to_string())
  )))
}

fn parse_devtools_active_port(content: &str) -> Result<String> {
  let lines: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
  let (Some(port_line), Some(path)) = (lines.first(), lines.get(1)) else {
    return Err(FerriError::Backend(format!(
      "Invalid DevToolsActivePort content: {content:?}"
    )));
  };

  let port: u16 = port_line
    .parse()
    .map_err(|_| FerriError::Backend(format!("Invalid port '{port_line}' in DevToolsActivePort")))?;

  Ok(format!("ws://127.0.0.1:{port}{path}"))
}

/// Profile directories to search for a running browser, best match first.
///
/// Returns every directory that exists rather than the first, because an
/// installed-but-idle Chrome must not shadow a Chromium that is actually
/// running with debugging enabled.
fn chrome_default_user_data_dirs(channel: &str) -> Result<Vec<std::path::PathBuf>> {
  let home = std::env::var("HOME")
    .or_else(|_| std::env::var("USERPROFILE"))
    .map_err(|_| FerriError::backend("Cannot determine home directory"))?;

  let os = std::env::consts::OS;
  let suffix = match channel {
    "stable" | "chrome" => "",
    "beta" => " Beta",
    "dev" => " Dev",
    "canary" => " Canary",
    "chromium" => {
      let path = match os {
        "linux" => std::path::PathBuf::from(&home).join(".config/chromium"),
        "macos" => std::path::PathBuf::from(&home).join("Library/Application Support/Chromium"),
        "windows" => {
          let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
          std::path::PathBuf::from(local_app_data).join("Chromium/User Data")
        },
        _ => return Err(FerriError::unsupported(format!("OS: {os}"))),
      };
      return Ok(vec![path]);
    },
    other => {
      return Err(FerriError::invalid_argument(
        "channel",
        format!("unknown Chrome channel: {other}"),
      ));
    },
  };

  let chrome_path = match os {
    "linux" => {
      let dir_name = if suffix.is_empty() {
        "google-chrome".to_string()
      } else {
        format!("google-chrome{}", suffix.to_lowercase().replace(' ', "-"))
      };
      std::path::PathBuf::from(&home).join(".config").join(dir_name)
    },
    "macos" => std::path::PathBuf::from(&home)
      .join("Library/Application Support")
      .join(format!("Google/Chrome{suffix}")),
    "windows" => {
      let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
      std::path::PathBuf::from(local_app_data).join(format!("Google/Chrome{suffix}/User Data"))
    },
    _ => {
      return Err(FerriError::unsupported(format!("OS: {os}")));
    },
  };

  let chromium_path = match os {
    "linux" => Some(std::path::PathBuf::from(&home).join(".config/chromium")),
    "macos" => Some(std::path::PathBuf::from(&home).join("Library/Application Support/Chromium")),
    "windows" => {
      let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
      Some(std::path::PathBuf::from(local_app_data).join("Chromium/User Data"))
    },
    _ => None,
  };

  let found: Vec<_> = std::iter::once(chrome_path.clone())
    .chain(chromium_path.clone())
    .filter(|p| p.exists())
    .collect();

  if found.is_empty() {
    return Err(FerriError::Backend(match chromium_path {
      Some(c) => format!(
        "Chrome user data dir not found at {} or {}",
        chrome_path.display(),
        c.display()
      ),
      None => format!("Chrome user data dir not found: {}", chrome_path.display()),
    }));
  }

  Ok(found)
}

/// Common Chrome/Chromium launch flags used by cdp-pipe and cdp-raw backends.
#[must_use]
/// Build Chrome flags matching Playwright's launch sequence exactly.
/// Order: chromiumSwitches → headless flags → sandbox → user args.
pub fn chrome_flags(headless: bool, extra_args: &[String]) -> Vec<String> {
  chrome_flags_with(headless, extra_args, None)
}

/// [`chrome_flags`] with Playwright's `ignoreDefaultArgs`.
///
/// Every built-in switch is a policy decision that can conflict with a
/// caller's environment (a proxy that needs background networking, an
/// extension that needs the sandbox). Playwright has always let a
/// caller drop them; ferridriver carried the option on `LaunchPlan` and
/// then applied it nowhere, so the flags were unconditional.
#[must_use]
pub fn chrome_flags_with(
  headless: bool,
  extra_args: &[String],
  ignore: Option<&crate::options::IgnoreDefaultArgs>,
) -> Vec<String> {
  use crate::options::IgnoreDefaultArgs;

  let drop_all = matches!(ignore, Some(IgnoreDefaultArgs::All));
  let dropped: &[String] = match ignore {
    Some(IgnoreDefaultArgs::Some(list)) => list,
    _ => &[],
  };
  // Compare on the switch NAME so `--foo=bar` is dropped by `--foo`.
  let is_dropped = |flag: &str| {
    let name = flag.split('=').next().unwrap_or(flag);
    dropped.iter().any(|d| {
      let d_name = d.split('=').next().unwrap_or(d);
      d_name == name
    })
  };

  let mut flags: Vec<String> = Vec::with_capacity(40 + extra_args.len());
  let loads_extension = extra_args.iter().any(|arg| arg.starts_with("--load-extension="));

  // 1. Base chromiumSwitches (from Playwright's chromiumSwitches.ts)
  if !drop_all {
    for f in CHROMIUM_SWITCHES {
      if loads_extension && *f == "--disable-extensions" {
        continue;
      }
      if !is_dropped(f) {
        flags.push((*f).into());
      }
    }
  }

  // 2. Always added after base switches
  if !drop_all && !is_dropped("--enable-unsafe-swiftshader") {
    flags.push("--enable-unsafe-swiftshader".into());
  }

  // 3. Headless flags (Playwright adds these when headless=true).
  // Playwright passes bare `--headless` too — Chrome maps to
  // `--headless=old` on full chrome. The 2x perf gap on Regular
  // Chrome lives elsewhere, not in this flag (verified via
  // playwright-core/lib/server/chromium/chromium.js:288).
  //
  // Filtered by `ignoreDefaultArgs` like every other section: Playwright
  // builds the headless switches inside `defaultArgs()`, so
  // `ignoreDefaultArgs: true` drops them there too. Gating only sections
  // 1/2/4 left the caller with a browser that was still forced headless
  // (and still colour-scheme-pinned) after asking for no defaults at all.
  if headless && !drop_all {
    for f in ["--headless", "--hide-scrollbars", "--mute-audio"] {
      if !is_dropped(f) {
        flags.push(f.into());
      }
    }
    // `preferredColorScheme=1` pins Blink's "no override" baseline to
    // light. Without it, headless Chrome inherits the host's GTK / KDE
    // dark-mode setting, which causes `matchMedia('(prefers-color-scheme:
    // dark)').matches` to stay `true` even after
    // `page.emulateMedia({colorScheme: null})` clears the override —
    // the override is gone but the system fallback is still dark.
    // Tests that rely on "null reset returns to light" only pass on
    // light-mode hosts otherwise. Playwright's own chromiumSwitches
    // skip this because their CI runs on light-mode hosts; we cover
    // both.
    if !is_dropped("--blink-settings") {
      flags.push(
        "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4,preferredColorScheme=1".into(),
      );
    }
  }

  // 4. Sandbox control (Playwright disables by default unless chromiumSandbox=true)
  if !drop_all && !is_dropped("--no-sandbox") {
    flags.push("--no-sandbox".into());
  }

  // 5. User-provided args. Never filtered: `ignoreDefaultArgs` names
  // DEFAULTS to drop, so an explicitly-passed arg still wins.
  for arg in extra_args {
    flags.push(arg.clone());
  }

  merge_feature_switches(&mut flags);
  flags
}

/// Chromium reads only the last `--enable-features` and the last
/// `--disable-features`, so a caller's own switch would silently drop the
/// defaults (Playwright appends them the same way and loses them too). Each
/// kind becomes one switch where it first appeared, and a feature named
/// again later moves to that later list, so the caller wins feature by
/// feature over the defaults before it.
///
/// `WebMCP` also brings what an automation client needs from it on builds
/// that gate those separately: `WebMCPTesting` for `getTools()` and
/// `DevToolsWebMCPSupport` for the `DevTools` domain. Chrome 153 enables both
/// with `WebMCP` already; a caller who disables one keeps it disabled.
fn merge_feature_switches(flags: &mut Vec<String>) {
  const ENABLE: &str = "--enable-features=";
  const DISABLE: &str = "--disable-features=";
  // A feature may carry a field trial or parameters: `Name<Trial:key/value`.
  fn name(feature: &str) -> &str {
    let feature = feature.trim_start_matches('*');
    feature.split(['<', ':']).next().unwrap_or(feature)
  }
  let mut lists: [Vec<String>; 2] = [Vec::new(), Vec::new()];
  let mut slots: [Option<usize>; 2] = [None, None];
  let mut kept = Vec::with_capacity(flags.len());
  for flag in flags.drain(..) {
    let (kind, value) = if let Some(value) = flag.strip_prefix(ENABLE) {
      (0, value)
    } else if let Some(value) = flag.strip_prefix(DISABLE) {
      (1, value)
    } else {
      kept.push(flag);
      continue;
    };
    slots[kind].get_or_insert(kept.len());
    for feature in value.split(',').filter(|feature| !feature.is_empty()) {
      for list in &mut lists {
        list.retain(|named| name(named) != name(feature));
      }
      lists[kind].push(feature.to_owned());
    }
  }
  let [enabled, disabled] = &mut lists;
  if enabled.iter().any(|feature| name(feature) == "WebMCP") {
    for companion in ["WebMCPTesting", "DevToolsWebMCPSupport"] {
      if !enabled
        .iter()
        .chain(disabled.iter())
        .any(|feature| name(feature) == companion)
      {
        enabled.push(companion.to_owned());
      }
    }
  }
  let mut switches = Vec::new();
  for (slot, switch, list) in [(slots[0], ENABLE, enabled), (slots[1], DISABLE, disabled)] {
    if let Some(slot) = slot
      && !list.is_empty()
    {
      switches.push((slot, format!("{switch}{}", list.join(","))));
    }
  }
  // Later slots first, so inserting one does not shift the other.
  switches.sort_by_key(|(slot, _)| std::cmp::Reverse(*slot));
  for (slot, switch) in switches {
    kept.insert(slot, switch);
  }
  *flags = kept;
}

/// Chrome switches matching Playwright's `chromiumSwitches()` exactly.
/// See: playwright/packages/playwright-core/src/server/chromium/chromiumSwitches.ts
const CHROMIUM_SWITCHES: &[&str] = &[
  "--disable-field-trial-config",
  "--disable-background-networking",
  "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows",
  "--disable-back-forward-cache",
  "--disable-breakpad",
  "--disable-client-side-phishing-detection",
  "--disable-component-extensions-with-background-pages",
  "--disable-component-update",
  "--no-default-browser-check",
  "--disable-default-apps",
  "--disable-dev-shm-usage",
  "--disable-edgeupdater",
  "--disable-extensions",
  "--disable-features=AvoidUnnecessaryBeforeUnloadCheckSync,BlockInsecurePrivateNetworkRequests,BoundaryEventDispatchTracksNodeRemoval,DestroyProfileOnBrowserClose,DialMediaRouteProvider,GlobalMediaControls,HttpsUpgrades,LensOverlay,MediaRouter,PaintHolding,PrivateNetworkAccessSendPreflights,ThirdPartyStoragePartitioning,Translate,AutoDeElevate,RenderDocument,OptimizationHints,msForceBrowserSignIn,msEdgeUpdateLaunchServicesPreferredVersion",
  "--enable-features=CDPScreenshotNewSurface",
  "--allow-pre-commit-input",
  "--disable-hang-monitor",
  "--disable-ipc-flooding-protection",
  "--disable-popup-blocking",
  "--disable-prompt-on-repost",
  "--disable-renderer-backgrounding",
  "--force-color-profile=srgb",
  "--metrics-recording-only",
  "--no-first-run",
  "--password-store=basic",
  "--use-mock-keychain",
  "--no-service-autorun",
  "--export-tagged-pdf",
  "--disable-search-engine-choice-screen",
  "--unsafely-disable-devtools-self-xss-warnings",
  "--edge-skip-compat-layer-relaunch",
  "--enable-automation",
  "--disable-infobars",
  "--disable-sync",
];

/// Resolve the Chrome binary to use, respecting env vars and headless mode.
///
/// Follows Playwright's resolution strategy:
/// - `executablePath` (handled by caller before this function) always wins.
/// - Explicit env vars (`CHROMIUM_HEADLESS_SHELL_PATH`, `CHROMIUM_PATH`) override auto-detection.
/// - When headless and no explicit path is set, prefer Chrome Headless Shell (lighter, faster).
/// - Fall back to full Chrome/Chromium otherwise.
///
/// Precedence (headless=true):
/// 1. `CHROMIUM_HEADLESS_SHELL_PATH` env var
/// 2. `CHROMIUM_PATH` env var (explicit user override always wins over auto-detection)
/// 3. Auto-detect headless shell (Playwright cache, ferridriver cache)
/// 4. Auto-detect regular Chrome (`detect_chromium()`)
///
/// Precedence (headless=false):
/// 1. `CHROMIUM_PATH` env var
/// 2. Auto-detect regular Chrome (`detect_chromium()`)
#[must_use]
pub fn resolve_chromium(headless: bool) -> String {
  if headless {
    // Explicit headless shell path
    if let Ok(p) = std::env::var("CHROMIUM_HEADLESS_SHELL_PATH")
      && std::path::Path::new(&p).exists()
    {
      return p;
    }

    // Explicit chrome path -- user chose a specific binary, respect it
    if let Ok(p) = std::env::var("CHROMIUM_PATH")
      && std::path::Path::new(&p).exists()
    {
      return p;
    }

    // Auto-detect headless shell (Playwright cache, ferridriver cache)
    if let Some(p) = detect_chromium_headless_shell() {
      return p;
    }
  }

  // Headed mode, or no headless shell found: use regular Chrome
  detect_chromium()
}

/// Auto-detect Chrome Headless Shell binary on the system.
///
/// Searches Playwright's cache and ferridriver's own cache for installed
/// headless shell binaries. Does NOT check env vars (that's `resolve_chromium()`'s job).
#[must_use]
pub fn detect_chromium_headless_shell() -> Option<String> {
  // Check Playwright's cache for headless shell
  if let Some(p) = find_playwright_headless_shell() {
    return Some(p);
  }

  // Check ferridriver's own cache
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_headless_shell() {
    return Some(p);
  }

  None
}

/// Chrome for Testing / Chromium builds under a browser cache directory,
/// newest first. Covers every layout Chrome for Testing has shipped: the
/// per-arch `chrome-mac-arm64` / `chrome-mac-x64` bundles it uses now, and
/// the older `chrome-mac/Chromium.app` that Playwright caches still carry.
fn cached_chromium_in(cache: &std::path::Path) -> Option<String> {
  const LAYOUTS: [&str; 6] = [
    "chrome-linux64/chrome",
    "chrome-linux/chrome",
    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
    "chrome-win64/chrome.exe",
  ];

  let entries = std::fs::read_dir(cache).ok()?;
  let mut candidates: Vec<_> = entries
    .filter_map(std::result::Result::ok)
    .filter(|e| {
      let name = e.file_name().to_string_lossy().into_owned();
      name.starts_with("chromium-") && !name.starts_with("chromium-headless-shell-")
    })
    .collect();
  candidates.sort_by_key(|b| std::cmp::Reverse(b.file_name())); // newest first

  for entry in candidates {
    for layout in LAYOUTS {
      let exe = entry.path().join(layout);
      if exe.exists() {
        return Some(exe.to_string_lossy().to_string());
      }
    }
  }
  None
}

/// Playwright's browser cache, following Playwright's own registry logic:
/// `PLAYWRIGHT_BROWSERS_PATH`, then `XDG_CACHE_HOME`, then `~/.cache`.
fn playwright_cache_dir() -> Option<std::path::PathBuf> {
  if let Ok(p) = std::env::var("PLAYWRIGHT_BROWSERS_PATH") {
    return Some(std::path::PathBuf::from(p));
  }
  std::env::var("XDG_CACHE_HOME")
    .ok()
    .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.cache")))
    .map(|c| std::path::PathBuf::from(c).join("ms-playwright"))
}

/// Detect Chrome/Chromium binary on the system.
///
/// Automation-capable builds win over the platform's installed browser, and
/// not merely because they are better tested: a Chrome enrolled in cloud
/// management can carry a `RemoteDebuggingAllowed=false` policy, under which
/// it starts normally, refuses to open the debugging port, and never writes
/// `DevToolsActivePort` — indistinguishable from a hang. Chrome for Testing
/// and Chromium builds are never enrolled. Both launchers here always pass
/// their own `--user-data-dir`, so preferring one costs no profile.
#[must_use]
pub fn detect_chromium() -> String {
  if let Ok(p) = std::env::var("CHROMIUM_PATH")
    && std::path::Path::new(&p).exists()
  {
    return p;
  }

  // ferridriver's own `install chromium` download.
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_chromium() {
    return p;
  }

  if let Some(pw_cache) = playwright_cache_dir()
    && pw_cache.is_dir()
    && let Some(p) = cached_chromium_in(&pw_cache)
  {
    return p;
  }

  // Chrome for Testing installed as a normal app, and plain Chromium — both
  // unenrolled, so they outrank the managed bundles further down.
  #[cfg(target_os = "macos")]
  {
    let unmanaged = [
      "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
      "Chromium.app/Contents/MacOS/Chromium",
    ];
    for bundle in &unmanaged {
      let sys = std::path::PathBuf::from("/Applications").join(bundle);
      if sys.exists() {
        return sys.to_string_lossy().to_string();
      }
      if let Ok(home) = std::env::var("HOME") {
        let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
        if user.exists() {
          return user.to_string_lossy().to_string();
        }
      }
    }
  }

  if let Ok(path_var) = std::env::var("PATH") {
    let names = [
      "chromium-browser",
      "chromium",
      "google-chrome-stable",
      "google-chrome",
      "microsoft-edge",
      "chrome",
    ];
    for name in &names {
      for dir in path_var.split(':') {
        let candidate = std::path::PathBuf::from(dir).join(name);
        if candidate.exists() {
          return candidate.to_string_lossy().to_string();
        }
      }
    }
  }

  #[cfg(target_os = "macos")]
  {
    // Chromium and Chrome for Testing were already tried above; what is left
    // is the platform's own browser, which may be enrolled.
    let bundles = [
      "Google Chrome.app/Contents/MacOS/Google Chrome",
      "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
      "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    for bundle in &bundles {
      let sys = std::path::PathBuf::from("/Applications").join(bundle);
      if sys.exists() {
        return sys.to_string_lossy().to_string();
      }
      if let Ok(home) = std::env::var("HOME") {
        let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
        if user.exists() {
          return user.to_string_lossy().to_string();
        }
      }
    }
  }

  #[cfg(target_os = "linux")]
  {
    let paths = [
      "/usr/bin/chromium-browser",
      "/usr/bin/chromium",
      "/snap/bin/chromium",
      "/usr/bin/google-chrome-stable",
      "/usr/bin/google-chrome",
      "/usr/bin/microsoft-edge",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return path.to_string();
      }
    }
  }

  // Check ferridriver's own browser cache
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_chromium() {
    return p;
  }

  if let Some(p) = find_playwright_chrome() {
    return p;
  }

  "chromium".to_string()
}

/// Detect Firefox binary on the system.
///
/// Search order (matches Chrome detection pattern):
/// 1. `FIREFOX_PATH` environment variable
/// 2. ferridriver's own browser cache (installed via `install_firefox()`)
/// 3. Playwright's browser cache
/// 4. System-installed Firefox (platform-specific paths)
/// 5. `which firefox` fallback
///
/// # Errors
///
/// Returns an error if no Firefox binary can be found.
pub fn detect_firefox() -> Result<String> {
  // 1. Env var (highest priority)
  if let Ok(p) = std::env::var("FIREFOX_PATH")
    && std::path::Path::new(&p).exists()
  {
    return Ok(p);
  }

  // 2. ferridriver's own cache
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_firefox() {
    return Ok(p);
  }

  // 3. Playwright's Firefox cache
  if let Some(p) = find_playwright_firefox() {
    return Ok(p);
  }

  // 4. System-installed Firefox
  #[cfg(target_os = "macos")]
  {
    let paths = [
      "/Applications/Firefox.app/Contents/MacOS/firefox",
      "/Applications/Firefox Nightly.app/Contents/MacOS/firefox",
      "/Applications/Firefox Developer Edition.app/Contents/MacOS/firefox",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return Ok(path.to_string());
      }
    }
  }

  #[cfg(target_os = "linux")]
  {
    // Skip snap-wrapped Firefox builds: Ubuntu 24.04+ ships a snap as
    // /usr/bin/firefox (and the explicit /snap/bin/firefox). Snap's
    // confinement blocks the WebDriver BiDi remote-debugging port and the
    // shim never prints the WebSocket URL on stderr, so detection hangs
    // until the 15s discovery timeout. Treat snap wrappers as
    // "not installed" so callers fall back to ferridriver's own download.
    let paths = [
      "/usr/bin/firefox",
      "/usr/bin/firefox-esr",
      "/snap/bin/firefox",
      "/usr/lib/firefox/firefox",
      "/usr/lib64/firefox/firefox",
    ];
    for path in &paths {
      let p = std::path::Path::new(path);
      if !p.exists() {
        continue;
      }
      let resolved = std::fs::canonicalize(p).map_or_else(|_| path.to_string(), |c| c.to_string_lossy().to_string());
      if resolved.contains("/snap/") {
        continue;
      }
      return Ok(path.to_string());
    }
  }

  #[cfg(target_os = "windows")]
  {
    let paths = [
      r"C:\Program Files\Mozilla Firefox\firefox.exe",
      r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return Ok(path.to_string());
      }
    }
  }

  // 5. which/where fallback
  let cmd = if cfg!(windows) { "where" } else { "which" };
  if let Ok(output) = std::process::Command::new(cmd).arg("firefox").output()
    && output.status.success()
  {
    let p = String::from_utf8_lossy(&output.stdout)
      .lines()
      .next()
      .unwrap_or("")
      .trim()
      .to_string();
    if !p.is_empty() && std::path::Path::new(&p).exists() {
      return Ok(p);
    }
  }

  Err(FerriError::backend(
    "Firefox not found. Install with `ferridriver install firefox` or set FIREFOX_PATH.",
  ))
}

/// Search Playwright's cache for an installed Firefox binary.
fn find_playwright_firefox() -> Option<String> {
  let home = std::env::var("HOME").ok()?;

  #[cfg(target_os = "macos")]
  let cache_base = std::path::PathBuf::from(&home).join("Library/Caches/ms-playwright");
  #[cfg(target_os = "linux")]
  let cache_base = std::env::var("XDG_CACHE_HOME")
    .map_or_else(
      |_| std::path::PathBuf::from(&home).join(".cache"),
      std::path::PathBuf::from,
    )
    .join("ms-playwright");
  #[cfg(target_os = "windows")]
  let cache_base = std::env::var("LOCALAPPDATA")
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|_| std::path::PathBuf::from(&home))
    .join("ms-playwright");

  let entries = std::fs::read_dir(&cache_base).ok()?;
  let mut firefox_dirs: Vec<_> = entries
    .filter_map(std::result::Result::ok)
    .filter(|e| {
      let name = e.file_name().to_string_lossy().to_string();
      name.starts_with("firefox-")
    })
    .collect();
  firefox_dirs.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

  for dir in firefox_dirs {
    let path = dir.path();
    #[cfg(target_os = "macos")]
    let exe = path.join("Firefox.app/Contents/MacOS/firefox");
    #[cfg(target_os = "linux")]
    let exe = path.join("firefox/firefox");
    #[cfg(target_os = "windows")]
    let exe = path.join("firefox/firefox.exe");

    if exe.exists() {
      return Some(exe.to_string_lossy().to_string());
    }
  }
  None
}

/// Search Playwright's cache dir for a Chrome Headless Shell binary.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn find_playwright_headless_shell() -> Option<String> {
  let home = std::env::var("HOME").ok()?;

  #[cfg(target_os = "macos")]
  let cache_dir = std::path::PathBuf::from(&home).join("Library/Caches/ms-playwright");
  #[cfg(target_os = "linux")]
  let cache_dir = std::path::PathBuf::from(&home).join(".cache/ms-playwright");

  if !cache_dir.exists() {
    return None;
  }

  let mut best_rev: u32 = 0;
  let mut best_name = String::new();
  let prefix = "chromium_headless_shell-";

  if let Ok(entries) = std::fs::read_dir(&cache_dir) {
    for entry in entries.flatten() {
      let name = entry.file_name().to_string_lossy().to_string();
      if let Some(rev_str) = name.strip_prefix(prefix)
        && let Ok(rev) = rev_str.parse::<u32>()
        && rev > best_rev
      {
        best_rev = rev;
        best_name = name;
      }
    }
  }

  if best_rev == 0 {
    return None;
  }

  #[cfg(target_os = "macos")]
  let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
  #[cfg(target_os = "linux")]
  let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

  #[cfg(target_os = "macos")]
  let plat = "mac";
  #[cfg(target_os = "linux")]
  let plat = "linux";

  let cft_binary = cache_dir
    .join(&best_name)
    .join(format!("chrome-headless-shell-{plat}-{arch}"))
    .join("chrome-headless-shell");

  if cft_binary.exists() {
    return Some(cft_binary.to_string_lossy().to_string());
  }

  None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn find_playwright_headless_shell() -> Option<String> {
  None
}

/// Search Playwright's cache dir for a chromium headless shell binary.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn find_playwright_chrome() -> Option<String> {
  let home = std::env::var("HOME").ok()?;

  #[cfg(target_os = "macos")]
  let cache_dir = std::path::PathBuf::from(&home).join("Library/Caches/ms-playwright");
  #[cfg(target_os = "linux")]
  let cache_dir = std::path::PathBuf::from(&home).join(".cache/ms-playwright");

  if !cache_dir.exists() {
    return None;
  }

  let mut best_rev: u32 = 0;
  let mut best_name = String::new();
  let prefix = "chromium_headless_shell-";

  if let Ok(entries) = std::fs::read_dir(&cache_dir) {
    for entry in entries.flatten() {
      let name = entry.file_name().to_string_lossy().to_string();
      if let Some(rev_str) = name.strip_prefix(prefix)
        && let Ok(rev) = rev_str.parse::<u32>()
        && rev > best_rev
      {
        best_rev = rev;
        best_name = name;
      }
    }
  }

  if best_rev == 0 {
    return None;
  }

  #[cfg(target_os = "macos")]
  let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
  #[cfg(target_os = "linux")]
  let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

  #[cfg(target_os = "macos")]
  let plat = "mac";
  #[cfg(target_os = "linux")]
  let plat = "linux";

  let cft_binary = cache_dir
    .join(&best_name)
    .join(format!("chrome-headless-shell-{plat}-{arch}"))
    .join("chrome-headless-shell");

  if cft_binary.exists() {
    return Some(cft_binary.to_string_lossy().to_string());
  }

  #[cfg(target_os = "linux")]
  {
    let alt_binary = cache_dir.join(&best_name).join("chrome-linux").join("headless_shell");
    if alt_binary.exists() {
      return Some(alt_binary.to_string_lossy().to_string());
    }
  }

  None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn find_playwright_chrome() -> Option<String> {
  None
}

#[cfg(test)]
mod discovery_tests {
  use super::{discover_ws_from_http, parse_devtools_active_port};
  use tokio::io::AsyncWriteExt;

  /// Serve one hand-rolled HTTP response, then hold the socket open.
  ///
  /// Keeping the connection alive is the whole point: Chrome 136+ ignores our
  /// `Connection: close`, so a reader that waits for EOF waits forever.
  async fn serve_once(response: &'static str) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
      let Ok((mut sock, _)) = listener.accept().await else {
        return;
      };
      let _ = sock.write_all(response.as_bytes()).await;
      let _ = sock.flush().await;
      // Never shut down the write half, and never drop `sock`.
      std::future::pending::<()>().await;
    });
    addr
  }

  /// Chrome 136+ with "Allow remote debugging for this browser instance" binds
  /// the port but serves no HTTP JSON endpoints. It answers a bodyless 404 and
  /// leaves the connection intact, which used to park the read forever and
  /// wedge the global state lock behind it.
  #[tokio::test]
  async fn bodyless_keepalive_404_errors_instead_of_hanging() {
    let addr = serve_once("HTTP/1.1 404 Not Found\r\nContent-Length:0\r\nContent-Type:text/html\r\n\r\n").await;

    let result = tokio::time::timeout(
      std::time::Duration::from_secs(5),
      discover_ws_from_http(&format!("http://{addr}")),
    )
    .await
    .expect("discovery must not hang on a keep-alive 404");

    let err = result.expect_err("404 is not a successful discovery").to_string();
    assert!(err.contains("404"), "error should name the status: {err}");
    assert!(
      err.contains("DevToolsActivePort"),
      "error should point at the workaround: {err}"
    );
  }

  /// A 200 whose `Content-Length` is honoured exactly, on a connection that
  /// never closes. Reading to EOF here would hang just as badly as the 404.
  #[tokio::test]
  async fn keepalive_200_is_read_by_content_length() {
    let body = r#"{"webSocketDebuggerUrl":"ws://localhost:9222/devtools/browser/abc"}"#;
    let response: &'static str =
      Box::leak(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_boxed_str());
    let addr = serve_once(response).await;

    let ws = tokio::time::timeout(
      std::time::Duration::from_secs(5),
      discover_ws_from_http(&format!("http://{addr}")),
    )
    .await
    .expect("discovery must not hang")
    .expect("valid /json/version");

    // The advertised `localhost` authority is replaced by the address actually reached.
    assert_eq!(ws, format!("ws://{addr}/devtools/browser/abc"));
  }

  #[test]
  fn devtools_active_port_needs_both_lines() {
    assert_eq!(
      parse_devtools_active_port("9222\n/devtools/browser/abc\n").expect("two lines"),
      "ws://127.0.0.1:9222/devtools/browser/abc"
    );
    assert!(parse_devtools_active_port("9222\n").is_err());
    assert!(parse_devtools_active_port("").is_err());
    assert!(parse_devtools_active_port("notaport\n/devtools/browser/abc").is_err());
  }
}

#[cfg(test)]
#[path = "state/adoption_tests.rs"]
mod adoption_tests;

#[cfg(test)]
mod tests {
  use super::cached_chromium_in;

  /// Chrome for Testing has shipped several directory layouts and a cache
  /// commonly holds more than one build; picking the wrong entry is how a
  /// machine silently falls through to its enrolled system Chrome.
  #[test]
  fn cached_chromium_prefers_the_newest_build_and_knows_every_layout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    let old = root
      .join("chromium-100")
      .join("chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS");
    std::fs::create_dir_all(&old).expect("mkdir");
    std::fs::write(old.join("Google Chrome for Testing"), b"x").expect("write");

    let new = root
      .join("chromium-150")
      .join("chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS");
    std::fs::create_dir_all(&new).expect("mkdir");
    std::fs::write(new.join("Google Chrome for Testing"), b"x").expect("write");

    let found = cached_chromium_in(root).expect("a build");
    assert!(found.contains("chromium-150"), "newest build must win, got {found}");
  }

  /// `chromium-headless-shell-*` also starts with `chromium-` but is a
  /// different binary; treating it as a headed build yields a browser that
  /// can never open a window.
  #[test]
  fn cached_chromium_ignores_the_headless_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Sorts AFTER "chromium-150" descending, so a naive filter picks it first.
    let shell = root
      .join("chromium-headless-shell-999")
      .join("chrome-headless-shell-mac-arm64");
    std::fs::create_dir_all(&shell).expect("mkdir");
    std::fs::write(shell.join("chrome-headless-shell"), b"x").expect("write");

    assert!(
      cached_chromium_in(root).is_none(),
      "headless shell is not a headed build"
    );

    let headed = root.join("chromium-150").join("chrome-linux64");
    std::fs::create_dir_all(&headed).expect("mkdir");
    std::fs::write(headed.join("chrome"), b"x").expect("write");

    let found = cached_chromium_in(root).expect("a build");
    assert!(
      found.contains("chromium-150"),
      "must skip the shell and take the headed build, got {found}"
    );
  }

  use std::sync::Arc;

  use super::*;

  #[test]
  fn pin_ws_authority_rewrites_advertised_localhost_host() {
    let addr = "127.0.0.1:9222".parse().unwrap();
    // Chrome advertises localhost; the actual listener is 127.0.0.1.
    assert_eq!(
      pin_ws_authority("ws://localhost:9222/devtools/browser/abc-123", addr),
      "ws://127.0.0.1:9222/devtools/browser/abc-123"
    );
    // Path-less and wss variants are handled; non-ws input is untouched.
    assert_eq!(pin_ws_authority("ws://localhost:9222", addr), "ws://127.0.0.1:9222");
    assert_eq!(
      pin_ws_authority("wss://localhost:9222/x", addr),
      "wss://127.0.0.1:9222/x"
    );
    assert_eq!(
      pin_ws_authority("http://localhost:9222/x", addr),
      "http://localhost:9222/x"
    );
  }

  #[test]
  fn pin_ws_authority_preserves_ipv6_literal() {
    let addr: std::net::SocketAddr = "[::1]:9222".parse().unwrap();
    assert_eq!(
      pin_ws_authority("ws://localhost:9222/devtools/browser/x", addr),
      "ws://[::1]:9222/devtools/browser/x"
    );
  }
  use crate::backend::BackendKind;

  /// Test helper: build a `BrowserState` with the minimum `LaunchPlan`
  /// needed to exercise the resolver/args plumbing. Using
  /// `LaunchPlan::default()` keeps these tests in lock-step with the
  /// single production construction path ([`BrowserState::with_plan`]).
  pub(super) fn test_state(backend: BackendKind) -> BrowserState {
    let selection = crate::options::BrowserSelection::resolve(None, Some(backend)).expect("backend selection");
    BrowserState::with_plan(
      ConnectMode::Launch,
      crate::options::LaunchPlan {
        backend,
        kind: selection.browser,
        headless: false,
        ..Default::default()
      },
    )
  }

  #[tokio::test]
  async fn configured_safari_connection_uses_its_backend_in_both_state_paths() {
    use tokio::io::AsyncWriteExt;

    for shared in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("http://{}", listener.local_addr().unwrap());
      let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for value in [
          serde_json::json!({"sessionId":"configured-safari","capabilities":{"browserName":"Safari","browserVersion":"26.2"}}),
          serde_json::json!(["device"]),
          serde_json::Value::Null,
        ] {
          let (mut socket, _) = listener.accept().await.unwrap();
          requests.push(crate::backend::webdriver::session::tests::request(&mut socket).await);
          let body = serde_json::json!({"value":value}).to_string();
          socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
      });
      let mut state = test_state(BackendKind::CdpPipe);
      state.set_instance_overrides_fn(Arc::new(|_| {
        Ok(crate::options::InstanceOverrides {
          kind: Some(crate::options::BrowserKind::Safari),
          ..Default::default()
        })
      }));
      state.set_instance_resolver_fn(Arc::new(move |_| {
        Some(ConnectMode::WebDriver {
          endpoint: endpoint.clone(),
          browser_name: "safari".into(),
          protocol: crate::backend::webdriver::WebDriverProtocol::Auto,
          capabilities: None,
          headers: None,
          timeout: Some(1000),
        })
      }));
      let state = Arc::new(tokio::sync::RwLock::new(state));
      if shared {
        BrowserState::ensure_instance_shared(&state, "safari").await.unwrap();
      } else {
        state.write().await.ensure_instance("safari").await.unwrap();
      }
      assert!(matches!(
        state.read().await.instances["safari"].browser,
        AnyBrowser::WebDriver(_)
      ));
      state.write().await.shutdown().await.unwrap();
      let requests = server.await.unwrap();
      assert!(
        requests[0].1["capabilities"]["alwaysMatch"]
          .get("webSocketUrl")
          .is_none()
      );
      assert_eq!(requests[1].0, "GET /session/configured-safari/window/handles HTTP/1.1");
      assert_eq!(requests[2].0, "DELETE /session/configured-safari HTTP/1.1");
    }
  }

  #[tokio::test]
  async fn failed_or_cancelled_shutdown_retains_the_provider_for_retry() {
    use tokio::io::AsyncWriteExt as _;

    for cancelled in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("http://{}", listener.local_addr().unwrap());
      let (received, receipt) = tokio::sync::oneshot::channel();
      let server = tokio::spawn(async move {
        let mut received = Some(received);
        let mut held = Vec::new();
        let mut requests = Vec::new();
        for (index, value) in [
          serde_json::json!({"sessionId":"cleanup","capabilities":{"browserName":"Safari","browserVersion":"26.2"}}),
          serde_json::json!(["page"]),
          serde_json::json!({"error":"unknown error","message":"provider cleanup unavailable"}),
          serde_json::Value::Null,
        ]
        .into_iter()
        .enumerate()
        {
          let (mut socket, _) = listener.accept().await.unwrap();
          requests.push(crate::backend::webdriver::session::tests::request(&mut socket).await);
          if index == 2 && cancelled {
            held.push(socket);
            received.take().unwrap().send(()).unwrap();
            continue;
          }
          let status = if index == 2 {
            "500 Internal Server Error"
          } else {
            "200 OK"
          };
          let body = serde_json::json!({"value":value}).to_string();
          socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
      });
      let mut state = test_state(BackendKind::WebDriver);
      state.connect_mode = ConnectMode::WebDriver {
        endpoint,
        browser_name: "safari".into(),
        protocol: crate::backend::webdriver::WebDriverProtocol::Classic,
        capabilities: None,
        headers: None,
        timeout: Some(1000),
      };
      state.ensure_instance("default").await.unwrap();
      if cancelled {
        tokio::select! {
          result = state.shutdown() => panic!("cleanup completed before cancellation: {result:?}"),
          result = receipt => result.unwrap(),
        }
      } else {
        assert!(
          state
            .shutdown()
            .await
            .unwrap_err()
            .to_string()
            .contains("provider cleanup unavailable")
        );
      }
      assert!(!state.is_connected());
      assert_eq!(state.pending_cleanup.len(), 1);
      assert!(
        state
          .ensure_instance("default")
          .await
          .unwrap_err()
          .to_string()
          .contains("unfinished cleanup")
      );
      let state = Arc::new(tokio::sync::RwLock::new(state));
      assert!(
        BrowserState::ensure_instance_shared(&state, "default")
          .await
          .unwrap_err()
          .to_string()
          .contains("unfinished cleanup")
      );
      state.write().await.shutdown().await.unwrap();
      assert!(state.read().await.pending_cleanup.is_empty());
      state.write().await.shutdown().await.unwrap();
      let requests = server.await.unwrap();
      assert_eq!(requests.len(), 4);
      assert_eq!(requests[2].0, "DELETE /session/cleanup HTTP/1.1");
      assert_eq!(requests[3].0, requests[2].0);
    }
  }

  #[tokio::test]
  async fn webkit_browser_override_does_not_inherit_chromium_window_flags() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_overrides_fn(Arc::new(|_| {
      Ok(crate::options::InstanceOverrides {
        kind: Some(crate::options::BrowserKind::WebKit),
        ..Default::default()
      })
    }));
    let (_, effective) = state.launch_spec().resolve_off_lock("webkit").await.unwrap();
    assert_eq!(effective.backend_kind, BackendKind::WebKit);
    assert!(!effective.args.iter().any(|arg| arg.starts_with("--window-size")));
  }

  #[test]
  fn test_instance_resolver_none_by_default() {
    let state = test_state(BackendKind::CdpPipe);
    assert!(state.instance_resolver_fn.is_none());
  }

  #[test]
  fn test_instance_resolver_returns_connect_url() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_resolver_fn(Arc::new(|instance| match instance {
      "staging" => Some(ConnectMode::ConnectUrl(
        "ws://127.0.0.1:9222/devtools/browser/abc".to_owned(),
      )),
      _ => None,
    }));

    // Resolver returns Some for "staging"
    let resolved = state.instance_resolver_fn.as_ref().unwrap()("staging");
    assert!(matches!(resolved, Some(ConnectMode::ConnectUrl(url)) if url.contains("9222")));

    // Resolver returns None for unknown instance (falls through to default)
    let resolved = state.instance_resolver_fn.as_ref().unwrap()("unknown");
    assert!(resolved.is_none());
  }

  #[test]
  fn bare_session_key_selects_a_configured_instance() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_known_instances(["staging".to_string(), "dev".to_string()]);

    // The whole point: `session: "staging"` used to mean "a context
    // named staging on the default instance", i.e. a browser with no
    // environment mapping at all.
    let key = state.session_key("staging");
    assert_eq!(&*key.instance, "staging");
    assert_eq!(&*key.context, "default");

    // A name that is NOT an instance keeps the context meaning.
    let key = state.session_key("my-scratch-session");
    assert_eq!(&*key.instance, "default");
    assert_eq!(&*key.context, "my-scratch-session");

    // Composite keys are unaffected.
    let key = state.session_key("staging:admin");
    assert_eq!(&*key.instance, "staging");
    assert_eq!(&*key.context, "admin");

    // `default` stays the default pair.
    let key = state.session_key("default");
    assert_eq!(&*key.instance, "default");
    assert_eq!(&*key.context, "default");
  }

  /// The vocabulary is per-state, so two states in one process disagree
  /// about a bare key without either one leaking into the other — which
  /// a process-global registry could not express.
  #[test]
  fn the_instance_vocabulary_is_per_state() {
    let mut with_staging = test_state(BackendKind::CdpPipe);
    with_staging.set_known_instances(["staging".to_string()]);
    let plain = test_state(BackendKind::CdpPipe);

    assert_eq!(&*with_staging.session_key("staging").instance, "staging");
    assert_eq!(&*plain.session_key("staging").instance, "default");
    assert_eq!(&*plain.session_key("staging").context, "staging");
    // The bare, vocabulary-free parse keeps the context meaning too.
    assert_eq!(&*SessionKey::parse("staging").context, "staging");
  }

  #[test]
  fn test_instance_overrides_fn_independent_of_resolver() {
    let mut state = test_state(BackendKind::CdpPipe);

    state.set_instance_overrides_fn(Arc::new(|instance| {
      Ok(crate::options::InstanceOverrides {
        args: vec![format!("--window-name={instance}")],
        user_data_dir: Some(format!("/profiles/{instance}")),
        headless: Some(true),
        ..Default::default()
      })
    }));

    state.set_instance_resolver_fn(Arc::new(|_| None));

    // Both callbacks set independently
    let overrides = state.instance_overrides_fn.as_ref().unwrap()("dev").expect("overrides");
    assert_eq!(overrides.args, vec!["--window-name=dev"]);
    assert_eq!(overrides.user_data_dir.as_deref(), Some("/profiles/dev"));
    assert_eq!(overrides.headless, Some(true));

    let resolved = state.instance_resolver_fn.as_ref().unwrap()("dev");
    assert!(resolved.is_none());
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn per_instance_overrides_reach_the_effective_launch() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.headless = false;
    state.set_instance_overrides_fn(Arc::new(|instance| {
      Ok(crate::options::InstanceOverrides {
        args: vec!["--instance-flag".into()],
        user_data_dir: Some(format!("/profiles/{instance}")),
        executable_path: Some("/bin/other-chrome".into()),
        headless: Some(true),
        backend: Some(BackendKind::CdpWs),
        kind: None,
        env: [("APP_ENV".to_string(), instance.to_string())].into_iter().collect(),
        ignore_default_args: Some(crate::options::IgnoreDefaultArgs::Some(vec!["--no-sandbox".into()])),
      })
    }));

    let spec = state.launch_spec();
    let (mode, eff) = spec.resolve_off_lock("staging").await.expect("resolve");

    assert!(matches!(mode, ConnectMode::Launch));
    assert!(eff.args.contains(&"--instance-flag".to_string()));
    assert_eq!(eff.user_data_dir.as_deref(), Some("/profiles/staging"));
    assert_eq!(eff.chromium_path, "/bin/other-chrome");
    assert!(eff.headless, "instance override beats the state default");
    assert_eq!(eff.backend_kind, BackendKind::CdpWs);
    assert_eq!(eff.env.get("APP_ENV").map(String::as_str), Some("staging"));

    // `ignoreDefaultArgs` must actually drop the switch.
    let flags = chrome_flags_with(eff.headless, &eff.args, eff.ignore_default_args.as_ref());
    assert!(!flags.iter().any(|f| f == "--no-sandbox"), "dropped: {flags:?}");
    assert!(flags.iter().any(|f| f == "--instance-flag"), "user args survive");
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn overrides_callback_error_aborts_the_launch() {
    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_overrides_fn(Arc::new(|instance| {
      Err(format!("no environment mapped for '{instance}'"))
    }));

    let spec = state.launch_spec();
    // Silently launching an unconfigured browser is the failure this
    // prevents: the caller would be on the wrong environment.
    let err = spec.resolve_off_lock("bogus").await.expect_err("must abort");
    assert!(err.to_string().contains("no environment mapped"), "{err}");
  }

  #[test]
  fn ignore_default_args_all_drops_every_builtin() {
    let flags = chrome_flags_with(
      false,
      &["--keep-me".to_string()],
      Some(&crate::options::IgnoreDefaultArgs::All),
    );
    assert_eq!(flags, vec!["--keep-me"]);
  }

  #[test]
  fn loading_an_extension_keeps_headless_defaults_and_removes_only_extension_blocker() {
    let flags = chrome_flags_with(true, &["--load-extension=/tmp/example-extension".to_string()], None);
    assert!(
      flags
        .iter()
        .any(|flag| flag == "--load-extension=/tmp/example-extension")
    );
    assert!(!flags.iter().any(|flag| flag == "--disable-extensions"));
    assert!(flags.iter().any(|flag| flag == "--headless"));
    assert!(flags.iter().any(|flag| flag == "--enable-automation"));
  }

  /// The headless switches are part of Playwright's `defaultArgs()`, so
  /// `ignoreDefaultArgs: true` drops them too. Gating only the other
  /// sections left a caller who asked for no defaults with a browser
  /// still forced headless and still colour-scheme-pinned.
  #[test]
  fn ignore_default_args_all_drops_the_headless_switches_too() {
    let flags = chrome_flags_with(
      true,
      &["--keep-me".to_string()],
      Some(&crate::options::IgnoreDefaultArgs::All),
    );
    assert_eq!(flags, vec!["--keep-me"], "got: {flags:?}");
  }

  #[test]
  fn a_named_headless_switch_can_be_dropped_on_its_own() {
    let flags = chrome_flags_with(
      true,
      &[],
      Some(&crate::options::IgnoreDefaultArgs::Some(vec![
        "--hide-scrollbars".to_string(),
      ])),
    );
    assert!(!flags.iter().any(|f| f == "--hide-scrollbars"), "got: {flags:?}");
    assert!(flags.iter().any(|f| f == "--headless"), "the rest survive");
  }

  /// `ignoreDefaultArgs` filters a Chromium switch list that the Firefox
  /// and `WebKit` launch paths do not have, so accepting it there and
  /// applying it nowhere would tell the caller a switch was dropped when
  /// nothing was.
  #[tokio::test(flavor = "multi_thread")]
  async fn ignore_default_args_is_refused_on_the_backends_without_defaults() {
    for backend in [BackendKind::Bidi, BackendKind::WebKit] {
      let mut state = test_state(backend);
      state.set_instance_overrides_fn(Arc::new(|_| {
        Ok(crate::options::InstanceOverrides {
          ignore_default_args: Some(crate::options::IgnoreDefaultArgs::All),
          ..Default::default()
        })
      }));
      let spec = state.launch_spec();
      let (_, eff) = spec.resolve_off_lock("x").await.expect("resolve");
      let Err(err) = spec.launch_browser(&eff).await else {
        panic!("{backend:?} must refuse ignoreDefaultArgs rather than drop it");
      };
      assert!(
        matches!(err, FerriError::Unsupported { .. }),
        "{backend:?} must report Unsupported, got {err}"
      );
      assert!(err.to_string().contains("ignoreDefaultArgs"), "{err}");
    }
  }

  /// An instance's profile directory and args have to reach EVERY
  /// backend's launch config, not just the Chromium ones — a config that
  /// is accepted, validated and then dropped is worse than a rejected
  /// one.
  #[tokio::test(flavor = "multi_thread")]
  async fn webkit_carries_the_instance_profile_and_args() {
    let mut state = test_state(BackendKind::WebKit);
    state.set_instance_overrides_fn(Arc::new(|instance| {
      Ok(crate::options::InstanceOverrides {
        args: vec!["--instance-flag".into()],
        user_data_dir: Some(format!("/profiles/{instance}")),
        ..Default::default()
      })
    }));
    let spec = state.launch_spec();
    let (_, eff) = spec.resolve_off_lock("staging").await.expect("resolve");

    // The launch itself needs a real WebKit build, so assert on the
    // config the launch would be given.
    let config = crate::backend::webkit::LaunchConfig {
      headless: eff.headless,
      env: eff.env.clone(),
      user_data_dir: eff.user_data_dir.as_ref().map(std::path::PathBuf::from),
      extra_args: eff.args.clone(),
      ..Default::default()
    };
    assert_eq!(
      config.user_data_dir.as_deref(),
      Some(std::path::Path::new("/profiles/staging"))
    );
    assert!(config.extra_args.contains(&"--instance-flag".to_string()));
  }

  #[tokio::test]
  async fn managed_devices_keep_browser_arguments_without_desktop_window_flags() {
    let state = BrowserState::with_plan(
      ConnectMode::Device {
        target: crate::device::DeviceTarget::Android(crate::android::AndroidOptions::default()),
        headless: true,
      },
      crate::options::LaunchPlan {
        backend: BackendKind::CdpWs,
        args: vec!["--enable-features=WebMCP".into()],
        ..Default::default()
      },
    );
    let (mode, effective) = state.launch_spec().resolve_off_lock("phone").await.unwrap();
    assert!(matches!(mode, ConnectMode::Device { .. }));
    assert_eq!(effective.args, ["--enable-features=WebMCP"]);
  }

  #[test]
  fn feature_switches_merge_so_defaults_survive_and_the_caller_wins() {
    let switches = |prefix: &str, args: &[&str]| {
      let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
      chrome_flags(true, &args)
        .into_iter()
        .filter_map(|flag| flag.strip_prefix(prefix).map(str::to_owned))
        .collect::<Vec<_>>()
    };
    let enabled = |args: &[&str]| switches("--enable-features=", args);
    let disabled = |args: &[&str]| switches("--disable-features=", args);

    assert_eq!(enabled(&[]), ["CDPScreenshotNewSurface"]);
    assert_eq!(
      enabled(&["--enable-features=WebMCP"]),
      ["CDPScreenshotNewSurface,WebMCP,WebMCPTesting,DevToolsWebMCPSupport"]
    );
    assert_eq!(
      enabled(&["--enable-features=Foo,DevToolsWebMCPSupport,WebMCP"]),
      ["CDPScreenshotNewSurface,Foo,DevToolsWebMCPSupport,WebMCP,WebMCPTesting"]
    );
    assert_eq!(
      enabled(&["--enable-features=WebMCP", "--enable-features=Foo<Trial:sashoush/1"]),
      ["CDPScreenshotNewSurface,WebMCP,Foo<Trial:sashoush/1,WebMCPTesting,DevToolsWebMCPSupport"]
    );

    let defaults = disabled(&[]);
    assert_eq!(defaults.len(), 1);
    assert!(defaults[0].contains("HttpsUpgrades"), "{defaults:?}");
    assert_eq!(
      disabled(&["--disable-features=Sashoush"]),
      [format!("{},Sashoush", defaults[0])]
    );

    // A later mention wins: the caller can turn a default either way.
    assert!(!disabled(&["--enable-features=HttpsUpgrades"])[0].contains("HttpsUpgrades"));
    assert_eq!(
      enabled(&["--enable-features=HttpsUpgrades"]),
      ["CDPScreenshotNewSurface,HttpsUpgrades"]
    );
    assert!(enabled(&["--disable-features=CDPScreenshotNewSurface"]).is_empty());
    let args = ["--enable-features=WebMCP", "--disable-features=WebMCPTesting"];
    assert_eq!(enabled(&args), ["CDPScreenshotNewSurface,WebMCP,DevToolsWebMCPSupport"]);
    assert!(disabled(&args)[0].ends_with(",WebMCPTesting"));
  }

  #[test]
  fn ignore_default_args_matches_on_switch_name() {
    // A default carrying a value is dropped by its bare name.
    let flags = chrome_flags_with(
      false,
      &[],
      Some(&crate::options::IgnoreDefaultArgs::Some(vec![
        "--disable-features".to_string(),
      ])),
    );
    assert!(
      !flags.iter().any(|f| f.starts_with("--disable-features")),
      "got: {flags:?}"
    );
    assert!(flags.iter().any(|f| f == "--no-sandbox"), "others survive");
  }

  #[tokio::test]
  async fn test_ensure_instance_uses_resolver_for_connect() {
    // Bind then drop to get a port that's definitely not listening.
    let port = {
      let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      l.local_addr().unwrap().port()
      // listener drops here, port is free
    };

    let mut state = test_state(BackendKind::CdpWs);
    state.set_instance_resolver_fn(Arc::new(move |instance| {
      if instance == "test-resolved" {
        Some(ConnectMode::ConnectUrl(format!(
          "ws://127.0.0.1:{port}/devtools/browser/test"
        )))
      } else {
        None
      }
    }));

    // Should attempt WebSocket connection to the dead port (fails fast with
    // "connection refused"), proving the resolver was invoked instead of launching.
    let result = Box::pin(state.ensure_instance("test-resolved")).await;
    assert!(
      result.is_err(),
      "Should fail with connection refused, proving resolver was invoked"
    );
    let err = result.unwrap_err().to_string();
    assert!(
      !err.contains("not found") && !err.contains("No such file"),
      "Error should be connection-related, not binary-not-found: {err}"
    );
  }

  #[tokio::test]
  async fn test_ensure_instance_skips_resolver_when_exists() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let call_count = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&call_count);

    let mut state = test_state(BackendKind::CdpPipe);
    state.set_instance_resolver_fn(Arc::new(move |_| {
      counter.fetch_add(1, Ordering::Relaxed);
      None // Fall through to default
    }));

    // First call: resolver should be called (but will fall through and try to launch)
    let _ = Box::pin(state.ensure_instance("test")).await;
    // Resolver was called exactly once (regardless of whether launch succeeded)
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
  }
}
