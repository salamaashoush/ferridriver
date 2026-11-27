//! Playwright `WebKit` page handle — the full ferridriver page API over
//! the PW `WebKit` Inspector protocol.
//!
//! A page owns three [`Session`]s: the root browser session (for
//! `Playwright.navigate` / screenshot), the page-proxy session (for
//! `Target.*` / `Dialog.*`), and the inner target session (for `Page.*`
//! / `Runtime.*` / `DOM.*` / `Network.*` / `Input.*`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use base64::Engine as _;
use serde_json::{Value, json};

use super::browser::{BrowserError, WebKitBrowser};
use super::connection::Session;
use super::element::WebKitElement;
use super::protocol::{self, Envelope};
use crate::backend::{
  AnyElement, AxNodeData, BackgroundColor, CookieData, FrameInfo, ImageFormat, MetricData, NavLifecycle,
  ScreenshotCapture, ScreenshotScale,
};
use crate::console_message::ConsoleMessage;
use crate::error::{FerriError, Result};
use crate::network::{Request as NetworkRequest, Response};
use crate::state::DialogEvent;
use ferridriver_types::keyboard::KeyStroke;

mod capture;
mod emulation;
mod eval;
mod interception;
mod navigation;

/// Name of the utility execution context — mirrors `UTILITY_WORLD_NAME`
/// in `wkPage.ts`.
pub const UTILITY_WORLD_NAME: &str = "__playwright_utility_world__";

/// Document-start bootstrap that suppresses the native context menu on
/// every page. A right-button click on the Playwright `WebKit` build
/// opens context-menu tracking that, with no active UI to dismiss it,
/// stays pending and swallows every subsequent synthetic mouse event — so
/// any action after a right-click silently does nothing. Cancelling the
/// default in a capture-phase listener clears that state while still
/// letting the page's own `contextmenu` handlers run (`preventDefault`
/// does not stop propagation), matching the headless `Chromium` backend
/// where a synthetic right-click never raises an OS menu.
const CONTEXT_MENU_SUPPRESSOR: &str =
  "window.addEventListener('contextmenu', function(e){ e.preventDefault(); }, true)";

/// Document-start shim for the API surface the Playwright `WebKit` build is
/// missing next to Safari, mirroring `wkPage.ts::_calculateBootstrapScript`.
///
/// `window.safari` and `window.GestureEvent` do not exist in this build, so
/// Safari feature detection (`!!window.GestureEvent`, `window.safari.push...`)
/// takes the non-Safari path and the page under test behaves unlike the browser
/// it is emulating. `PublicKeyCredential` is absent for the same reason. These
/// are the exact fragments Playwright injects, so a page sees the same surface
/// under ferridriver as under Playwright's own `WebKit`.
const SAFARI_SURFACE_SHIM: &str = concat!(
  "if (!window.safari) window.safari = { pushNotification: { toString() { return \"[object SafariRemoteNotification]\"; } } };\n",
  "if (!window.GestureEvent) window.GestureEvent = function GestureEvent() {};\n",
  "window.PublicKeyCredential ??= {",
  "  async getClientCapabilities() { return {}; },",
  "  async isConditionalMediationAvailable() { return false; },",
  "  async isUserVerifyingPlatformAuthenticatorAvailable() { return false; },",
  "};",
);

/// Document-start shim removing the orientation surface a desktop browser does
/// not have. Injected only when the page is NOT emulating a mobile device --
/// this build exposes `window.orientation` unconditionally, which makes a
/// desktop page look like a phone to any script sniffing for it.
const DESKTOP_ORIENTATION_SHIM: &str =
  "delete window.orientation;\ndelete window.ondevicemotion;\ndelete window.ondeviceorientation";

/// `navigator.platform` implied by an emulated user agent, mirroring
/// Playwright's `calculateUserAgentEmulation`.
///
/// Returns `None` when the UA names no platform this maps -- better to leave
/// the real value than to invent one.
fn navigator_platform_for(user_agent: &str) -> Option<&'static str> {
  let arm = user_agent.contains("ARM") || user_agent.contains("aarch64");

  if user_agent.contains("Android") {
    // Android is arm regardless of what else the UA says, per Playwright.
    return Some("Linux armv8l");
  }
  if user_agent.contains("iPhone OS") {
    return Some("iPhone");
  }
  if user_agent.contains("iPad; CPU OS") {
    return Some("iPad");
  }
  if user_agent.contains("Mac OS X") {
    return Some("MacIntel");
  }
  if user_agent.contains("Windows") {
    return Some("Win32");
  }
  if user_agent.to_lowercase().contains("linux") {
    return Some(if arm { "Linux aarch64" } else { "Linux x86_64" });
  }
  None
}

/// What `WebKit` answers a call anchored in a document that is gone. A
/// call the document went from under, which it does not answer at all,
/// is reported the same way.
const STALE_ANCHOR: &str = "Missing injected script for given objectId";

#[derive(Debug, Default)]
pub(crate) struct ViewportState {
  config: Option<crate::options::ViewportConfig>,
  bootstrap_mobile: bool,
}

#[derive(Debug, Default)]
struct AuthCredentials {
  site: Vec<crate::options::HttpCredentials>,
  proxy: Option<crate::options::HttpCredentials>,
}

/// The main frame's id in a `Page.getResourceTree` answer.
fn main_frame_id(resource_tree: Option<&Value>) -> Option<String> {
  resource_tree?
    .pointer("/frameTree/frame/id")
    .and_then(Value::as_str)
    .map(str::to_string)
}

