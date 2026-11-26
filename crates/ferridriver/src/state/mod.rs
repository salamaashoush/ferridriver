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
  /// What every context of this instance does when its active page closes.
  refocus: crate::context::Refocus,
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

/// Backend teardown extracted from [`BrowserState::detach_context`] so
/// callers can release the global state write lock before awaiting protocol I/O.
pub(crate) struct DetachedContext {
  browser: AnyBrowser,
  ctx_id: Option<String>,
  pages: Vec<AnyPage>,
  recycle: bool,
  origins: Vec<String>,
  hints: crate::backend::cdp::RetireHints,
}

impl DetachedContext {
  pub(crate) async fn finish(self) {
    // A page's native recording goes with its target; write it first.
    for page in &self.pages {
      page.finish_screen_recording().await;
    }
    if let Some(id) = self.ctx_id {
      // Tear down local page state & listener tasks synchronously before
      // handing the backend context to background retirement or disposal so
      // `page.isClosed()` is immediately true when `context.close()` returns.
      for page in &self.pages {
        page.dispose_local();
      }
      let _ = if self.recycle {
        Box::pin(self.browser.retire_context(&id, &self.origins, self.hints)).await
      } else {
        self.browser.dispose_context(&id).await
      };
    } else {
      for page in self.pages {
        let _ = page.close_page(crate::options::PageCloseOptions::default()).await;
      }
    }
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
      .or_insert_with(|| BrowserContext::new(name.to_string(), self.refocus))
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

/// Per-context init-script registry map: composite session key → the
/// scripts in registration order. Applied to every page of the context —
/// current pages at registration time, future pages by
/// `ContextRef::new_page`.
pub type ContextInitScripts = HashMap<String, Vec<ContextInitScript>>;

/// One context init script and every page it went onto, so disposing it
/// reaches the pages opened after it as well as those open before.
pub struct ContextInitScript {
  pub id: u64,
  pub source: String,
  pub installed: Arc<std::sync::Mutex<Vec<(crate::backend::AnyPage, String)>>>,
}

/// What becomes of a context when it is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClosedContexts {
  /// Disposed, with its pages.
  #[default]
  Disposed,
  /// Handed to the backend to empty and keep for the next context
  /// (`AnyBrowser::retire_context`).
  Recycled,
}

/// How [`BrowserState::list_opened_page`] lists a page.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Listing {
  /// A page opened through ferridriver, which becomes the active page.
  Active,
  /// A page the browser opened, which becomes the active page only when
  /// its context has none.
  Popup,
}

