//! `AnyPage` — zero-cost enum dispatch across all four browser backends,
//! plus `JSHandle` <-> `AnyElement` remote conversion helpers.

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::console_message::ConsoleMessage;
use crate::error::{FerriError, Result};
use crate::events::EventEmitter;
use crate::network::Request as NetworkRequest;

use super::binding::{PageBinding, binding_result_expression};
use super::media::{MediaEmulation, MediaFeature, MediaFeatures, SharedMediaEmulation};
use super::types::split_key_combo;
use super::{
  AnyElement, AxNodeData, BackendClickArgs, BackendHoverArgs, BackendKind, BackgroundColor, ClearCookieOptions,
  CookieData, FrameInfo, MetricData, NavLifecycle, PageBackref, ScreenshotCapture, SharedPageState, bidi, cdp,
  keyboard, screencast, webdriver, webkit,
};

/// Page handle — enum dispatch across backends. Cheaply cloneable (Arc-based).
#[derive(Clone)]
pub enum AnyPage {
  Cdp(cdp::CdpPage),
  WebKit(webkit::WebKitPage),
  Bidi(bidi::BidiPage),
  WebDriver(webdriver::page::WebDriverPage),
}

/// Dispatch an async method call uniformly across all `AnyPage` variants.
macro_rules! page_dispatch {
  ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
    match $self {
      AnyPage::Cdp(p) => p.$method($($arg),*).await,
      AnyPage::WebKit(p) => p.$method($($arg),*).await,
      AnyPage::Bidi(p) => p.$method($($arg),*).await,
      AnyPage::WebDriver(p) => p.$method($($arg),*).await,
    }
  };
}

/// Dispatch a synchronous method call uniformly across all `AnyPage` variants.
macro_rules! page_dispatch_sync {
  ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
    match $self {
      AnyPage::Cdp(p) => p.$method($($arg),*),
      AnyPage::WebKit(p) => p.$method($($arg),*),
      AnyPage::Bidi(p) => p.$method($($arg),*),
      AnyPage::WebDriver(p) => p.$method($($arg),*),
    }
  };
}

/// Borrow a field present on all `AnyPage` variants.
macro_rules! page_dispatch_field {
  ($self:expr, $field:ident) => {
    match $self {
      AnyPage::Cdp(p) => &p.$field,
      AnyPage::WebKit(p) => &p.$field,
      AnyPage::Bidi(p) => &p.$field,
      AnyPage::WebDriver(p) => &p.$field,
    }
  };
}

impl AnyPage {
  /// Backend-agnostic per-page state embedded in every backend page handle.
  #[must_use]
  pub fn shared(&self) -> &SharedPageState {
    page_dispatch_field!(self, shared)
  }

  // ── Events ──

  /// Get the event emitter for this page.
  #[must_use]
  pub fn events(&self) -> &EventEmitter {
    &self.shared().events
  }

  /// Per-page dialog handler registry. Mirrors Playwright's server-side
  /// `DialogManager`. Register with
  /// [`crate::dialog::DialogManager::add_handler`] to receive live
  /// [`crate::dialog::Dialog`] handles when `alert` / `confirm` /
  /// `prompt` / `beforeunload` fire.
  #[must_use]
  pub fn dialog_manager(&self) -> &crate::dialog::DialogManager {
    &self.shared().dialog_manager
  }

  /// Per-page file-chooser handler registry. Mirrors Playwright's
  /// `FileChooser` dispatch path. Register with
  /// [`crate::file_chooser::FileChooserManager::add_handler`] to
  /// receive live [`crate::file_chooser::FileChooser`] handles when
  /// an `<input type=file>` click (or `showPicker()`) fires.
  #[must_use]
  pub fn file_chooser_manager(&self) -> &crate::file_chooser::FileChooserManager {
    &self.shared().file_chooser_manager
  }

  /// Per-page download handler registry. Mirrors Playwright's
  /// server-side download dispatch path. Register with
  /// [`crate::download::DownloadManager::add_handler`] to receive live
  /// [`crate::download::Download`] handles when the browser starts
  /// writing a file download.
  #[must_use]
  pub fn download_manager(&self) -> &crate::download::DownloadManager {
    &self.shared().download_manager
  }

  /// Idempotently fire the backend command that turns on
  /// file-chooser interception. Lazy parity with Playwright's
  /// `_updateFileChooserInterception` — the page-level listener
  /// loop is already subscribed to events, but the underlying
  /// browser only emits `fileChooserOpened` after we ask it to.
  /// Called from `Page::wait_for_file_chooser` and
  /// `Page::on('filechooser', ...)`.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `enable_file_chooser_intercept`, and returns
  /// `FerriError::Unsupported` on Classic `WebDriver`. Never fails on `BiDi`, which needs no setup.
  pub async fn enable_file_chooser_intercept(&self) -> Result<()> {
    match self {
      AnyPage::Cdp(p) => p.enable_file_chooser_intercept().await,
      AnyPage::WebKit(p) => p.enable_file_chooser_intercept().await,
      AnyPage::Bidi(_) => Ok(()),
      AnyPage::WebDriver(_) => Err(FerriError::unsupported(
        "Classic WebDriver does not expose file chooser interception; use locator.setInputFiles",
      )),
    }
  }

  /// Idempotently fire the backend command that turns on download
  /// reporting. Lazy parity with Playwright's per-context
  /// `Browser.setDownloadBehavior` (`crBrowser.ts:354`). Called from
  /// `Page::wait_for_download` and `Page::on('download', ...)`.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `enable_download_behavior`, and returns
  /// `FerriError::Unsupported` on Classic `WebDriver`. Never fails on `BiDi`, which needs no setup.
  pub async fn enable_download_behavior(&self) -> Result<()> {
    match self {
      AnyPage::Cdp(p) => p.enable_download_behavior().await,
      AnyPage::WebKit(p) => p.enable_download_behavior().await,
      AnyPage::Bidi(_) => Ok(()),
      AnyPage::WebDriver(_) => Err(FerriError::unsupported(
        "Classic WebDriver does not expose download events or browser download controls",
      )),
    }
  }

  /// Populate the weak back-reference to the outer `Arc<Page>`.
  /// Called by [`crate::page::Page::new`] / `Page::with_context`
  /// every time a new `Arc<Page>` is constructed — callers like the
  /// MCP server wrap the same backend page fresh on every tool
  /// invocation, so successive calls register their wrapper without
  /// invalidating older wrappers that remain alive.
  pub fn set_page_backref(&self, weak: std::sync::Weak<crate::page::Page>) {
    self.shared().page_backref.set(weak);
  }

  /// Upgrade the page back-reference to the public wrapper `Arc<Page>`,
  /// if one is currently alive. Used by the per-page → per-context
  /// event bridge to deliver a `Page` to context-level
  /// `'pageclose'`/`'pageload'` and to resolve a `Frame` for the
  /// frame-lifecycle mirror events. Returns `None` if every wrapper has
  /// been dropped (matches Playwright's weak `createHandle` guard).
  #[must_use]
  pub fn upgrade_page_backref(&self) -> Option<std::sync::Arc<crate::page::Page>> {
    self.page_backref_handle().upgrade()
  }