/// Playwright `WebKit` page. Cheaply cloneable (single `Arc` pointer).
#[derive(Clone)]
pub struct WebKitPage(Arc<WebKitPageInner>);

impl std::ops::Deref for WebKitPage {
  type Target = WebKitPageInner;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[doc(hidden)]
pub struct WebKitPageInner {
  pub shared: crate::backend::SharedPageState,
  proxy: Session,
  /// Live target session. Swapped on cross-process navigation when
  /// `WebKit` creates a provisional target and commits it via
  /// `Target.didCommitProvisionalTarget` (mirrors `wkPage.ts::_setSession`).
  /// Wrapped in `ArcSwap` so listeners can replace it atomically without
  /// blocking concurrent senders on the old session.
  target: Arc<ArcSwap<Session>>,
  browser: Session,
  proxy_id: Arc<str>,
  /// Live target id. Swapped alongside [`Self::target`] on commit.
  target_id: Arc<ArcSwap<Arc<str>>>,
  context_id: Option<Arc<str>>,
  /// Set when `attach(defer_resume: true)` observed a paused popup
  /// target; cleared (and `Target.resume` sent) by `WebKitPage::resume_popup`.
  popup_paused: Arc<AtomicBool>,
  /// Latch: the `window.__fd` selector engine has been injected.
  engine_injected: Arc<EngineLatch>,
  /// Cached `objectId` of the main-world global. `Runtime.callFunctionOn`
  /// needs an `objectId` anchor (no `contextId` form); evaluating
  /// `"this"` once gives a stable handle into the page's main realm.
  /// Cleared on navigation — a new document means a new global.
  /// Mirrors `WKExecutionContext._contextGlobalObjectId`.
  global_object_id: Arc<std::sync::Mutex<Option<String>>>,
  /// Exposed-binding callback registry. Keyed by the JS-side function
  /// name; the listener task dispatches `Runtime.bindingCalled` events
  /// back through these callbacks with the calling frame's
  /// `BindingSource` (plain exposed functions are wrapped source-less
  /// by `AnyPage::expose_function`).
  pub(crate) exposed_fns: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::backend::PageBinding>>>,
  /// Idempotent latch for the `Runtime.addBinding` + listener setup.
  binding_initialized: Arc<AtomicBool>,
  /// Held while the channel installs, so a concurrent caller waits for
  /// the controller instead of finding the flag set and racing ahead.
  binding_install: Arc<tokio::sync::Mutex<()>>,
  /// Ordered list of `(identifier, source)` bootstrap-script fragments.
  /// PW `WebKit`'s `Page.setBootstrapScript` holds a SINGLE source that
  /// each call overwrites, so every `add_init_script` must re-send the
  /// JOINED accumulation (mirrors `wkPage.ts::_calculateBootstrapScript`
  /// and `_updateBootstrapScript`). Without this, registering the binding
  /// controller and then a `__fd_bc.add(name)` fragment left only the
  /// fragment at bootstrap time, so a freshly-navigated page hit a
  /// `__fd_bc` undefined error and `window[name]` never installed.
  init_scripts: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
  /// Monotonic id source for [`Self::init_scripts`] identifiers.
  init_script_seq: Arc<std::sync::atomic::AtomicU64>,
  /// Both halves of `Emulation.setAuthCredentials`: one command sets
  /// the site credentials and the proxy's together, so each is resent
  /// with the other.
  auth_credentials: Arc<std::sync::Mutex<AuthCredentials>>,
  /// Extra HTTP headers applied via `Network.setExtraHTTPHeaders`. The
  /// backend recreates the target session on every cross-document navigation, so
  /// (like Playwright's `_updateState`) the headers are stashed here and
  /// re-applied to each new provisional target session — otherwise a
  /// `setExtraHTTPHeaders` before a `goto` would be lost on the swap.
  pub(crate) extra_http_headers: Arc<std::sync::Mutex<Option<serde_json::Map<String, Value>>>>,
  /// Merged media / user-preference emulation state, stashed by
  /// [`WebKitPage::emulate_media`] for the same reason as
  /// [`Self::extra_http_headers`]: the backend swaps the target session
  /// on every cross-document navigation and drops active overrides, so
  /// the provisional-target handler replays them.
  pub(crate) emulated_media: Arc<std::sync::Mutex<Option<crate::options::EmulateMediaOptions>>>,
  /// The timezone this page was started with, and the only one it can
  /// have: `WebKit` honours `Page.setTimeZone` before a session's first
  /// document and ignores it after, so a later CHANGE is refused rather
  /// than silently dropped. Re-asserting the same value is a no-op and
  /// must stay one — [`WebKitPage::apply_context_options`] re-sends the whole
  /// bag on every context-level mutation.
  pub(crate) applied_timezone: Arc<std::sync::Mutex<Option<String>>>,
  /// Viewport emulation currently in force and whether the installed
  /// `Page.setBootstrapScript` omitted [`DESKTOP_ORIENTATION_SHIM`], stashed
  /// for provisional-target replay and unchanged-viewport skipping.
  pub(crate) emulated_viewport: Arc<std::sync::Mutex<ViewportState>>,
  /// Live request table, keyed by PW `WebKit` `requestId`. The network
  /// listener inserts on `Network.requestWillBeSent`, links responses
  /// on `Network.responseReceived`, and removes on terminal
  /// finished/failed events.
  pub(crate) requests: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::network::Request>>>,
  /// Slot holding the most-recent main-document `Request` so
  /// [`WebKitPage::goto`] / [`WebKitPage::reload`] / history traversals can resolve
  /// the navigation `Response` without polling.
  pub(crate) nav_request_slot: crate::network::NavRequestSlot,
  /// Registered network-interception routes. Listener consults this
  /// vec on `Network.requestIntercepted` to decide which handler to
  /// invoke.
  pub(crate) routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  /// Idempotent latch for `Network.setInterceptionEnabled` +
  /// `Network.addInterception` setup.
  pub(crate) intercept_enabled: Arc<AtomicBool>,
  /// `frameId` → `executionContextId` mapping populated by
  /// `Runtime.executionContextCreated` events. Used by
  /// [`WebKitPage::evaluate_in_frame`] to evaluate inside an iframe's realm.
  pub(crate) frame_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>,
  /// `frameId` → `executionContextId` of the frame's utility world
  /// ([`UTILITY_WORLD_NAME`]), where the page's changes to DOM objects do
  /// not reach.
  pub(crate) utility_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>,
  /// Per-execution-context cache of the global-object `objectId` keyed
  /// by `executionContextId`, plus a flag for whether the `window.__fd`
  /// selector engine has been injected into that context. The main
  /// frame's `__fd` injection is tracked by [`Self::engine_injected`] +
  /// [`Self::global_object_id`]; child frames each have their own realm
  /// where `__fd` must be injected independently (Playwright's
  /// `wkPage.ts` creates a `FrameExecutionContext` per
  /// `Runtime.executionContextCreated`). Without this, a
  /// `frame.waitForSelector` polling `window.__fd.selOne` in the child
  /// realm throws forever (`__fd` is undefined there) and the wait
  /// times out. Cleared on navigation.
  frame_engine_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<i64, FrameEngineContext>>>,
  /// Main-frame id captured from the initial `Page.getResourceTree`
  /// response. PW `WebKit`'s `frame_id` is distinct from `target_id`
  /// (the page proxy's id), so event routing keys off this rather than
  /// the target. Mirrors CDP's `main_frame_id` `OnceLock`.
  pub(crate) main_frame_id_cache: Arc<std::sync::Mutex<Option<String>>>,
  /// Live WebSocket table, keyed by PW `WebKit` `requestId`. The network
  /// listener inserts on `Network.webSocketCreated`, emits frames on
  /// `webSocketFrameSent` / `webSocketFrameReceived`, and removes the
  /// entry on `webSocketClosed`.
  pub(crate) websockets: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::network::WebSocket>>>,
  /// Dedicated workers of the current target, by `workerId`.
  pub(crate) workers: super::worker::Workers,
  pub(crate) screencast_hub: crate::backend::screencast::ScreencastHub,
  pub(crate) network_idle: Arc<crate::network_idle::NetworkIdle>,
  /// The buttons held down, which every mouse event reports, so a move
  /// after a press drags (Playwright's `Mouse._lastButton` / `_buttons`).
  pub(crate) mouse: Arc<std::sync::Mutex<super::input::MouseState>>,
  /// Per-page lifecycle signals fed by the target-session listener.
  /// Lets navigation methods await `Page.loadEventFired` /
  /// `domContentEventFired` / `frameNavigated` survivably across
  /// cross-process target swaps (the listener re-subscribes to the new
  /// target on commit, so events continue arriving here).
  pub(crate) lifecycle: Arc<LifecycleSignals>,
  /// Where the listener writes console/network/dialog events. Swapped
  /// in by [`WebKitPage::attach_listeners`] so the always-on listener (spawned
  /// from [`WebKitPage::attach`]) writes to the caller's logs once they're
  /// provided. Default-empty sinks let the raw API (no `attach_listeners`
  /// call) use page methods without `wait_for_lifecycle` wedging on the
  /// missing-listener path.
  pub(crate) console_log: Arc<ArcSwap<tokio::sync::RwLock<Vec<ConsoleMessage>>>>,
  pub(crate) network_log: Arc<ArcSwap<tokio::sync::RwLock<Vec<NetworkRequest>>>>,
  pub(crate) dialog_log: Arc<ArcSwap<tokio::sync::RwLock<Vec<DialogEvent>>>>,
}

impl std::ops::Deref for WebKitPageInner {
  type Target = crate::backend::SharedPageState;