/// All browser state -- manages multiple Chrome instances, each with contexts and pages.
pub struct BrowserState {
  instances: HashMap<String, BrowserInstance>,
  pending_cleanup: Vec<(String, PendingOwner)>,
  shutdown_generation: u64,
  disposed: bool,
  /// Instance name → browser generation whose popup pump is running
  /// (see [`Self::claim_popup_pump`]).
  popup_pumps: HashMap<String, u64>,
  /// The lock this state is shared behind, once [`Self::into_shared`] put
  /// it there. Installing a browser is what starts its popup pump, and the
  /// pump registers popups through this handle. A pump started anywhere
  /// later (on the first page opened) is a pump a client that connected
  /// to a running browser and drives its existing tabs never gets.
  shared: std::sync::Weak<tokio::sync::RwLock<BrowserState>>,
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
  /// `launch({ chromiumSandbox })`: Chromium keeps its sandbox only when `true`.
  chromium_sandbox: Option<bool>,
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
  /// The proxy each instance was actually launched with, the state's or
  /// its own override, written when the launch succeeds so a context of
  /// that instance answers the right proxy's challenges.
  launched_proxies: LaunchedProxies,
  /// Loopback relays standing in for a context's authenticated SOCKS5
  /// proxy, which the browser cannot authenticate to itself; kept for the
  /// context's life.
  context_relays: Arc<std::sync::Mutex<HashMap<String, Arc<crate::socks_relay::SocksRelay>>>>,
  /// Milliseconds every input, focus and navigation action waits after it
  /// runs (`launch`/`connect({ slowMo })`), launched or connected alike.
  pub slow_mo: Option<u64>,
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
  /// The options a context gets when nobody gave it any: the device and
  /// context options an MCP server or session host is configured with,
  /// for the contexts it opens pages in. A context made with
  /// `browser.newContext()` has options of its own, even an empty set, and
  /// never falls back to these.
  pub context_defaults: Arc<std::sync::Mutex<Option<crate::options::BrowserContextOptions>>>,
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
  /// Per-context callbacks behind functions passed to a context init
  /// script with `exposeFunctions`: bindings every page in the context
  /// reaches through the bindings controller, never as a global.
  pub context_callbacks: Arc<tokio::sync::RwLock<HashMap<String, HashMap<String, crate::events::ExposedBinding>>>>,
  /// Per-context `WebAuthn` authenticators (`browserContext.credentials`).
  pub(crate) context_credentials: Arc<std::sync::Mutex<HashMap<String, Arc<crate::credentials::CredentialStore>>>>,
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
  /// The http(s) origins each context's frames have committed a document
  /// of, or were restored a storage state for: Playwright's
  /// `BrowserContext._origins`. `storageState()` reads the ones no live
  /// page is on through a page of its own.
  pub visited_origins: VisitedOrigins,
  pub closed_contexts: ClosedContexts,
  /// Contexts that were ever given HTTP credentials, by composite key,
  /// including ones that have since had them taken away. The browser
  /// caches what it authenticated with and offers no way to clear it, so
  /// such a context is never kept for another user.
  pub http_authenticated: Arc<std::sync::Mutex<rustc_hash::FxHashSet<String>>>,
  /// Each context's open pages, readable without this state's lock:
  /// what makes `context.pages()` synchronous, as in Playwright.
  pub context_pages: ContextPages,
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
  /// [`Self::ConnectUrl`] with Playwright's `connectOptions`: `headers` go on
  /// the WebSocket handshake (and the discovery request of an HTTP CDP
  /// endpoint), which is how a remote browser behind an authenticating
  /// gateway is reached; `timeout` bounds the connection.
  ConnectUrlWith {
    url: String,
    headers: Option<rustc_hash::FxHashMap<String, String>>,
    timeout: Option<u64>,
  },
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
      chromium_sandbox: plan.chromium_sandbox,
      extra_args: plan.args,
      base_ignore_default_args: plan.ignore_default_args,
      proxy: plan.proxy,
      launched_proxies: Arc::default(),
      context_relays: Arc::default(),
      slow_mo: plan.slow_mo,
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
      context_defaults: Arc::default(),
      har_recorders: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_har_updates: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_bindings: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_callbacks: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_credentials: Arc::new(std::sync::Mutex::new(HashMap::default())),
      context_ws_routes: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_routes: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_init_scripts: Arc::new(tokio::sync::RwLock::new(HashMap::default())),
      context_init_script_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      clock_installed: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashSet::default())),
      connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      storage_state_hydrated: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashSet::default())),
      visited_origins: VisitedOrigins::default(),
      closed_contexts: ClosedContexts::default(),
      http_authenticated: Arc::default(),
      context_pages: ContextPages::default(),
      record_har_installed: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashSet::default())),
      persistent_context: false,
      popup_pumps: HashMap::default(),
      shared: std::sync::Weak::new(),
      launch_permits: Arc::new(std::sync::Mutex::new(HashMap::default())),
    }
  }

  /// Put this state behind the lock every handle to it shares, and start
  /// the popup pump of each browser already installed in it.
  #[must_use]
  pub fn into_shared(self) -> Arc<tokio::sync::RwLock<Self>> {
    let state = Arc::new_cyclic(|shared| {
      let mut this = self;
      this.shared = std::sync::Weak::clone(shared);
      tokio::sync::RwLock::new(this)
    });
    // Nothing else holds the lock yet, so this cannot wait.
    if let Ok(mut guard) = state.try_write() {
      let installed: Vec<String> = guard.instances.keys().cloned().collect();
      for instance in installed {
        guard.start_popup_pump(&instance);
      }
    }
    state
  }

  /// Start `instance`'s popup pump unless its current browser generation
  /// already has one, so popups from every page, adopted or opened, get
  /// registered, configured and announced as `'page'` events. A state
  /// nobody shares has nowhere to register them, and starts none.
  fn start_popup_pump(&mut self, instance: &str) {
    let Some(state) = self.shared.upgrade() else {
      return;
    };
    if let Some(browser) = self.claim_popup_pump(instance) {
      crate::context::spawn_popup_pump(state, Arc::from(instance), &browser);
    }
  }

  /// Claim the popup pump for `instance`'s CURRENT browser generation.
  /// Returns the browser handle exactly once per generation; later calls
  /// (and calls before the instance exists) return `None`. A relaunch
  /// under the same name bumps the generation, so its install starts a
  /// fresh pump against the new browser.
  fn claim_popup_pump(&mut self, instance: &str) -> Option<AnyBrowser> {
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
        .map(|(name, _)| name.clone())
        // The `default` context opens its pages in the browser's own
        // context and never learns that context's id, but the browser
        // still names it on the pages those pages open.
        .or_else(|| {
          inst
            .contexts
            .get("default")
            .filter(|ctx| ctx.cdp_context_id.is_none())
            .map(|_| "default".to_string())
        }),
    }
  }

  /// Find an open page in `instance` by its backend target id (CDP
  /// targetId / `BiDi` context id / `WebKit` pageProxyId). Used to wire
  /// `page.opener()` for popups.
  pub(crate) fn find_page_by_backend_id(&self, instance: &str, backend_id: &str) -> Option<AnyPage> {
    let inst = self.instances.get(instance)?;
    let pages = || inst.contexts.values().flat_map(|ctx| ctx.pages.iter());
    pages()
      .find(|page| page.backend_target_id() == backend_id)
      // A popup a child frame opened names that frame (a `BiDi` browsing
      // context of its own) as its opener; the page is the one holding it.
      .or_else(|| {
        pages().find(|page| {
          page
            .frame_cache()
            .lock()
            .is_ok_and(|cache| cache.record(backend_id).is_some())
        })
      })
      .cloned()
  }

  /// The backend kind this state was constructed with. Cached at
  /// `with_plan` time and never mutated, so callers don't need to
  /// take the outer `RwLock` read guard to ask the question.
  #[must_use]
  pub fn backend_kind(&self) -> BackendKind {
    self.backend_kind
  }

  pub(crate) fn browser_kind(&self) -> crate::options::BrowserKind {
    self.browser_kind
  }

  pub(crate) fn engine_name(&self) -> &'static str {
    match self.browser_kind {
      crate::options::BrowserKind::Chromium => "chromium",
      crate::options::BrowserKind::Firefox => "firefox",
      crate::options::BrowserKind::WebKit | crate::options::BrowserKind::Safari => "webkit",
      crate::options::BrowserKind::Ladybird => "ladybird",
      crate::options::BrowserKind::Lightpanda => "lightpanda",
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

  /// Fetch a clone of the options bag for a composite key: its own, else
  /// [`Self::context_defaults`].
  #[must_use]
  pub fn get_context_options(&self, composite_key: &str) -> Option<crate::options::BrowserContextOptions> {
    let own = self.context_options.lock().ok()?.get(composite_key).cloned();
    own.or_else(|| {
      self
        .context_defaults
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    })
  }

  /// Set [`Self::context_defaults`].
  pub fn set_context_defaults(&self, options: Option<crate::options::BrowserContextOptions>) {
    *self
      .context_defaults
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = options;
  }

  /// The proxy a context's traffic leaves through: its own, else the one
  /// its instance was launched with. Playwright's
  /// `this._options.proxy || this._browser.options.proxy`.
  #[must_use]
  pub fn effective_proxy(&self, composite_key: &str) -> Option<crate::options::ProxyConfig> {
    let own = self
      .context_options
      .lock()
      .ok()
      .and_then(|map| map.get(composite_key).and_then(|options| options.proxy.clone()));
    own.or_else(|| {
      let instance = self.session_key(composite_key).instance;
      self
        .launched_proxies
        .lock()
        .ok()
        .and_then(|launched| launched.get(&*instance).map(|launched| launched.proxy.clone()))
    })
  }

  /// The proxy to create `composite`'s browser context with. An
  /// authenticated SOCKS5 proxy, which no browser can log in to, is swapped
  /// for a loopback relay that logs in for it, held until the context goes.
  ///
  /// # Errors
  ///
  /// The relay cannot start.
  pub(crate) async fn browser_facing_proxy(
    &self,
    composite: &str,
    proxy: &crate::options::ProxyConfig,
  ) -> Result<crate::options::ProxyConfig> {
    if !proxy.is_authenticated_socks() {
      return Ok(proxy.clone());
    }
    let relay = crate::socks_relay::SocksRelay::start(proxy).await?;
    let browser_proxy = relay.browser_proxy(proxy);
    self
      .context_relays
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(composite.to_string(), relay);
    Ok(browser_proxy)
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
    Arc::clone(&self.context_bindings)
  }

  /// Shared handle to the per-context WebSocket-route registry. Used by
  /// `ContextRef::route_web_socket` to register and by
  /// `ContextRef::new_page` to apply context-level routes onto a fresh
  /// page.
  #[must_use]
  pub fn context_ws_routes_handle(&self) -> Arc<tokio::sync::RwLock<ContextWsRoutes>> {
    Arc::clone(&self.context_ws_routes)
  }

  /// Shared handle to the per-context route registry. Used by
  /// `ContextRef::route` / `route_from_har` to register and by
  /// `ContextRef::new_page` to apply context-level routes onto a fresh
  /// page.
  #[must_use]
  pub fn context_routes_handle(&self) -> Arc<tokio::sync::RwLock<ContextRoutes>> {
    Arc::clone(&self.context_routes)
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
    Arc::clone(
      map
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(std::sync::RwLock::new(None))),
    )
  }

  /// Look up (or lazily create) the shared closed-flag for a context's
  /// composite key. See [`Self::context_closed`].
  #[must_use]
  pub fn get_or_create_context_closed(&self, key: &str) -> Arc<std::sync::atomic::AtomicBool> {
    let mut map = match self.context_closed.lock() {
      Ok(g) => g,
      Err(poisoned) => poisoned.into_inner(),
    };
    Arc::clone(
      map
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false))),
    )
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
}