  /// Clone of the shared [`PageBackref`] slot. Lets long-lived
  /// listeners (the page→context event bridge) resolve the current
  /// wrapper per event WITHOUT holding a strong `AnyPage` — a strong
  /// clone pins the page's `EventEmitter` sender, so the listener's
  /// own subscription can never observe `Closed`.
  #[must_use]
  pub(crate) fn page_backref_handle(&self) -> PageBackref {
    self.shared().page_backref.clone()
  }

  /// Backend-native identity of this page's top-level target: CDP
  /// `targetId`, `BiDi` browsing-context id, `WebKit` `pageProxyId`.
  /// Used to correlate popups with their opener
  /// ([`super::PopupInfo::opener_target_id`]).
  pub(crate) fn backend_target_id(&self) -> String {
    match self {
      AnyPage::Cdp(p) => p.target_id.to_string(),
      AnyPage::WebKit(p) => p.page_proxy_id().to_string(),
      AnyPage::Bidi(p) => p.context_id.to_string(),
      AnyPage::WebDriver(p) => p.page_guid(),
    }
  }

  /// Unpause a browser-created popup once the popup pump has finished
  /// registering it (context init scripts, bindings, routes applied).
  /// CDP parks popups via `Target.setAutoAttach`'s
  /// `waitForDebuggerOnStart`; `WebKit` popup targets arrive `isPaused`
  /// and are held by `attach(defer_resume: true)`. `BiDi` popups run
  /// freely — no-op there.
  pub(crate) async fn resume_popup(&self) -> Result<()> {
    match self {
      AnyPage::Cdp(p) => p.resume_popup().await,
      AnyPage::WebKit(p) => p.resume_popup().await,
      AnyPage::Bidi(p) => p.first_document_committed().await,
      AnyPage::WebDriver(_) => Ok(()),
    }
  }

  /// Let a popup that is not this browser's to drive go: resumed, and on
  /// CDP detached from, so the auto-attach its claim armed holds none of
  /// the frames it opens in other processes paused.
  pub(crate) async fn let_go_of_popup(&self) -> Result<()> {
    self.resume_popup().await?;
    match self {
      AnyPage::Cdp(p) => p.detach_session().await,
      AnyPage::WebKit(_) | AnyPage::Bidi(_) | AnyPage::WebDriver(_) => Ok(()),
    }
  }

  /// Backend-owned frame cache shared across `Arc<crate::page::Page>`
  /// wrappers. MCP tool handlers construct a fresh wrapper per call,
  /// so storing the cache on the wrapper would reset it between
  /// `navigate` and the next `run_script` — losing the subframe
  /// entries the navigate's frame-event listener wrote. Pinning the
  /// cache to the backend keeps it alive for the lifetime of the
  /// underlying browser page.
  pub(crate) fn frame_cache(&self) -> &std::sync::Arc<std::sync::Mutex<crate::frame_cache::FrameCache>> {
    &self.shared().frame_cache
  }

  /// Firefox overrides only `prefers-color-scheme` per page; Classic
  /// `WebDriver` has no media emulation at all.
  fn emulated_media_features(&self) -> MediaFeatures {
    match self {
      AnyPage::Cdp(_) | AnyPage::WebKit(_) => MediaFeatures::ALL,
      AnyPage::Bidi(_) => MediaFeatures(&[MediaFeature::ColorScheme]),
      AnyPage::WebDriver(_) => MediaFeatures::NONE,
    }
  }

  fn media_emulation(&self) -> &SharedMediaEmulation {
    &self.shared().media_emulation
  }

  /// Whether `self` and `other` wrap the SAME underlying browser page
  /// (as opposed to being equal-valued clones of different pages).
  /// Compared via the per-backend-page `frame_cache` allocation, which
  /// every clone of a backend page shares. Used by the MCP server to
  /// validate its per-context `Page`-wrapper cache against the
  /// currently-active page.
  #[must_use]
  pub fn same_backend_page(&self, other: &AnyPage) -> bool {
    std::sync::Arc::ptr_eq(self.frame_cache(), other.frame_cache())
  }

  /// Console / page-error retention backing `page.consoleMessages()` /
  /// `page.pageErrors()`. Pinned to the backend page for the same
  /// reason as [`Self::frame_cache`]: wrappers are minted per tool
  /// call, but the history must span them.
  pub(crate) fn observed(&self) -> &std::sync::Arc<std::sync::Mutex<crate::observed::ObservedBuffers>> {
    &self.shared().observed
  }

  /// The page's `networkidle` lifecycle, on every backend that reports
  /// its requests. Pinned to the backend page like [`Self::observed`].
  pub(crate) fn network_idle(&self) -> Option<&std::sync::Arc<crate::network_idle::NetworkIdle>> {
    match self {
      AnyPage::Cdp(p) => Some(&p.network_idle),
      AnyPage::WebKit(p) => Some(&p.network_idle),
      AnyPage::Bidi(p) => Some(&p.network_idle),
      AnyPage::WebDriver(_) => None,
    }
  }

  /// Force a garbage-collection pass in the page's JS engine.
  /// Playwright: `page.requestGC(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `request_gc`.
  pub async fn request_gc(&self) -> Result<()> {
    page_dispatch!(self, request_gc())
  }

  /// Atomic latch consulted by `Page::new` to spawn the frame-event
  /// listener exactly once per backend page (instead of once per
  /// wrapper). Subsequent wrappers see the latch set and skip the
  /// spawn — see `Page::seed_frame_cache`.
  pub(crate) fn frame_listener_started(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
    &self.shared().frame_listener_started
  }

  /// Whether cookies are read and cleared through the document the page
  /// is on, as Classic `WebDriver` serves them, rather than for the whole
  /// context. Known once the page has cleared its cookies.
  #[must_use]
  pub(crate) fn cookies_follow_the_document(&self) -> bool {
    match self {
      Self::WebDriver(_) => true,
      Self::Bidi(page) => page.session.lacks("storage.deleteCookies"),
      Self::Cdp(_) | Self::WebKit(_) => false,
    }
  }

  /// The backend kind this page lives on. Used by action paths that
  /// branch per-backend (e.g. `tap` needs a CDP-only native touch API
  /// and returns `FerriError::Unsupported` on `BiDi` / `WebKit` where
  /// the protocol has no public touch injection primitive).
  #[must_use]
  pub fn kind(&self) -> BackendKind {
    match self {
      AnyPage::Cdp(p) => p.backend_kind(),
      AnyPage::WebKit(_) => BackendKind::WebKit,
      AnyPage::Bidi(_) => BackendKind::Bidi,
      AnyPage::WebDriver(_) => BackendKind::WebDriver,
    }
  }

  #[must_use]
  pub(crate) fn is_lightpanda(&self) -> bool {
    matches!(self, AnyPage::Cdp(p) if p.is_lightpanda())
  }