  fn deref(&self) -> &Self::Target {
    &self.shared
  }
}

/// Per-child-frame execution-context bookkeeping for the `window.__fd`
/// selector engine. `global_object_id` anchors `Runtime.callFunctionOn`
/// at the child realm; `engine_injected` latches the one-time injection.
#[derive(Clone, Default)]
struct FrameEngineContext {
  global_object_id: Option<String>,
  engine_injected: bool,
}

impl WebKitPage {
  /// Attach to a freshly-created page proxy: wait for the inner
  /// `Target.targetCreated`, open the target session, run the standard
  /// `*.enable` initialisation (mirrors `WKPage._initializeSessionMayThrow`).
  /// `defer_resume`: when the target arrived paused, leave it paused
  /// and let `Self::resume_popup` release it — the popup pump needs
  /// the pause window to register listeners before the popup's first
  /// navigation fires. `new_page` passes `false` (resume immediately).
  ///
  /// # Errors
  ///
  /// Returns `BrowserError::Protocol` when the page proxy closes before announcing its page target,
  /// or an error when the `Page.enable`, `Runtime.enable`, `Network.enable`, `Console.enable`,
  /// `Worker.enable`, `Dialog.enable`, `Emulation.setActiveAndFocused` or `Page.getResourceTree`
  /// command fails.
  pub fn attach(
    browser: &WebKitBrowser,
    proxy: Session,
    context_id: Option<String>,
    defer_resume: bool,
  ) -> impl std::future::Future<Output = std::result::Result<Self, BrowserError>> + Send + 'static {
    let conn = Arc::clone(browser.connection());
    let root = browser.root().clone();
    let options = browser.context_options_lookup();
    async move {
      let proxy_id = proxy.page_proxy_id().unwrap_or_default().to_string();
      // ONE proxy subscription for the whole attach, opened before the
      // first await and handed to the listener loop at the end.
      //
      // `wait_for_first_page_target` used to open its own and drop it on
      // return, so every proxy event between that drop and the loop's
      // subscription was lost. A popup that is already navigating
      // announces its provisional target ~10ms after its own — inside
      // that window — and losing it meant the commit found nothing
      // stashed, the page kept sending to the destroyed target, and every
      // later call blocked with no response to wait for.
      let mut proxy_rx = proxy.events();
      let (target_id, is_paused) = wait_for_first_page_target(&mut proxy_rx).await?;
      let target = conn.target_session(&proxy_id, &target_id);
      // Same reason for the target session: everything between here and
      // `attach_listeners` is awaited protocol work, and the events are
      // buffered until the loop starts.
      let target_rx = target.events();
      // The frame cache moves with the frame events in wire order, from
      // the connection's reader; set up before `Page.enable` sends the
      // first of them.
      // The target id stands in for the main frame only until the resource
      // tree below (or the first frame event) reports the frame's own id.
      let frame_cache = Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::new(&target_id)));
      let frames = super::events::FrameTracker::new(Arc::clone(&frame_cache));
      frames.track_target(&target);
      frames.track_proxy(&proxy);