/// Each context's open pages, by composite session key, outside the
/// state lock. Everything that changes a context's page list writes it
/// here too; a page the browser closed on its own stays listed until the
/// next such write, so a reader drops the closed ones.
#[derive(Clone, Default)]
pub struct ContextPages {
  by_context: Arc<std::sync::Mutex<HashMap<String, Vec<AnyPage>>>>,
}

impl ContextPages {
  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<AnyPage>>> {
    self
      .by_context
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  pub(crate) fn set(&self, composite: &str, pages: &[AnyPage]) {
    self.lock().insert(composite.to_string(), pages.to_vec());
  }

  /// The pages of `composite` that are still open, in the order they
  /// were opened.
  #[must_use]
  pub fn open(&self, composite: &str) -> Vec<AnyPage> {
    self
      .lock()
      .get(composite)
      .map(|pages| pages.iter().filter(|page| !page.is_closed()).cloned().collect())
      .unwrap_or_default()
  }

  fn forget(&self, composite: &str) {
    self.lock().remove(composite);
  }
}

/// Per-context visited origins, by composite session key. Shared with the
/// tasks that watch each page, so it sits outside the state lock.
#[derive(Clone, Default)]
pub struct VisitedOrigins {
  by_context: Arc<std::sync::Mutex<HashMap<String, std::collections::BTreeSet<String>>>>,
}

impl VisitedOrigins {
  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, std::collections::BTreeSet<String>>> {
    self
      .by_context
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Record the origin of a document a frame of `composite` committed.
  /// Only http(s) documents have storage to speak of, as in Playwright's
  /// `Frame.origin()`.
  pub fn visit(&self, composite: &str, url: &str) {
    if let Some(origin) = http_origin(url) {
      self.lock().entry(composite.to_string()).or_default().insert(origin);
    }
  }

  /// The origins of `composite`, in a stable order.
  #[must_use]
  pub fn of(&self, composite: &str) -> Vec<String> {
    self
      .lock()
      .get(composite)
      .map(|origins| origins.iter().cloned().collect())
      .unwrap_or_default()
  }

  /// Replace the origins of `composite`, as restoring a storage state
  /// does: what it wrote is all the context has.
  pub fn replace(&self, composite: &str, origins: impl IntoIterator<Item = String>) {
    self.lock().insert(composite.to_string(), origins.into_iter().collect());
  }

  fn forget(&self, composite: &str) {
    self.lock().remove(composite);
  }
}

/// The origin of an http(s) URL; `None` for every other scheme.
fn http_origin(url: &str) -> Option<String> {
  if !url.starts_with("http") {
    return None;
  }
  let origin = reqwest::Url::parse(url).ok()?.origin();
  origin.is_tuple().then(|| origin.ascii_serialization())
}

mod launch;
mod sessions;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod adoption_tests;

use launch::{LaunchSpec, LaunchedProxies, browser_executable, connect_browser};
pub(crate) use sessions::{CHROMIUM_SWITCHES, merge_feature_switches};
pub use sessions::{
  ContextInfo, PageInfo, SessionInfo, chrome_flags, chrome_flags_with, detect_chromium, detect_chromium_headless_shell,
  detect_firefox, detect_ladybird, detect_lightpanda, resolve_chromium,
};