  // ── Frames ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `get_frame_tree`.
  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>> {
    page_dispatch!(self, get_frame_tree())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `evaluate_in_frame`.
  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>> {
    page_dispatch!(self, evaluate_in_frame(expression, frame_id))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `bring_to_front`.
  pub async fn bring_to_front(&self) -> Result<()> {
    page_dispatch!(self, bring_to_front())
  }

  /// Bring the page to the front and return once it is drawn there, for
  /// a browser that draws only the tab in front. The CDP backends are
  /// the ones that drive such a browser.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `bring_to_front_drawn`, or of `bring_to_front` on
  /// backends without one.
  pub async fn bring_to_front_drawn(&self) -> Result<()> {
    match self {
      Self::Cdp(page) => page.bring_to_front_drawn().await,
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => self.bring_to_front().await,
    }
  }

  /// Mark a child frame's `<iframe>` element in its parent frame with
  /// the child's frame id via the trace snapshot streamer's
  /// `markIframe` (see `crate::snapshotter::annotate_iframe`).
  /// Protocol-level on every backend: CDP `DOM.getFrameOwner`,
  /// `WebKit` `DOM.resolveNode {frameId}`, `BiDi`
  /// `browsingContext.locateNodes` with a context locator.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `mark_snapshot_iframe`.
  pub async fn mark_snapshot_iframe(
    &self,
    child_frame_id: &str,
    parent_frame_id: &str,
    streamer_global: &str,
  ) -> Result<()> {
    page_dispatch!(
      self,
      mark_snapshot_iframe(child_frame_id, parent_frame_id, streamer_global)
    )
  }

  /// The content-frame id for an `<iframe>`/`<frame>` element given its
  /// remote-object id. Deterministic on CDP and `WebKit`
  /// (`DOM.describeNode` → `contentFrameId`); `None` on `BiDi` (its
  /// deterministic path lives on the element handle via
  /// `content_frame_context`) or when the element hosts no frame.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `content_frame_id`. Never fails on `BiDi`, which
  /// answers `None`.
  pub async fn content_frame_id(&self, object_id: &str) -> Result<Option<String>> {
    match self {
      AnyPage::Cdp(p) => p.content_frame_id(object_id).await,
      AnyPage::WebKit(p) => p.content_frame_id(object_id).await,
      AnyPage::Bidi(_) => Ok(None),
      AnyPage::WebDriver(p) => p.content_frame_id(object_id).await,
    }
  }

  // ── Navigation ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `goto`.
  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<crate::network::Response>> {
    page_dispatch!(self, goto(url, lifecycle, timeout_ms, referer))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `wait_for_navigation`.
  pub async fn wait_for_navigation(&self) -> Result<()> {
    page_dispatch!(self, wait_for_navigation())
  }

  /// Snapshot navigation state before an input action (cheap, no RTT).
  /// CDP-only; returns a trivial snapshot on other backends.
  #[must_use]
  pub fn nav_snapshot(&self) -> (u64, u64) {
    match self {
      AnyPage::Cdp(p) => p.nav_snapshot(),
      _ => (0, 0),
    }
  }

  /// Wait (bounded, best-effort) for a navigation the just-completed action
  /// started to commit — Playwright's `waitForSignalsCreatedBy`. Zero cost
  /// when no navigation started; no-op on non-CDP backends (their
  /// auto-retrying locators cover the same case).
  pub async fn settle_navigation(&self, snap: (u64, u64), timeout_ms: u64) {
    if let AnyPage::Cdp(p) = self {
      p.settle_navigation(snap, timeout_ms).await;
    }
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `reload`.
  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<crate::network::Response>> {
    page_dispatch!(self, reload(lifecycle, timeout_ms))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `go_back`.
  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<crate::network::Response>> {
    page_dispatch!(self, go_back(lifecycle, timeout_ms))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `go_forward`.
  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<crate::network::Response>> {
    page_dispatch!(self, go_forward(lifecycle, timeout_ms))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `url`.
  pub async fn url(&self) -> Result<Option<String>> {
    page_dispatch!(self, url())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `title`.
  pub async fn title(&self) -> Result<Option<String>> {
    page_dispatch!(self, title())
  }

  // ── JavaScript ──

  /// Returns a script that ensures the selector engine is injected.
  /// Mirrored after Playwright's `injectedScript()`.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `injected_script`.
  pub async fn injected_script(&self) -> Result<String> {
    page_dispatch!(self, injected_script())
  }

  /// Call `function` on `element` in its frame's utility world, where the
  /// element is the browser's own: what the page put on it, a framework's
  /// own `value` property, is not seen there. `WebDriver` Classic has no
  /// other world, so the call runs in the page's.
  ///
  /// # Errors
  ///
  /// The protocol error, or what the function threw.
  pub async fn call_on_element_in_utility(
    &self,
    element: &AnyElement,
    function: &str,
  ) -> Result<Option<serde_json::Value>> {
    match (self, element) {
      (Self::Cdp(page), AnyElement::Cdp(element)) => {
        let known = page.known_utility_context(element);
        element.call_js_fn_value_in_utility(function, known).await
      },
      (_, AnyElement::Cdp(element)) => element.call_js_fn_value_in_utility(function, None).await,
      (_, AnyElement::Bidi(element)) => element.call_js_fn_value_in_utility(function).await,
      (AnyPage::WebKit(page), AnyElement::WebKit(element)) => {
        element.call_js_fn_value_in(&page.utility_contexts, function).await
      },
      (_, other) => other.call_js_fn_value(function).await,
    }
  }

  /// Evaluate an expression directly inside the frame's isolated utility
  /// world when the backend supports it (`CDP`), returning `None` on
  /// backends without a fast direct utility-eval path.
  pub async fn evaluate_in_utility(
    &self,
    frame_id: Option<&str>,
    expression: &str,
  ) -> Option<Result<Option<serde_json::Value>>> {
    match self {
      Self::Cdp(page) => Some(page.evaluate_in_utility_world(frame_id, expression).await),
      Self::WebKit(page) => Some(match frame_id {
        Some(fid) => page.evaluate_in_frame(expression, fid).await,
        None => page.evaluate(expression).await,
      }),
      Self::Bidi(page) => Some(match frame_id {
        Some(fid) => page.evaluate_in_frame(expression, fid).await,
        None => page.evaluate(expression).await,
      }),
      Self::WebDriver(_) => None,
    }
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `ensure_engine_injected`.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    page_dispatch!(self, ensure_engine_injected())
  }

  /// The engine a frame was given is gone or belongs to another realm, so
  /// the next [`Self::ensure_engine_injected`] has to inject it again.
  /// CDP installs it with a per-document init script and `WebDriver` runs
  /// the lazy injection on every call, so neither keeps state to forget.
  pub fn forget_engine(&self, frame_id: Option<&str>) {
    match self {
      Self::Bidi(page) => page.forget_engine(frame_id),
      Self::WebKit(page) => page.forget_engine(),
      Self::Cdp(_) | Self::WebDriver(_) => {},
    }
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `evaluate`.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    page_dispatch!(self, evaluate(expression))
  }

  // ── Elements ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `find_element`.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    match self {
      Self::Cdp(page) => page.find_element(selector).await,
      Self::WebKit(page) => page.find_element(selector).await,
      Self::Bidi(page) => page.find_element(selector).await,
      Self::WebDriver(page) => page.find_element(selector).await.map(AnyElement::WebDriver),
    }
  }

  /// Evaluate `js` and return the resulting DOM element, resolving in
  /// the execution context of `frame_id` (or the main frame when
  /// `frame_id` is `None`). Mirrors Playwright's `Frame`-bound element
  /// resolution: every Locator carries a Frame, and actions reach the
  /// backend with that frame's id threaded all the way through. CDP
  /// uses `Runtime.evaluate.contextId`; `BiDi` uses the browsing-context
  /// realm; `WebKit` currently has no per-frame execution context, so
  /// non-main `frame_id` values fall back to the main page (DOM access
  /// via `WKFrameInfo` is a separate gap).
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `evaluate_to_element`, and on Classic `WebDriver` the
  /// error of switching to `frame_id`.
  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement> {
    match self {
      Self::Cdp(page) => page.evaluate_to_element(js, frame_id).await,
      Self::WebKit(page) => page.evaluate_to_element(js, frame_id).await,
      Self::Bidi(page) => page.evaluate_to_element(js, frame_id).await,
      Self::WebDriver(page) => page
        .in_frame(frame_id)?
        .evaluate_to_element(js)
        .await
        .map(AnyElement::WebDriver),
    }
  }

  // ── Content ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `content`.
  pub async fn content(&self) -> Result<String> {
    page_dispatch!(self, content())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_content`.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    page_dispatch!(self, set_content(html))
  }

  /// Set the main frame's HTML and probe `document.readyState` + iframe
  /// presence in a single round trip where supported (CDP, `BiDi`, `WebKit`).
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_content`.
  pub async fn set_content_and_check(&self, html: &str, dom_content_loaded: bool) -> Result<Option<serde_json::Value>> {
    let check_js = if dom_content_loaded {
      "(() => { const s = document.readyState; return (s === 'interactive' || s === 'complete') ? (document.querySelector('iframe,frame') ? 2 : 1) : 0; })()"
    } else {
      "(() => document.readyState === 'complete' ? (document.querySelector('iframe,frame') ? 2 : 1) : 0)()"
    };
    match self {
      Self::Cdp(p) => p.set_content_and_check(html, check_js).await,
      Self::Bidi(p) => p.set_content_and_check(html, dom_content_loaded).await,
      Self::WebKit(p) => p.set_content_and_check(html, dom_content_loaded).await,
      Self::WebDriver(p) => {
        p.set_content(html).await?;
        Ok(p.evaluate(check_js).await.ok().flatten())
      },
    }
  }

  // ── Screenshots ──

  /// The engine half of a screenshot (`PageDelegate.takeScreenshot`).
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `take_screenshot`.
  pub async fn take_screenshot(&self, capture: ScreenshotCapture) -> Result<Vec<u8>> {
    page_dispatch!(self, take_screenshot(capture))
  }

  /// `PageDelegate.setBackgroundColor`: `None` restores the default.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_background_color`.
  pub async fn set_background_color(&self, color: Option<BackgroundColor>) -> Result<()> {
    match self {
      Self::Cdp(p) => p.set_background_color(color).await,
      Self::WebKit(p) => p.set_background_color(color).await,
      Self::Bidi(_) => bidi::BidiPage::set_background_color(color),
      Self::WebDriver(_) => webdriver::page::WebDriverPage::set_background_color(color),
    }
  }

  /// `PageDelegate.shouldToggleStyleSheetToSyncAnimations`: engines whose
  /// animation clock only catches up on a style recalc.
  #[must_use]
  pub fn should_toggle_style_sheet_to_sync_animations(&self) -> bool {
    !matches!(self, Self::Cdp(_))
  }

  // ── Accessibility ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `accessibility_tree`.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    page_dispatch!(self, accessibility_tree())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `accessibility_tree_with_depth`.
  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>> {
    page_dispatch!(self, accessibility_tree_with_depth(depth))
  }

  // ── Input ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `click_at`.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    page_dispatch!(self, click_at(x, y))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `click_at_opts`.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    page_dispatch!(self, click_at_opts(x, y, button, click_count))
  }

  /// Dispatch a click at page-level coordinates `(x, y)` with the full
  /// Playwright option bag: `button`, `click_count`, delay between
  /// press/release, and `steps` interpolated `mousemove` events between
  /// the current cursor and the target. The events carry the held
  /// modifiers; [`Self::ensure_modifiers`] sets them for one action.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `click_at_with`.
  pub async fn click_at_with(&self, x: f64, y: f64, args: &BackendClickArgs) -> Result<()> {
    page_dispatch!(self, click_at_with(x, y, args))
  }

  /// Dispatch a hover (no press/release) at `(x, y)` with `steps`
  /// interpolated `mousemove` events carrying the held modifiers.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `hover_at_with`.
  pub async fn hover_at_with(&self, x: f64, y: f64, args: &BackendHoverArgs) -> Result<()> {
    page_dispatch!(self, hover_at_with(x, y, args))
  }

  /// Dispatch a native tap at `(x, y)` — CDP `Input.dispatchTouchEvent`
  /// (`touchStart` + `touchEnd`), `WebKit` `Input.dispatchTapEvent`,
  /// `BiDi` a `touch`-pointer `input.performActions` sequence.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `tap_at_with`.
  pub async fn tap_at_with(&self, x: f64, y: f64) -> Result<()> {
    page_dispatch!(self, tap_at_with(x, y))
  }

  pub(crate) fn keyboard(&self) -> &keyboard::KeyboardState {
    &self.shared().keyboard
  }

  /// Hold exactly the modifiers in `modifiers` (a CDP mask), pressing
  /// the missing ones and releasing the extra ones, and return the mask
  /// that was held before, which restores it.
  ///
  /// # Errors
  ///
  /// Returns the first error from `key_down` or `key_up` while changing a modifier; modifiers
  /// changed before it stay changed.
  pub async fn ensure_modifiers(&self, modifiers: u32) -> Result<u32> {
    let restore = self.keyboard().modifiers();
    for modifier in ferridriver_types::keyboard::Modifier::ALL {
      let need = modifiers & modifier.bit() != 0;
      let held = self.keyboard().modifiers() & modifier.bit() != 0;
      if need && !held {
        self.key_down(modifier.key()).await?;
      } else if !need && held {
        self.key_up(modifier.key()).await?;
      }
    }
    Ok(restore)
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `move_mouse`.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    page_dispatch!(self, move_mouse(x, y))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `move_mouse_smooth`.
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    page_dispatch!(self, move_mouse_smooth(from_x, from_y, to_x, to_y, steps))
  }

  /// Dispatch a wheel event at `(x, y)`, the pointer's position: the
  /// browser scrolls whatever is under it, as a real wheel would.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `mouse_wheel`.
  pub async fn mouse_wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
    page_dispatch!(self, mouse_wheel(x, y, delta_x, delta_y))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `mouse_down`.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    page_dispatch!(self, mouse_down(x, y, button, click_count))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `mouse_up`.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    page_dispatch!(self, mouse_up(x, y, button, click_count))
  }

  /// Insert text at the focus without emitting keyboard events (only
  /// `input` event). This is Playwright's `keyboard.insertText()` semantic.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `insert_text`.
  pub async fn insert_text(&self, text: &str) -> Result<()> {
    page_dispatch!(self, insert_text(text))
  }

  /// Press `key` and keep it held. A held modifier applies to every
  /// later key, mouse and touch event until [`Self::key_up`].
  ///
  /// # Errors
  ///
  /// `InvalidArgument` for a key the US layout does not have.
  pub async fn key_down(&self, key: &str) -> Result<()> {
    let stroke = self.keyboard().down(key)?;
    let name = ferridriver_types::keyboard::resolve_control_or_meta(key, keyboard::MAC);
    page_dispatch!(self, dispatch_key_down(name, &stroke))
  }

  /// Release `key`.
  ///
  /// # Errors
  ///
  /// `InvalidArgument` for a key the US layout does not have.
  pub async fn key_up(&self, key: &str) -> Result<()> {
    let stroke = self.keyboard().up(key)?;
    let name = ferridriver_types::keyboard::resolve_control_or_meta(key, keyboard::MAC);
    page_dispatch!(self, dispatch_key_up(name, &stroke))
  }

  /// Press a key or a `+` combination (`"Control+a"`): every key goes
  /// down in order, the last one is held for `delay_ms`, and they come
  /// up in reverse.
  ///
  /// # Errors
  ///
  /// Returns the first error from `key_down` or `key_up`, including `FerriError::InvalidArgument`
  /// for a key the US layout does not have.
  pub async fn press_key(&self, key: &str, delay_ms: u64) -> Result<()> {
    let keys = split_key_combo(key);
    let Some((last, held)) = keys.split_last() else {
      return Ok(());
    };
    for k in held {
      self.key_down(k).await?;
    }
    self.key_down(last).await?;
    if delay_ms > 0 {
      tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    self.key_up(last).await?;
    for k in held.iter().rev() {
      self.key_up(k).await?;
    }
    Ok(())
  }

  /// Type one character: a key press when the US layout has a key for
  /// it, inserted text otherwise.
  ///
  /// # Errors
  ///
  /// Forwards the error of `press_key` or `insert_text`.
  pub async fn type_char(&self, ch: char) -> Result<()> {
    let mut buf = [0u8; 4];
    let ch = ch.encode_utf8(&mut buf);
    if ferridriver_types::keyboard::describe(ch, false).is_some() {
      self.press_key(ch, 0).await
    } else {
      self.insert_text(ch).await
    }
  }

  // ── Cookies ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `get_cookies`.
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    page_dispatch!(self, get_cookies())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_cookie`.
  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    page_dispatch!(self, set_cookie(cookie))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `delete_cookie`.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    page_dispatch!(self, delete_cookie(name, domain))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `clear_cookies`.
  pub async fn clear_cookies(&self) -> Result<()> {
    page_dispatch!(self, clear_cookies())
  }

  /// Clear cookies matching the given filters. If no filters, clears all.
  ///
  /// # Errors
  ///
  /// Returns an error when a filter pattern does not compile, or when reading or deleting cookies
  /// fails.
  pub async fn clear_cookies_filtered(&self, options: &ClearCookieOptions) -> Result<()> {
    if options.is_empty() {
      return self.clear_cookies().await;
    }
    let filter = options.compile()?;
    let cookies = self.get_cookies().await?;
    for c in &cookies {
      if filter.matches(c) {
        self.delete_cookie(&c.name, Some(&c.domain)).await?;
      }
    }
    Ok(())
  }

  // ── Emulation ──

  /// Apply a full [`crate::options::BrowserContextOptions`] bag. The
  /// single dispatch point for context-level state — each backend
  /// implementation fires the relevant protocol commands in parallel
  /// via `tokio::join!` and aggregates errors. Callers in
  /// `ContextRef::new_page` and the context-level public setters
  /// (`setOffline`, `setGeolocation`, etc.) mutate the bag and
  /// re-apply.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` for network or CPU throttling off Chromium, and otherwise
  /// the error of the backend's `apply_context_options` or of applying the context's media.
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    if !matches!(self, AnyPage::Cdp(_)) {
      let throttled = [
        (
          opts.network_conditions.is_some_and(|c| c.is_throttled()),
          "networkConditions",
        ),
        (opts.cpu_throttling_rate.is_some_and(|r| r > 1.0), "cpuThrottling"),
      ];
      if let Some((_, name)) = throttled.iter().find(|(asked, _)| *asked) {
        return Err(FerriError::unsupported(format!(
          "{name} needs Chromium: only the DevTools protocol can throttle a page"
        )));
      }
    }
    let (applied, media) = tokio::join!(
      async { page_dispatch!(self, apply_context_options(opts)) },
      self.emulate_context_media(opts.emulated_media()),
    );
    applied.and(media)
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `emulate_viewport`.
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    page_dispatch!(self, emulate_viewport(config))
  }

  /// `page.emulateMedia`: a partial update of the page's own overrides,
  /// which the context's media is the fallback for.
  ///
  /// # Errors
  ///
  /// Forwards the error of sending the media overrides; the page's previous overrides are kept when
  /// it fails.
  pub async fn emulate_media(&self, update: &crate::options::EmulateMediaOptions) -> Result<()> {
    let mut emulation = self.media_emulation().lock().await;
    let updated = update.layered_over(&emulation.page);
    let previous = std::mem::replace(&mut emulation.page, updated);
    let sent = self.send_media(&emulation).await;
    if sent.is_err() {
      emulation.page = previous;
    }
    sent
  }

  /// Set the media the page's context names, under any override the page
  /// made itself. A page whose context names nothing still gets the
  /// default colour scheme from here.
  pub(crate) async fn emulate_context_media(&self, context: crate::options::EmulateMediaOptions) -> Result<()> {
    let mut emulation = self.media_emulation().lock().await;
    let previous = std::mem::replace(&mut emulation.context, context);
    let sent = self.send_media(&emulation).await;
    if sent.is_err() {
      emulation.context = previous;
    }
    sent
  }

  /// Held under the emulation lock, so two updates cannot reach the
  /// browser in the opposite order to the one they were made in. A
  /// refused update is undone by the caller, so it does not ride along
  /// with the next one.
  async fn send_media(&self, emulation: &MediaEmulation) -> Result<()> {
    let (media, requested) = emulation.effective(self.emulated_media_features());
    if media.is_empty() {
      return Ok(());
    }
    match page_dispatch!(self, emulate_media(&media)) {
      Err(FerriError::Unsupported(reason)) if !requested => {
        tracing::debug!(target: "ferridriver::emulation", %reason, "the default colour scheme cannot be emulated here");
        Ok(())
      },
      result => result,
    }
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_extra_http_headers`.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    page_dispatch!(self, set_extra_http_headers(headers))
  }

  /// Set (or clear) HTTP credentials. Backs
  /// [`crate::ContextRef::set_http_credentials`] (Playwright
  /// `browserContext.setHTTPCredentials(creds | null)`).
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_http_credentials`.
  pub async fn set_http_credentials(&self, creds: Option<Vec<crate::options::HttpCredentials>>) -> Result<()> {
    page_dispatch!(self, set_http_credentials(creds))
  }

  /// The proxy this page's context sends through (its own, else the
  /// launch proxy): its credentials answer the proxy's challenges.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `set_proxy`, and returns `FerriError::Unsupported`
  /// for a proxy on Classic `WebDriver`.
  pub async fn set_proxy(&self, proxy: Option<crate::options::ProxyConfig>) -> Result<()> {
    match self {
      AnyPage::Cdp(p) => p.set_proxy(proxy).await,
      AnyPage::WebKit(p) => p.set_proxy(proxy).await,
      AnyPage::Bidi(p) => p.set_proxy(proxy).await,
      AnyPage::WebDriver(_) if proxy.is_some() => Err(FerriError::unsupported(
        "Classic WebDriver has no per-context proxy; set the proxy in the session's capabilities",
      )),
      AnyPage::WebDriver(_) => Ok(()),
    }
  }

  /// Reset any context-granted permissions. Backs
  /// [`crate::ContextRef::clear_permissions`] (Playwright
  /// `browserContext.clearPermissions`).
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `reset_permissions`.
  pub async fn reset_permissions(&self) -> Result<()> {
    page_dispatch!(self, reset_permissions())
  }

  // ── Heap profiling ──

  /// Take a V8 heap snapshot, as a `.heapsnapshot` file holds it.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Unsupported`] on `WebKit` and
  /// `BiDi`. The format is V8's. `JavaScriptCore` and `SpiderMonkey` each
  /// write something of their own that no reader of this one could open,
  /// and Playwright exposes neither, so this is a capability those
  /// backends do not have rather than one they have differently. There
  /// is nothing here to approximate.
  pub async fn take_heap_snapshot(&self) -> Result<String> {
    match self {
      Self::Cdp(p) => p.take_heap_snapshot().await,
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => Err(crate::error::FerriError::unsupported(
        "heap snapshots are V8's format, so they are only available on Chromium (CDP backends)",
      )),
    }
  }

  // ── Tracing ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `start_tracing`.
  pub async fn start_tracing(&self, categories: Option<&[String]>) -> Result<()> {
    page_dispatch!(self, start_tracing(categories))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `stop_tracing`.
  pub async fn stop_tracing(&self) -> Result<Vec<serde_json::Value>> {
    page_dispatch!(self, stop_tracing())
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `metrics`.
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    page_dispatch!(self, metrics())
  }

  // ── Ref resolution ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `resolve_backend_node`.
  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement> {
    page_dispatch!(self, resolve_backend_node(backend_node_id, ref_id))
  }

  // ── Event listeners ──

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    page_dispatch_sync!(self, attach_listeners(console_log, network_log, dialog_log));
  }

  // ── PDF generation ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `pdf`.
  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    page_dispatch!(self, pdf(opts))
  }

  // ── Screencast (video recording) ──

  /// The page's shared screencast. Every consumer subscribes here rather
  /// than starting the backend stream itself (see [`screencast`]).
  pub(crate) fn screencast_hub(&self) -> Result<&screencast::ScreencastHub> {
    match self {
      AnyPage::Cdp(p) => Ok(&p.screencast_hub),
      AnyPage::WebKit(p) => Ok(&p.screencast_hub),
      AnyPage::Bidi(p) => Ok(&p.screencast_hub),
      AnyPage::WebDriver(_) => Err(FerriError::unsupported(
        "Classic WebDriver does not expose a screencast stream",
      )),
    }
  }

  pub(crate) async fn subscribe_screencast(
    &self,
    params: screencast::ScreencastParams,
  ) -> Result<(
    screencast::ScreencastSubscription,
    tokio::sync::mpsc::UnboundedReceiver<screencast::ScreencastFrame>,
  )> {
    self.screencast_hub()?.subscribe(self, params).await
  }

  /// Starts the browser's own recorder (Chromium's
  /// `Page.startScreenRecording`); `None` where the backend or browser
  /// has none, and the caller records from the screencast instead.
  pub(crate) async fn start_screen_recording(
    &self,
    params: cdp::recording::RecordingParams,
  ) -> Option<tokio::sync::watch::Receiver<Option<cdp::recording::Recorded>>> {
    match self {
      AnyPage::Cdp(p) => p.start_screen_recording(params).await,
      AnyPage::WebKit(_) | AnyPage::Bidi(_) | AnyPage::WebDriver(_) => None,
    }
  }

  /// Stops the page's native recording, if one runs, so its file is
  /// written before the page's target closes.
  pub(crate) async fn finish_screen_recording(&self) {
    if let AnyPage::Cdp(p) = self {
      p.finish_screen_recording().await;
    }
  }

  async fn start_screencast_stream(&self, params: screencast::ScreencastParams) -> Result<screencast::RawScreencast> {
    let screencast::ScreencastParams {
      quality,
      max_width,
      max_height,
    } = params;
    match self {
      AnyPage::WebDriver(_) => Err(FerriError::unsupported(
        "Classic WebDriver does not expose a screencast stream",
      )),
      AnyPage::Cdp(p) => p.start_screencast(quality, max_width, max_height).await,
      AnyPage::WebKit(p) => p.start_screencast(quality, max_width, max_height).await,
      AnyPage::Bidi(p) => p.start_screencast(quality, max_width, max_height).await,
    }
  }

  async fn stop_screencast_stream(&self) -> Result<()> {
    page_dispatch!(self, stop_screencast())
  }

  // ── File upload ──

  /// Set the file list on a resolved `<input type=file>` element.
  /// Element-addressed (not selector-addressed) to mirror Playwright's
  /// `setInputFilePaths(handle, paths)`: the caller resolves the element
  /// through the selector engine in the owning frame immediately before
  /// this call, so a stale node surfaces as a retryable error and the
  /// locator funnel re-resolves. Dispatched through the page (not
  /// `AnyElement`) because `WebKit` pairs the DOM call with a
  /// browser-session `Playwright.grantFileReadAccess`.
  ///
  /// # Errors
  ///
  /// Forwards the error of the backend's file-input call, and returns a backend error when
  /// `element` belongs to another backend.
  pub async fn set_input_files_element(&self, element: &AnyElement, paths: &[String]) -> Result<()> {
    match (self, element) {
      (AnyPage::Cdp(_), AnyElement::Cdp(e)) => e.set_input_files(paths).await,
      (AnyPage::WebKit(p), AnyElement::WebKit(e)) => p.set_input_files_element(e, paths).await,
      (AnyPage::Bidi(p), AnyElement::Bidi(e)) => p.set_input_files_element(e, paths).await,
      (AnyPage::WebDriver(_), AnyElement::WebDriver(e)) => e.set_input_files(paths).await,
      _ => Err(crate::error::FerriError::backend(
        "set_input_files: element does not belong to this page's backend",
      )),
    }
  }

  // ── Network Interception & Raw Protocol Sessions ──

  /// Stable identity of this page for capture-time request attribution
  /// — the same value the per-page network listener stamps on every
  /// `Request::page_guid` (CDP `targetId` / `WebKit` `pageProxyId` /
  /// `BiDi` top-level context id).
  #[must_use]
  pub fn page_guid(&self) -> String {
    match self {
      Self::Cdp(p) => p.session_parts().1.to_string(),
      Self::WebKit(p) => p.page_proxy_id().to_string(),
      Self::Bidi(p) => p.page_guid(),
      Self::WebDriver(p) => p.page_guid(),
    }
  }

  /// Attach a raw [`crate::cdp_session::CdpSession`] to this page's
  /// target. Chromium-only, mirroring Playwright's
  /// `browserContext.newCDPSession(page)`.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` off Chromium, and otherwise the error of attaching the
  /// session to the page's target.
  pub async fn new_cdp_session(&self) -> Result<crate::cdp_session::CdpSession> {
    use crate::cdp_session::CdpSession;
    match self {
      Self::Cdp(p) => {
        let (transport, target_id) = p.session_parts();
        let event_override = p.is_lightpanda().then(|| p.primary_session_id()).flatten();
        CdpSession::attach_to_target(transport, target_id, event_override).await
      },
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => Err(crate::error::FerriError::unsupported(
        "CDP session is only available on Chromium (CDP backends)",
      )),
    }
  }

  /// A raw `WebKit` Inspector Protocol session on this page's main target.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Unsupported`] off the `WebKit`
  /// backend.
  pub fn new_webkit_session(&self) -> Result<crate::webkit_session::WebKitSession> {
    match self {
      Self::WebKit(p) => Ok(crate::webkit_session::WebKitSession::new(p.clone())),
      Self::Cdp(_) | Self::Bidi(_) | Self::WebDriver(_) => Err(crate::error::FerriError::unsupported(
        "WebKit session is only available on the webkit backend",
      )),
    }
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `route`.
  pub async fn route(&self, route: crate::route::RegisteredRoute) -> Result<()> {
    page_dispatch!(self, route(route))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `unroute`.
  pub async fn unroute(
    &self,
    matcher: &crate::url_matcher::UrlMatcher,
    scope: crate::route::RouteScope,
    handler_id: Option<usize>,
  ) -> Result<()> {
    page_dispatch!(self, unroute(matcher, scope, handler_id))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `unroute_all`.
  pub async fn unroute_all(
    &self,
    behavior: crate::options::UnrouteBehavior,
    scope: Option<crate::route::RouteScope>,
  ) -> Result<()> {
    page_dispatch!(self, unroute_all(behavior, scope))
  }

  // ── Lifecycle ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `close_page`.
  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<()> {
    page_dispatch!(self, close_page(opts))
  }

  /// Release the page's local resources (listener tasks, routes, emitter
  /// Close) without asking the browser to close it. For callers that have
  /// already destroyed the page browser-side — context disposal kills
  /// every page in one call — this is the other half of the teardown.
  pub fn dispose_local(&self) {
    page_dispatch_sync!(self, dispose_local());
  }

  /// CDP target id when this page is backed by CDP (`None` for other backends).
  #[must_use]
  pub(crate) fn cdp_target_id(&self) -> Option<&str> {
    match self {
      Self::Cdp(p) => Some(&p.target_id),
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => None,
    }
  }

  /// Whether this page modified browser-level download behaviour (enabled
  /// download interception or set `acceptDownloads: false`).
  #[must_use]
  pub(crate) fn download_behavior_touched(&self) -> bool {
    match self {
      Self::Cdp(p) => {
        p.download_behavior_enabled.load(std::sync::atomic::Ordering::Relaxed)
          || !p.accept_downloads.load(std::sync::atomic::Ordering::Relaxed)
      },
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => false,
    }
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.shared().is_closed()
  }

  // ── Exposed Functions ──

  /// Register a page binding whose callback receives the
  /// [`crate::events::BindingSource`] of the calling frame. The backend
  /// fills `source.page` (main frame id) and `source.frame` (the frame
  /// the page-side call originated from); `source.context` is filled by
  /// the context layer, which knows the composite session key.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `expose_binding`.
  pub async fn expose_binding(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    let binding = self.page_binding(binding);
    page_dispatch!(self, expose_binding(name, binding))
  }

  /// `binding` as the backend runs it: what it returns becomes the
  /// expression its page-side caller resolves with.
  fn page_binding(&self, binding: crate::events::ExposedBinding) -> PageBinding {
    let page = self.page_backref_handle();
    Arc::new(move |mut source, args| {
      let frame = source.frame.clone();
      source.live = page.upgrade();
      let returned = binding(source, args);
      let page = page.clone();
      Box::pin(async move { binding_result_expression(&page, &frame, returned.await?).await })
    })
  }

  /// [`Self::expose_binding`] for a popup whose first document has not
  /// run yet. CDP/`WebKit` popups are held paused, so only the
  /// paused-safe half (binding + init scripts) is sent — the first
  /// document installs the channel at document-start. `BiDi` has no
  /// pause primitive and its popup document is already live, so it
  /// takes the normal path.
  pub(crate) async fn expose_binding_pre_doc(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    let binding = self.page_binding(binding);
    match self {
      AnyPage::Cdp(p) => p.expose_binding_pre_doc(name, binding).await,
      AnyPage::WebKit(p) => p.expose_binding_pre_doc(name, binding).await,
      AnyPage::Bidi(p) => p.expose_context_binding(name, binding).await,
      AnyPage::WebDriver(p) => p.expose_binding(name, binding).await,
    }
  }

  /// [`Self::expose_binding`] for a binding the page's whole context has.
  /// A backend that cannot hold a popup back before its first document
  /// registers it on the context, where the popup inherits it.
  pub(crate) async fn expose_context_binding(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    match self {
      AnyPage::Bidi(p) => p.expose_context_binding(name, self.page_binding(binding)).await,
      _ => self.expose_binding(name, binding).await,
    }
  }

  /// [`Self::expose_callback`] for a callback the page's whole context has.
  pub(crate) async fn expose_context_callback(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    match self {
      AnyPage::Bidi(p) => p.expose_context_callback(name, self.page_binding(binding)).await,
      _ => self.expose_callback(name, binding).await,
    }
  }

  /// [`Self::add_init_script`] for a script the page's whole context has,
  /// `key` naming it across the context's pages.
  pub(crate) async fn add_context_init_script(&self, key: u64, source: &str) -> Result<String> {
    match self {
      AnyPage::Bidi(p) => p.add_context_init_script(key, source).await,
      _ => self.add_init_script(source).await,
    }
  }

  /// # Errors
  ///
  /// Forwards the error of `expose_binding`.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    let binding: crate::events::ExposedBinding = std::sync::Arc::new(move |_source, args| func(args));
    self.expose_binding(name, binding).await
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `remove_exposed_function`.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    page_dispatch!(self, remove_exposed_function(name))
  }

  /// Register the callback behind a function passed as an argument: the
  /// page reaches it only through `__playwright__binding__controller__`.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `expose_callback`.
  pub async fn expose_callback(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    let binding = self.page_binding(binding);
    page_dispatch!(self, expose_callback(name, binding))
  }

  /// [`Self::expose_callback`] for a popup whose first document has not run
  /// yet; see [`Self::expose_binding_pre_doc`].
  pub(crate) async fn expose_callback_pre_doc(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    let binding = self.page_binding(binding);
    match self {
      AnyPage::Cdp(p) => p.expose_callback_pre_doc(name, binding).await,
      AnyPage::WebKit(p) => p.expose_callback_pre_doc(name, binding).await,
      AnyPage::Bidi(p) => p.expose_context_callback(name, binding).await,
      AnyPage::WebDriver(p) => p.expose_callback(name, binding).await,
    }
  }

  pub async fn remove_callback(&self, name: &str) {
    match self {
      AnyPage::Cdp(p) => p.remove_callback(name).await,
      AnyPage::WebKit(p) => p.remove_callback(name).await,
      AnyPage::Bidi(p) => p.remove_callback(name).await,
      AnyPage::WebDriver(_) => {},
    }
  }

  // ── Init Scripts ──

  /// # Errors
  ///
  /// Forwards the error of the active backend's `add_init_script`.
  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    page_dispatch!(self, add_init_script(source))
  }

  /// # Errors
  ///
  /// Forwards the error of the active backend's `remove_init_script`.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    page_dispatch!(self, remove_init_script(identifier))
  }

  // ── Handle lifecycle ──

  /// ferridriver's equivalent of Playwright's
  /// `evaluateExpression(context, expr, { returnByValue, isFunction }, ...args)`
  /// (`/tmp/playwright/packages/playwright-core/src/server/javascript.ts:248`).
  /// Dispatches to each backend's native `call_utility_evaluate`.
  ///
  /// # Errors
  ///
  /// Forwards backend errors (protocol failure, page-side exception,
  /// handle/backend mismatch).
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> crate::error::Result<crate::js_handle::EvaluateResult> {
    page_dispatch!(
      self,
      call_utility_evaluate(fn_source, args, handles, frame_id, is_function, return_by_value)
    )
  }

  /// Release the backend object the given [`crate::js_handle::HandleRemote`] refers to.
  ///
  /// # Errors
  ///
  /// Forwards the backend's protocol error if the release call fails.
  pub async fn release_handle(&self, remote: &crate::js_handle::HandleRemote) -> crate::error::Result<()> {
    use crate::js_handle::HandleRemote;
    match (self, remote) {
      (Self::Cdp(p), HandleRemote::Cdp { object_id, session_id }) => {
        p.release_object_in_session(object_id, session_id.as_deref()).await
      },
      (Self::WebKit(p), HandleRemote::WebKit(obj)) => p.release_object(obj).await,
      (Self::Bidi(p), HandleRemote::Bidi { shared_id, handle }) => p.release_handle(shared_id, handle.as_deref()).await,
      (Self::WebDriver(p), HandleRemote::WebDriver { id, frame, element }) => {
        p.release_handle(id, frame, *element).await
      },
      (page, remote) => Err(crate::error::FerriError::Backend(format!(
        "handle/backend mismatch: {:?} handle on {:?} backend",
        remote,
        page.kind()
      ))),
    }
  }
}

/// Re-wrap a [`crate::js_handle::HandleRemote`] as a backend
/// [`AnyElement`]. Inverse of [`element_handle_remote`] — used by
/// [`crate::js_handle::JSHandle::as_element`] when a
/// [`crate::js_handle::JSHandle`] turns out to reference a DOM node
/// and needs to be re-packaged as an
/// [`crate::element_handle::ElementHandle`].
///
/// # Errors
///
/// Returns a backend-mismatch error when the remote's variant doesn't
/// line up with the page's backend kind.
pub fn element_from_remote(
  page: &AnyPage,
  remote: &crate::js_handle::HandleRemote,
) -> crate::error::Result<AnyElement> {
  use crate::js_handle::HandleRemote;
  match (page, remote) {
    (AnyPage::Cdp(p), HandleRemote::Cdp { object_id, session_id }) => Ok(AnyElement::Cdp(
      p.element_from_object_id(Arc::clone(object_id), session_id.clone()),
    )),
    (AnyPage::WebKit(p), HandleRemote::WebKit(obj)) => {
      Ok(AnyElement::WebKit(p.element_from_object_id(obj.to_string())))
    },
    (AnyPage::Bidi(p), HandleRemote::Bidi { shared_id, .. }) => {
      Ok(AnyElement::Bidi(p.element_from_shared_id(shared_id.clone())))
    },
    (
      AnyPage::WebDriver(p),
      HandleRemote::WebDriver {
        id,
        frame,
        element: true,
      },
    ) => Ok(AnyElement::WebDriver(webdriver::element::WebDriverElement::new(
      p.in_frame(Some(frame))?,
      id.clone(),
    ))),
    (page, remote) => Err(crate::error::FerriError::Backend(format!(
      "element_from_remote: backend mismatch — {:?} remote on {:?} backend",
      remote,
      page.kind()
    ))),
  }
}

/// Extract a [`crate::js_handle::HandleRemote`] from a backend [`AnyElement`].
///
/// # Errors
///
/// On Chromium, returns the error of resolving the element's remote object id. Never fails on the
/// other backends.
pub async fn element_handle_remote(element: &AnyElement) -> crate::error::Result<crate::js_handle::HandleRemote> {
  use crate::js_handle::HandleRemote;
  match element {
    AnyElement::Cdp(e) => {
      let obj = e.ensure_object_id().await?;
      Ok(HandleRemote::Cdp {
        object_id: obj,
        session_id: e.session_id(),
      })
    },
    AnyElement::WebKit(e) => Ok(HandleRemote::WebKit(std::sync::Arc::from(e.object_id()))),
    AnyElement::Bidi(e) => Ok(HandleRemote::Bidi {
      shared_id: e.shared_id.clone(),
      handle: None,
    }),
    AnyElement::WebDriver(e) => Ok(HandleRemote::WebDriver {
      id: e.id.clone(),
      frame: e.page.frame_id(),
      element: true,
    }),
  }
}

impl screencast::ScreencastSource for AnyPage {
  fn start_stream(
    &self,
    params: screencast::ScreencastParams,
  ) -> impl std::future::Future<Output = Result<screencast::RawScreencast>> + Send {
    self.start_screencast_stream(params)
  }

  fn stop_stream(&self) -> impl std::future::Future<Output = Result<()>> + Send {
    self.stop_screencast_stream()
  }

  fn is_closed(&self) -> bool {
    AnyPage::is_closed(self)
  }
}