      let opts = context_id.as_deref().and_then(options);
      let pre_timezone = opts.as_ref().and_then(|o| o.timezone_id.clone());
      let initial_mobile = opts.as_ref().and_then(|o| o.is_mobile).unwrap_or(false);
      let resource_tree = Box::pin(initialize_target_session(
        &target,
        &proxy,
        opts.as_ref(),
        initial_mobile,
      ))
      .await?;
      let main_frame_id_cache = Arc::new(std::sync::Mutex::new(main_frame_id(Some(&resource_tree))));
      if let Some(root) = resource_tree.get("frameTree") {
        let mut frames = Vec::new();
        collect_frames(root, None, &mut frames);
        frame_cache
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .seed(frames);
      }

      let page = WebKitPage(Arc::new(WebKitPageInner {
        shared: crate::backend::SharedPageState::with_frame_cache(frame_cache),
        proxy,
        target: Arc::new(ArcSwap::from_pointee(target)),
        browser: root,
        proxy_id: Arc::from(proxy_id),
        target_id: Arc::new(ArcSwap::from_pointee(Arc::<str>::from(target_id))),
        context_id: context_id.map(Arc::from),
        popup_paused: Arc::new(AtomicBool::new(false)),
        engine_injected: Arc::default(),
        global_object_id: Arc::new(std::sync::Mutex::new(None)),
        exposed_fns: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
        binding_initialized: Arc::new(AtomicBool::new(false)),
        binding_install: Arc::new(tokio::sync::Mutex::new(())),
        init_scripts: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        init_script_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        extra_http_headers: Arc::new(std::sync::Mutex::new(None)),
        auth_credentials: Arc::default(),
        emulated_media: Arc::new(std::sync::Mutex::new(None)),
        emulated_viewport: Arc::new(std::sync::Mutex::new(ViewportState {
          config: None,
          bootstrap_mobile: initial_mobile,
        })),
        requests: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
        nav_request_slot: crate::network::NavRequestSlot::new(),
        routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        intercept_enabled: Arc::new(AtomicBool::new(false)),
        applied_timezone: Arc::new(std::sync::Mutex::new(pre_timezone)),
        frame_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
        utility_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
        frame_engine_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
        main_frame_id_cache,
        websockets: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
        workers: Arc::default(),
        screencast_hub: crate::backend::screencast::ScreencastHub::default(),
        network_idle: Arc::default(),
        mouse: Arc::default(),
        lifecycle: Arc::new(LifecycleSignals::default()),
        console_log: Arc::new(ArcSwap::from_pointee(tokio::sync::RwLock::new(Vec::new()))),
        network_log: Arc::new(ArcSwap::from_pointee(tokio::sync::RwLock::new(Vec::new()))),
        dialog_log: Arc::new(ArcSwap::from_pointee(tokio::sync::RwLock::new(Vec::new()))),
      }));
      // Spawn the always-on listener: lifecycle signals, frame events,
      // network log, console log, dialog log, route interception, and
      // cross-process target swap. Without this, raw users of the page
      // API (no `attach_listeners` from `BrowserState`) would see
      // `wait_for_lifecycle` wedge for the full timeout because no one
      // ever marks the lifecycle latches.
      super::events::attach_listeners(&page, target_rx, proxy_rx);
      // Browser-created popup targets arrive paused (`isPaused`, like the
      // provisional-target swap path) — resume only after the full
      // session init above so the popup's first document already sees
      // the bootstrap script and overrides (mirrors `WKPage`'s
      // initialize-then-`Target.resume` order). With `defer_resume` the
      // pause is held for `Self::resume_popup` instead.
      if is_paused {
        if defer_resume {
          page.popup_paused.store(true, Ordering::SeqCst);
        } else {
          let _ = page
            .proxy
            .send_typed(&ferridriver_webkit_proto::target::Resume {
              target_id: &page.target_id(),
            })
            .await;
        }
      }
      Ok(page)
    }
  }

  /// Release a popup target held paused by `attach(defer_resume:
  /// true)`. Idempotent; no-op for pages that never deferred.
  pub(crate) async fn resume_popup(&self) -> crate::error::Result<()> {
    if self.popup_paused.swap(false, Ordering::SeqCst) {
      let mut events = self.events.subscribe();
      self
        .proxy
        .send_typed(&ferridriver_webkit_proto::target::Resume {
          target_id: &self.target_id(),
        })
        .await
        .map_err(|e| crate::error::FerriError::backend(format!("webkit Target.resume: {e}")))?;
      self.first_document_committed(&mut events).await;
    }
    Ok(())
  }

  /// A popup starts on an empty document that is never the one it was
  /// opened for. It is handed on once the first real one has committed,
  /// or it has closed, so that what is done to it next lands in that
  /// document (`wkPage.ts` waits on the same commit before `reportAsNew`).
  async fn first_document_committed(&self, events: &mut crate::events::EventSubscription<crate::events::PageEvent>) {
    loop {
      let committed = self.lifecycle.notify.notified();
      if self.lifecycle.seen(NavLifecycle::Commit) || self.is_closed() {
        return;
      }
      tokio::select! {
        () = committed => {},
        event = events.recv() => {
          if matches!(event, None | Some(crate::events::PageEvent::Close)) {
            return;
          }
        },
      }
    }
  }

  #[must_use]
  pub fn page_proxy_id(&self) -> &str {
    &self.proxy_id
  }

  /// Current target id. Snapshot of the swappable inner — callers
  /// holding the returned `Arc<str>` keep that snapshot stable even if
  /// the live target is swapped underneath them.
  #[must_use]
  pub fn target_id(&self) -> Arc<str> {
    Arc::clone(&self.target_id.load())
  }

  /// Current target session. Cheap clone (Session is `Arc`-shaped
  /// inside). Swappable underneath callers via
  /// [`Self::swap_target_session`]; each call returns the live session
  /// at the moment of the call.
  #[must_use]
  pub(crate) fn target_session(&self) -> Session {
    Session::clone(&self.target.load())
  }

  /// Send a strongly typed `WebKit` protocol command on the live target session.
  pub(super) async fn send_typed<C: ferridriver_webkit_proto::Command>(&self, command: &C) -> Result<C::Response> {
    self.target_session().send_typed(command).await.map_err(conn_err)
  }

  /// Handle to the swappable target slot — used by the page-proxy
  /// listener so dispatch helpers read the LIVE session on every event
  /// (not a snapshot taken at attach time).
  #[must_use]
  pub(crate) fn target_swap(&self) -> Arc<ArcSwap<Session>> {
    Arc::clone(&self.target)
  }

  /// Atomically replace the live target session and target id. Called
  /// by the page-proxy listener when `WebKit` commits a provisional
  /// target after a cross-process navigation.
  pub(crate) fn swap_target_session(&self, new_session: Session, new_target_id: Arc<str>) {
    self.target.store(Arc::new(new_session));
    self.target_id.store(Arc::new(new_target_id));
    // The committed target lives in a fresh process — any frame
    // contexts / realm caches from the OLD process are invalid. The new
    // target's listener events will reseed the caches as frames attach +
    // navigate. The interception latch is NOT reset: when it is set, the
    // provisional-target handler already re-armed
    // `Network.setInterceptionEnabled` / `Network.addInterception` on the
    // session being committed here, so the latch still reflects the new
    // target's wire state.
    *self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    self.engine_injected.target_replaced();
    // The frame cache was emptied for the new process in wire order, by
    // the page's `FrameTracker`, when the commit arrived.
    self.main_frame_id_cache_clear();
  }

  /// Reset the main-frame id cache. Used after a cross-process target
  /// swap because the new target has a fresh main frame id distinct
  /// from the old one. The slot reads `None` until the listener
  /// re-seeds it via `Page.frameNavigated`.
  fn main_frame_id_cache_clear(&self) {
    *self
      .main_frame_id_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// Whether request interception has been armed on this page. Read by
  /// the provisional-target handler to replay
  /// `Network.setInterceptionEnabled` / `Network.addInterception` onto
  /// the new target session.
  #[must_use]
  pub(crate) fn interception_enabled(&self) -> bool {
    self.intercept_enabled.load(Ordering::SeqCst)
  }

  pub(crate) fn engine_injected_flag(&self) -> Arc<EngineLatch> {
    Arc::clone(&self.engine_injected)
  }

  pub(crate) fn global_object_id_slot(&self) -> Arc<std::sync::Mutex<Option<String>>> {
    Arc::clone(&self.global_object_id)
  }

  #[must_use]
  pub(crate) fn proxy_session(&self) -> &Session {
    &self.proxy
  }

  // ── Protocol helpers ──────────────────────────────────────────────────

  /// `Runtime.evaluate` on the target session. `return_by_value`
  /// controls whether the reply inlines the value or returns an
  /// `objectId` handle.
  /// A command the page's web process answers, so its reply comes after
  /// whatever that process sent before it.
  pub(crate) async fn web_process_round_trip(&self) -> Result<()> {
    self.runtime_evaluate("0", true).await.map(|_| ())
  }

  async fn runtime_evaluate(&self, expression: &str, return_by_value: bool) -> Result<Value> {
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({
          "expression": expression,
          "returnByValue": return_by_value,
        }),
      )
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let msg = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("evaluation threw")
        .to_string();
      return Err(FerriError::evaluation(msg));
    }
    Ok(resp)
  }

  /// Evaluate `expression` and return the inlined JSON value.
  async fn eval_value(&self, expression: &str) -> Result<Value> {
    self.eval_value_in_context(expression, None).await
  }

  async fn eval_value_in_context(&self, expression: &str, context_id: Option<i64>) -> Result<Value> {
    // Runtime.evaluate cannot await promises, including clock advancement.
    let params = json!({
      "functionDeclaration": "function(expression) { return (0, eval)(expression); }",
      "arguments": [{ "value": expression }],
      "returnByValue": true,
      "awaitPromise": true,
    });
    let resp = match context_id {
      Some(id) => {
        let mut params = params;
        params["objectId"] = Value::String(self.frame_context_anchor(id).await?);
        self.call_function_on(params).await.map_err(conn_err)?
      },
      None => self.call_on_global(params, false).await?,
    };
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("evaluation threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    Ok(
      resp
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null),
    )
  }

  /// Drop realm-scoped caches — a new document invalidates the engine
  /// injection and the cached global `objectId`.
  fn reset_realm(&self) {
    self.engine_injected.reset();
    *self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// Cached `objectId` of the main-world global, evaluating `"this"`
  /// once. The anchor `Runtime.callFunctionOn` runs against.
  async fn global_anchor(&self) -> Result<String> {
    let cached = self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    if let Some(id) = cached {
      return Ok(id);
    }
    let resp = self.runtime_evaluate("this", false).await?;
    let id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "global anchor: no objectId"))?
      .to_string();
    *self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id.clone());
    Ok(id)
  }

  /// `Runtime.callFunctionOn` on the live target. A call still running
  /// when its document is replaced is one `WebKit` never answers, so it
  /// is told apart from one that is merely slow: a target answers in
  /// order, and once a command sent after the document went is
  /// answered, a call sent before it has had its turn. It is reported
  /// like a call anchored in a document already gone, for the caller to
  /// make again in the document there now.
  async fn call_function_on(
    &self,
    mut params: Value,
  ) -> std::result::Result<Value, super::connection::ConnectionError> {
    use super::connection::ConnectionError;
    // As a user gesture, as Playwright's `WKExecutionContext` runs every
    // call: a script `focus()` moves the focus into a cross-origin frame
    // only from one.
    params["emulateUserGesture"] = json!(true);
    let session = self.target_session();
    let Some(context) = params
      .get("objectId")
      .and_then(Value::as_str)
      .and_then(object_id_context)
    else {
      return session.send(protocol::RUNTIME_CALL_FUNCTION_ON, params).await;
    };
    let mut document = self.engine_injected.watch(context);
    let mut call = Box::pin(session.send(protocol::RUNTIME_CALL_FUNCTION_ON, params));
    tokio::select! {
      biased;
      answer = &mut call => return answer,
      () = document.gone() => {},
    }
    if !document.on_live_target() {
      return Err(ConnectionError::Protocol(STALE_ANCHOR.to_string()));
    }
    let later = session.send(
      protocol::RUNTIME_EVALUATE,
      json!({ "expression": "0", "returnByValue": true }),
    );
    tokio::select! {
      biased;
      answer = &mut call => answer,
      _ = later => Err(ConnectionError::Protocol(STALE_ANCHOR.to_string())),
    }
  }

  /// `Runtime.callFunctionOn` anchored on the main-world global. A
  /// browser-initiated navigation can commit before its event resets the
  /// cached anchor, and `WebKit` then answers "Missing injected script for
  /// given objectId" without running the function. The realm caches are
  /// dropped and the call is made once more on a fresh anchor, with the
  /// engine re-injected first when the function needs `window.__fd`.
  async fn call_on_global(&self, mut params: Value, needs_engine: bool) -> Result<Value> {
    let mut retried = false;
    loop {
      if needs_engine {
        self.ensure_engine_injected().await?;
      }
      params["objectId"] = Value::String(self.global_anchor().await?);
      match self.call_function_on(params.clone()).await {
        Err(super::connection::ConnectionError::Protocol(message)) if !retried && message.contains(STALE_ANCHOR) => {
          retried = true;
          self.reset_realm();
        },
        result => return result.map_err(conn_err),
      }
    }
  }

  /// Resolve the execution-context id for `frame_id`, waiting briefly for
  /// the target listener to observe the frame's normal-world context
  /// (`Runtime.executionContextCreated` is consumed on the listener task,
  /// so a frame-scoped call issued right after a navigation can race the
  /// map update; cross-process swaps re-announce contexts with fresh
  /// ids). Erroring instead of falling back keeps a frame-scoped
  /// evaluate from silently running in the main frame.
  async fn resolve_frame_context(&self, frame_id: &str) -> Result<i64> {
    for _ in 0..200u32 {
      let context = self.frame_contexts.read().await.get(frame_id).copied();
      if let Some(id) = context {
        return Ok(id);
      }
      tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Err(FerriError::backend(format!(
      "webkit: no execution context found for frame '{frame_id}'. Frame may not be loaded yet."
    )))
  }

  /// `this` objectId in a specific execution context — the frame-scoped
  /// analogue of [`Self::global_anchor`]. Not cached: the context dies on
  /// every navigation of its frame and utility evals into child frames
  /// are rare.
  async fn anchor_in_context(&self, ctx_id: i64) -> Result<String> {
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({ "expression": "this", "returnByValue": false, "contextId": ctx_id }),
      )
      .await
      .map_err(conn_err)?;
    resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .map(std::string::ToString::to_string)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "frame anchor: no objectId"))
  }

  fn ensure_open(&self) -> Result<()> {
    if self.closed.load(Ordering::Relaxed) {
      return Err(FerriError::backend("webkit: page is closed"));
    }
    Ok(())
  }
}

fn initial_bootstrap_source(is_mobile: bool) -> String {
  if is_mobile {
    format!("{CONTEXT_MENU_SUPPRESSOR};\n{SAFARI_SURFACE_SHIM}")
  } else {
    format!("{CONTEXT_MENU_SUPPRESSOR};\n{SAFARI_SURFACE_SHIM};\n{DESKTOP_ORIENTATION_SHIM}")
  }
}

/// Pipeline domain enables, pre-page overrides, utility world creation,
/// initial bootstrap script, and resource tree fetch in exact FIFO wire order.
async fn initialize_target_session(
  target: &Session,
  proxy: &Session,
  opts: Option<&crate::options::BrowserContextOptions>,
  initial_mobile: bool,
) -> std::result::Result<Value, super::connection::ConnectionError> {
  let initial_bootstrap = initial_bootstrap_source(initial_mobile);
  let mut setup: Vec<futures::future::BoxFuture<'_, std::result::Result<(), super::connection::ConnectionError>>> =
    Vec::with_capacity(10);
  // Page before Runtime so executionContextCreated ordering holds.
  for method in [
    "Page.enable",
    "Runtime.enable",
    "Network.enable",
    "Console.enable",
    "Worker.enable",
  ] {
    setup.push(Box::pin(
      async move { target.send(method, json!({})).await.map(|_| ()) },
    ));
  }
  setup.push(Box::pin(async move {
    proxy
      .send_typed(&ferridriver_webkit_proto::dialog::Enable {})
      .await
      .map(|_| ())
  }));
  setup.push(Box::pin(async move {
    proxy
      .send_typed(&ferridriver_webkit_proto::emulation::SetActiveAndFocused { active: true })
      .await
      .map(|_| ())
  }));
  if let Some(opts) = opts {
    setup.push(Box::pin(async move {
      apply_pre_page_overrides(target, proxy, opts).await;
      Ok(())
    }));
  }
  setup.push(Box::pin(async move {
    let _ = target
      .send_typed(&ferridriver_webkit_proto::page::CreateUserWorld {
        name: UTILITY_WORLD_NAME,
      })
      .await;
    Ok(())
  }));
  setup.push(Box::pin(async move {
    let _ = target
      .send_typed(&ferridriver_webkit_proto::page::SetBootstrapScript {
        source: Some(&initial_bootstrap),
      })
      .await;
    Ok(())
  }));
  let (results, r_tree) = tokio::join!(
    futures::future::join_all(setup),
    target.send("Page.getResourceTree", json!({})),
  );
  for res in results {
    res?;
  }
  r_tree
}

async fn apply_pre_page_overrides(target: &Session, proxy: &Session, opts: &crate::options::BrowserContextOptions) {
  let mut cmds: Vec<futures::future::BoxFuture<'_, ()>> = Vec::new();
  if let Some(ua) = opts.user_agent.as_deref() {
    cmds.push(Box::pin(async move {
      let _ = target
        .send_typed(&ferridriver_webkit_proto::page::OverrideUserAgent { value: Some(ua) })
        .await;
    }));
  }
  if let Some(tz) = opts.timezone_id.as_deref() {
    cmds.push(Box::pin(async move {
      let _ = target
        .send_typed(&ferridriver_webkit_proto::page::SetTimeZone { time_zone: tz })
        .await;
    }));
  }
  if let Some(true) = opts.bypass_csp {
    cmds.push(Box::pin(async move {
      let _ = target
        .send_typed(&ferridriver_webkit_proto::page::SetBypassCsp { enabled: true })
        .await;
    }));
  }
  if let Some(o) = opts.offline {
    cmds.push(Box::pin(async move {
      let _ = target
        .send_typed(&ferridriver_webkit_proto::network::SetEmulateOfflineState { offline: o })
        .await;
    }));
  }
  if let Some(screen) = opts.screen {
    cmds.push(Box::pin(async move {
      let _ = target
        .send_typed(&ferridriver_webkit_proto::page::SetScreenSizeOverride {
          width: screen.width,
          height: screen.height,
        })
        .await;
    }));
  }
  if let Some(headers) = opts.extra_http_headers.as_ref() {
    let map: serde_json::Map<String, Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), Value::String(v.clone())))
      .collect();
    let headers_val = Value::Object(map);
    cmds.push(Box::pin(async move {
      let _ = target
        .send_typed(&ferridriver_webkit_proto::network::SetExtraHttpHeaders { headers: &headers_val })
        .await;
    }));
  }
  if matches!(opts.java_script_enabled, Some(false)) {
    cmds.push(Box::pin(async move {
      let _ = proxy
        .send_typed(&ferridriver_webkit_proto::emulation::SetJavaScriptEnabled { enabled: false })
        .await;
    }));
  }
  cmds.push(Box::pin(async move {
    let _ = grant_permissions(proxy, opts).await;
  }));
  futures::future::join_all(cmds).await;
}

/// `Emulation.grantPermissions` for every origin with grants, `*` for
/// the context-wide ones (`wkPage.ts::_grantPermissions`).
async fn grant_permissions(proxy: &Session, opts: &crate::options::BrowserContextOptions) -> Result<()> {
  let everywhere = opts.permissions.iter().map(|permissions| ("*", permissions));
  let by_origin = opts
    .origin_permissions
    .iter()
    .map(|(origin, permissions)| (origin.as_str(), permissions));
  for (origin, permissions) in everywhere.chain(by_origin) {
    let permissions = crate::permissions::webkit(permissions)?;
    proxy
      .send(
        "Emulation.grantPermissions",
        json!({ "origin": origin, "permissions": permissions }),
      )
      .await
      .map_err(conn_err)?;
  }
  Ok(())
}

/// Wait for the first non-provisional `Target.targetCreated` of type
/// `page` on the proxy session. Returns the target id plus whether the
/// target arrived paused (`isPaused` — browser-created popups do; the
/// caller resumes after session init).
async fn wait_for_first_page_target(
  rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
) -> std::result::Result<(String, bool), BrowserError> {
  while let Some(env) = rx.recv().await {
    if let Some(id) = page_target_id(&env) {
      let paused = env
        .params
        .get("targetInfo")
        .and_then(|i| i.get("isPaused"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
      return Ok((id, paused));
    }
  }
  Err(BrowserError::Protocol("page proxy closed before target".into()))
}

fn page_target_id(env: &Envelope) -> Option<String> {
  if env.method.as_deref()? != "Target.targetCreated" {
    return None;
  }
  let info = env.params.get("targetInfo")?;
  if info.get("type").and_then(Value::as_str) != Some("page")
    || info.get("isProvisional").and_then(Value::as_bool).unwrap_or(false)
  {
    return None;
  }
  Some(info.get("targetId")?.as_str()?.to_string())
}

/// Walk a PW `Page.FrameResourceTree` into flat [`FrameInfo`]s.
fn collect_frames(node: &Value, parent: Option<&str>, out: &mut Vec<FrameInfo>) {
  let Some(frame) = node.get("frame") else {
    return;
  };
  let frame_id = frame.get("id").and_then(Value::as_str).unwrap_or("").to_string();
  out.push(FrameInfo {
    frame_id: frame_id.clone(),
    parent_frame_id: parent.map(str::to_string),
    name: frame.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    url: frame.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
  });
  if let Some(children) = node.get("childFrames").and_then(Value::as_array) {
    for child in children {
      collect_frames(child, Some(&frame_id), out);
    }
  }
}

/// Map a [`super::connection::ConnectionError`] to a [`FerriError`].
fn conn_err(e: super::connection::ConnectionError) -> FerriError {
  e.into()
}

/// Extract the execution-context id (`injectedScriptId`) from a PW
/// `WebKit` `objectId`. The wire form is a JSON string like
/// `{"injectedScriptId":6,"id":42}`; the `injectedScriptId` is the
/// `Runtime.ExecutionContextId` the object lives in.
fn object_id_context(object_id: &str) -> Option<i64> {
  serde_json::from_str::<Value>(object_id)
    .ok()?
    .get("injectedScriptId")
    .and_then(Value::as_i64)
}

/// Decode a `Runtime.callFunctionOn` reply produced by the
/// `UTILITY_EVAL_WRAPPER` into an [`EvaluateResult`]. PW `WebKit`
/// signals errors via `wasThrown` (not CDP's `exceptionDetails`).
fn parse_eval_response(resp: &Value, return_by_value: bool) -> Result<crate::js_handle::EvaluateResult> {
  use crate::js_handle::{EvaluateResult, HandleRemote, JSHandleBacking};
  use crate::protocol::{SerializationContext, SerializedValue, SpecialValue};

  let result = resp
    .get("result")
    .ok_or_else(|| FerriError::protocol("Runtime.callFunctionOn", "call_utility_evaluate: no result"))?;
  if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
    let text = result
      .get("description")
      .or_else(|| result.get("value"))
      .and_then(Value::as_str)
      .unwrap_or("evaluation threw");
    return Err(FerriError::evaluation(text.to_string()));
  }

  if return_by_value {
    // The wrapper `JSON.stringify`d the isomorphic wire shape, so the
    // backend ships back a plain string; `null` is the `undefined`
    // sentinel.
    let wire = result.get("value").cloned().unwrap_or(Value::Null);
    let parsed: SerializedValue = match wire {
      Value::Null => SerializedValue::Special(SpecialValue::Undefined),
      Value::String(ref s) => {
        let inner: Value = serde_json::from_str(s)
          .map_err(|e| FerriError::backend(format!("call_utility_evaluate: inner JSON: {e}")))?;
        serde_json::from_value(inner)
          .map_err(|e| FerriError::backend(format!("call_utility_evaluate: parse result: {e}")))?
      },
      other => serde_json::from_value(other)
        .map_err(|e| FerriError::backend(format!("call_utility_evaluate: parse result: {e}")))?,
    };
    return Ok(EvaluateResult::Value(parsed));
  }

  if let Some(obj_id) = result.get("objectId").and_then(Value::as_str) {
    let is_node = result.get("subtype").and_then(Value::as_str) == Some("node");
    return Ok(EvaluateResult::Handle(
      JSHandleBacking::Remote(HandleRemote::WebKit(Arc::from(obj_id))),
      is_node,
    ));
  }

  let value = result.get("value").cloned().unwrap_or(Value::Null);
  let serialized = if value.is_null() {
    if result.get("type").and_then(Value::as_str) == Some("undefined") {
      SerializedValue::Special(SpecialValue::Undefined)
    } else {
      SerializedValue::Special(SpecialValue::Null)
    }
  } else {
    SerializedValue::from_json(&value, &mut SerializationContext::default())
  };
  Ok(EvaluateResult::Handle(JSHandleBacking::Value(serialized), false))
}

mod lifecycle;
#[cfg(test)]
mod tests;

pub(crate) use lifecycle::{EngineLatch, LifecycleSignals};
