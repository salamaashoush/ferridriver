//! High-level Page API -- mirrors Playwright's Page interface.
//!
//! All interaction methods auto-wait for element actionability.
//! Locator methods are lazy (don't query DOM until action).

mod capture;
mod eval;
mod events;
mod input_devices;
mod interaction;
mod navigation;

pub use input_devices::{
  Keyboard, KeyboardPressOptions, KeyboardTypeOptions, Mouse, MouseClickOptions, MouseDownOptions, MouseMoveOptions,
  MouseUpOptions, Touchscreen,
};

use crate::backend::AnyPage;
use crate::error::Result;
use crate::events::PageEvent;
use crate::frame_cache::FrameCache;
use crate::snapshot;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex as AsyncMutex;

/// No page-level default has been set, so the next level answers.
/// Playwright models the same thing as `undefined` in `TimeoutSettings`.
const UNSET_TIMEOUT: u64 = u64::MAX;

/// Playwright's floor when nothing in the chain sets a timeout.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// High-level page API, mirrors Playwright's Page interface.
/// Always constructed behind `Arc<Page>` — locators, frames, and consumers
/// hold Arc refs. No cloning of the Page struct itself.
/// One `locator.highlight()` in force.
#[derive(Debug, Clone)]
struct HighlightEntry {
  frame_id: Arc<str>,
  selector: String,
  style: Option<String>,
}

/// What [`Page::screenshot_turn`] hands out, released when dropped.
pub(crate) struct ScreenshotTurn<'a> {
  _queue: tokio::sync::MutexGuard<'a, ()>,
  _front: Option<tokio::sync::OwnedMutexGuard<()>>,
}

pub struct Page {
  pub(crate) inner: AnyPage,
  default_timeout: AtomicU64,
  /// Per Playwright: split from `default_timeout`. Navigation-family APIs
  /// (`goto`, `reload`, `go_back`, `go_forward`, `wait_for_url`) use this
  /// when explicitly set. The `u64::MAX` sentinel means "not set — fall
  /// back to `default_timeout`"; `0` means "infinite" (Playwright parity).
  default_navigation_timeout: AtomicU64,
  snapshot_tracker: Arc<AsyncMutex<snapshot::SnapshotTracker>>,
  /// Screenshots of one page run one at a time, as Playwright's
  /// `Screenshotter` queues them: each prepares and restores the page.
  screenshot_queue: AsyncMutex<()>,
  /// Playwright's `HighlightController`: the highlights `locator.highlight()`
  /// asked for, by frame. Re-applied every second while any remain, so a
  /// highlight survives the frame re-rendering or navigating.
  highlights: Mutex<Vec<HighlightEntry>>,
  highlight_timer: std::sync::atomic::AtomicBool,
  mouse_position: Mutex<(f64, f64)>,
  /// Emulated viewport, tracked client-side like Playwright's
  /// `Page._viewportSize` so [`Self::viewport_size`] is a sync accessor
  /// (`page.viewportSize(): null | { width, height }` never queries the
  /// browser). `None` = no viewport emulation applied (`viewport: null`
  /// or a page the runner never configured). Updated by
  /// [`Self::set_viewport_size`] and [`Self::apply_context_options`].
  viewport: Mutex<Option<(i64, i64)>>,
  /// The context's `strictSelectors`, cached so the selector-taking
  /// methods can read it without a lock or an await.
  ///
  /// Playwright resolves strictness once per query
  /// (`server/frameSelectors.ts:53`): an explicit per-call `strict`
  /// wins, otherwise the context option, which defaults to false. That
  /// is why `page.click(selector)` tolerates several matches while
  /// `page.locator(selector).click()` never does.
  strict_selectors: std::sync::atomic::AtomicBool,
  context_ref: Option<crate::context::ContextRef>,
  /// Human-readable `reason` passed to the last `close({ reason })` call,
  /// surfaced on subsequent `TargetClosed` errors — Playwright parity.
  close_reason: Mutex<Option<String>>,
  /// Client-side frame tree cache. Playwright keeps `Page._frames`,
  /// `Frame._parentFrame`, `Frame._url`, `Frame._detached` etc. up to
  /// date via wire-level `frameAttached`/`frameDetached`/`frameNavigated`
  /// events so that sync accessors (`mainFrame`, `frames`, `frame`,
  /// `parentFrame`, `childFrames`, `name`, `url`, `isDetached`) never
  /// await. ferridriver does the same. The actual storage lives on the
  /// backend (`AnyPage::frame_cache()`) so it outlives short-lived
  /// `Arc<Page>` wrappers — see `Page::seed_frame_cache` for the
  /// idempotent listener that keeps it fresh.
  frame_cache: Arc<Mutex<FrameCache>>,
  /// Live [`crate::video::Video`] handle when this page was opened
  /// inside a context with `record_video` enabled. `None` otherwise
  /// (matches Playwright's `page.video(): null | Video` —
  /// `types.d.ts:4756`). Populated by
  /// [`crate::state::BrowserState::register_opened_page`] at page
  /// registration time, when the recording runtime is spawned.
  video: Mutex<Option<Arc<crate::video::Video>>>,
  /// Registered `addLocatorHandler` callbacks. Consulted before every
  /// actionability retry (see [`crate::locator_handler::perform_checkpoint`]).
  locator_handlers: crate::locator_handler::LocatorHandlerRegistry,
  /// `page.coverage`'s runs, one of each kind at a time.
  coverage_state: AsyncMutex<crate::coverage::CoverageState>,
}

fn aria_snapshot_params(opts: &crate::options::AriaSnapshotOptions) -> serde_json::Value {
  serde_json::json!({ "mode": opts.mode.map(crate::options::AriaSnapshotMode::as_str), "depth": opts.depth, "boxes": opts.boxes })
}

impl Page {
  #[must_use]
  pub fn webmcp(self: &Arc<Self>) -> crate::web_mcp::WebMcp {
    self.main_frame().webmcp()
  }

  /// Interactive locator picker injected by [`Page::pick_locator`].
  const PICKER_JS: &'static str = include_str!("../picker.js");

  /// Construct a Page (no `BrowserContext`). Synchronous — only
  /// spawns the frame-cache listener; the eager `Page.getFrameTree`
  /// RTT was dropped (see `PERF_AUDIT` §M.4). The backend builds the
  /// cache around the main frame's id, and frame events keep it fresh.
  #[must_use]
  pub fn new(inner: AnyPage) -> Arc<Self> {
    let frame_cache = Arc::clone(inner.frame_cache());
    let page = Arc::new(Self {
      inner,
      default_timeout: AtomicU64::new(UNSET_TIMEOUT),
      default_navigation_timeout: AtomicU64::new(UNSET_TIMEOUT),
      snapshot_tracker: Arc::new(AsyncMutex::new(snapshot::SnapshotTracker::new())),
      screenshot_queue: AsyncMutex::new(()),
      highlights: Mutex::new(Vec::new()),
      highlight_timer: std::sync::atomic::AtomicBool::new(false),
      mouse_position: Mutex::new((0.0, 0.0)),
      viewport: Mutex::new(None),
      strict_selectors: std::sync::atomic::AtomicBool::new(false),
      context_ref: None,
      close_reason: Mutex::new(None),
      frame_cache,
      video: Mutex::new(None),
      locator_handlers: crate::locator_handler::LocatorHandlerRegistry::default(),
      coverage_state: AsyncMutex::new(crate::coverage::CoverageState::default()),
    });
    // Wire the backend's weak back-reference before the frame cache
    // starts seeding — the file-chooser listener (spawned in
    // `attach_listeners`) reads this slot per event and silently
    // drops events that arrive before the page is addressable.
    page.inner.set_page_backref(Arc::downgrade(&page));
    page.seed_frame_cache();
    page
  }

  /// Construct a Page bound to a `BrowserContext`. Same init
  /// contract as [`Self::new`].
  #[must_use]
  pub fn with_context(inner: AnyPage, context: crate::context::ContextRef) -> Arc<Self> {
    let frame_cache = Arc::clone(inner.frame_cache());
    let page = Arc::new(Self {
      inner,
      default_timeout: AtomicU64::new(UNSET_TIMEOUT),
      default_navigation_timeout: AtomicU64::new(UNSET_TIMEOUT),
      snapshot_tracker: Arc::new(AsyncMutex::new(snapshot::SnapshotTracker::new())),
      screenshot_queue: AsyncMutex::new(()),
      highlights: Mutex::new(Vec::new()),
      highlight_timer: std::sync::atomic::AtomicBool::new(false),
      mouse_position: Mutex::new((0.0, 0.0)),
      viewport: Mutex::new(None),
      strict_selectors: std::sync::atomic::AtomicBool::new(false),
      context_ref: Some(context),
      close_reason: Mutex::new(None),
      frame_cache,
      video: Mutex::new(None),
      locator_handlers: crate::locator_handler::LocatorHandlerRegistry::default(),
      coverage_state: AsyncMutex::new(crate::coverage::CoverageState::default()),
    });
    page.inner.set_page_backref(Arc::downgrade(&page));
    page.seed_frame_cache();
    page
  }

  // ── Frame cache plumbing (Playwright-parity sync frame accessors) ─────

  /// Read from the Page's frame cache under the shared lock.
  pub(crate) fn with_frame_cache<R>(&self, f: impl FnOnce(&FrameCache) -> R) -> R {
    match self.frame_cache.lock() {
      Ok(g) => f(&g),
      Err(poisoned) => f(&poisoned.into_inner()),
    }
  }

  /// Replace what the cache holds with the frame tree a backend reported.
  pub(crate) fn reseed_frames(&self, infos: Vec<crate::backend::FrameInfo>) {
    if let Ok(mut cache) = self.frame_cache.lock() {
      cache.seed(infos);
    }
  }

  /// Update frame state before public event delivery. The backend latch
  /// keeps wrappers of the same page from installing duplicate observers.
  fn seed_frame_cache(self: &Arc<Self>) {
    if self
      .inner
      .frame_listener_started()
      .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
      return;
    }
    let observer = page_state_observer(
      self.inner.kind(),
      Arc::clone(&self.frame_cache),
      Arc::clone(self.inner.observed()),
      self.inner.network_idle().cloned(),
    );
    // Frame and observation state must be current before public event delivery.
    let installed = self.inner.events().set_state_observer(Arc::new(observer));
    debug_assert!(installed, "a backend page installs its state observer once");
    let mut rx = self.inner.events().subscribe();
    // Trace identity captured at spawn: console / page-lifecycle events
    // mirror into the context's live trace (when one is recording).
    let trace_composite = self.context_ref.as_ref().map(super::context::ContextRef::composite);
    let trace_page_id = crate::trace::trace_page_id(&self.inner);
    let trace_inner = self.inner.clone();
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        // Cheap probe (RwLock read + map hit) — only pays when tracing.
        if let Some(recorder) = trace_composite.as_deref().and_then(crate::trace::recorder_for) {
          crate::trace::record_page_event(&recorder, &trace_page_id, &event);
          // markIframe keeps child-frame snapshots inlining in the
          // viewer (snapshotter.ts::_annotateFrameHierarchy fires on
          // FrameAttached). Spawned: a bookkeeping listener must never
          // block on protocol round-trips.
          if recorder.snapshots
            && let PageEvent::FrameAttached(info) = &event
            && let Some(parent_id) = info.parent_frame_id.clone()
          {
            let inner = trace_inner.clone();
            let child_id = info.frame_id.clone();
            tokio::spawn(async move {
              let _ = crate::snapshotter::annotate_iframe(&inner, &child_id, &parent_id).await;
            });
          }
        }
        if matches!(event, PageEvent::Close) {
          break;
        }
      }
    });
  }

  /// Refresh the frame cache from the backend without touching the
  /// listener. Useful when a caller wants to guarantee freshness before
  /// reading sync accessors (e.g. right after a navigation where event
  /// delivery is racing with the caller).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend's `get_frame_tree()` call fails.
  pub async fn sync_frames(&self) -> Result<()> {
    let infos = self.inner.get_frame_tree().await?;
    self
      .frame_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .seed(infos);
    Ok(())
  }

  /// Get the `BrowserContext` this page belongs to (matches Playwright's `page.context()`).
  #[must_use]
  pub fn context(&self) -> Option<&crate::context::ContextRef> {
    self.context_ref.as_ref()
  }

  /// The context-bound HTTP client backing Playwright's `page.request`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:137`:
  /// `this.request = this._browserContext.request`). `None` only for a
  /// page constructed without a context handle.
  #[must_use]
  pub fn http_client(&self) -> Option<crate::http_client::HttpClient> {
    self.context().map(crate::context::ContextRef::http_client)
  }

  /// Playwright: `page.opener(): Promise<null | Page>` —
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts`.
  /// The page that opened this popup via `window.open` /
  /// `target=_blank`; `None` for non-popup pages and once the opener
  /// has closed.
  ///
  /// Any wrapper of the popup answers, with a live wrapper of the opener,
  /// or a new one when every wrapper of it has been dropped.
  #[must_use]
  pub fn opener(&self) -> Option<Arc<Page>> {
    let slot = self.inner.page_backref_handle().opener()?;
    let opener = slot
      .upgrade()
      .or_else(|| self.context_ref.as_ref()?.page_sharing(&slot))?;
    (!opener.is_closed()).then_some(opener)
  }

  /// Record the opener relationship — called once by the popup pump
  /// right after the popup's wrapper is built.
  pub(crate) fn set_opener(&self, opener: &AnyPage) {
    self
      .inner
      .page_backref_handle()
      .set_opener(opener.page_backref_handle());
  }

  /// `page.clock` — the owning context's fake-time controller
  /// (Playwright: `page.clock` IS `context.clock`, `client/page.ts:137`).
  /// `None` for a page constructed without a context handle.
  #[must_use]
  pub fn clock(&self) -> Option<crate::clock::Clock> {
    self.context_ref.as_ref().map(super::context::ContextRef::clock)
  }

  /// Open a trace action span for a page-level API call when this
  /// page's context is being traced. Callers pass the final outcome to
  /// [`crate::trace::ActionSpan::finish`].
  pub(crate) fn trace_span(&self, method: &str, params: serde_json::Value) -> Option<crate::trace::ActionSpan> {
    // A context-less page can still be observed (`run --trace`); only the
    // zip recorder is keyed by composite.
    let composite = self.context_ref.as_ref().map(super::context::ContextRef::composite);
    crate::trace::begin_action(
      composite.as_deref(),
      "Page",
      method,
      Some(format!("page@{}", self.backend_page_id())),
      params,
    )
  }

  /// Capture the "before" DOM snapshot for a traced action (the
  /// `before` event already referenced the name — [`crate::trace`]
  /// fixes snapshot names up front like Playwright). No-ops (and costs
  /// nothing) unless this context is being traced with
  /// `snapshots: true`.
  pub(crate) async fn snapshot_before(
    &self,
    span: Option<crate::trace::ActionSpan>,
  ) -> Option<crate::trace::ActionSpan> {
    let span = span?;
    if let Some(name) = span.before_snapshot_name().map(str::to_string)
      && let Some(recorder) = self.active_trace_recorder()
    {
      crate::snapshotter::capture_page_snapshot(&recorder, self, span.call_id(), &name).await;
    }
    Some(span)
  }

  /// Open a traced `expect.<matcher>` action span (with its before
  /// snapshot). Public so the assertion crates layered on the core can
  /// mirror Playwright's expect tracing — an `expect.toBeVisible` call
  /// appears in the trace with DOM snapshots exactly like a page
  /// action. `None` when the context is not being traced.
  pub async fn begin_expect_trace(&self, matcher: &str, params: serde_json::Value) -> Option<crate::trace::ActionSpan> {
    let composite = self.context().map(crate::context::ContextRef::composite);
    let span = crate::trace::begin_action(
      composite.as_deref(),
      "Expect",
      matcher,
      Some(format!("page@{}", self.backend_page_id())),
      params,
    );
    let span = self.snapshot_before(span).await;
    crate::trace::open_action(span).await
  }

  /// Finish a traced expect span: capture the after snapshot and record
  /// the assertion outcome.
  pub async fn finish_expect_trace(&self, mut span: crate::trace::ActionSpan, error: Option<String>) {
    if span.snapshots_enabled()
      && let Some(recorder) = self.active_trace_recorder()
    {
      let name = format!("after@{}", span.call_id());
      crate::snapshotter::capture_page_snapshot(&recorder, self, span.call_id(), &name).await;
      span.set_after_snapshot(name);
    }
    span.finish_message(error);
  }

  /// Record the input-time trace state for an input-dispatching locator
  /// action: mark `element` as the action's target (the viewer's
  /// element highlight), capture the `input@<callId>` DOM snapshot,
  /// and emit the `input` event with the dispatch point (pointer
  /// actions aim at the element's border-box center — the backend's
  /// default click point). No-op for pure reads.
  pub(crate) async fn trace_capture_input(
    &self,
    span: &crate::trace::ActionSpan,
    element: &crate::backend::AnyElement,
    method: &str,
  ) {
    let Some(kind) = crate::trace::input_action_kind(method) else {
      return;
    };
    let mark_fn = format!(
      "function() {{ \
         try {{ \
           const prev = window.__ferridriver_marked_targets__ || []; \
           for (const el of prev) {{ try {{ delete el.__playwright_target__; }} catch (e) {{}} }} \
           this.__playwright_target__ = {call_id:?}; \
           window.__ferridriver_marked_targets__ = [this]; \
         }} catch (e) {{}} \
         try {{ \
           const r = this.getBoundingClientRect(); \
           let x = r.x + r.width / 2, y = r.y + r.height / 2; \
           let win = this.ownerDocument && this.ownerDocument.defaultView; \
           while (win && win !== win.parent && win.frameElement) {{ \
             const fr = win.frameElement.getBoundingClientRect(); \
             x += fr.x; y += fr.y; \
             win = win.parent; \
           }} \
           return JSON.stringify({{ x, y }}); \
         }} catch (e) {{ return JSON.stringify(null); }} \
       }}",
      call_id = span.call_id(),
    );
    let marked = element.call_js_fn_value(&mark_fn).await;
    let point = if kind == crate::trace::InputKind::Pointer {
      marked.ok().flatten().and_then(|value| {
        let parsed = match value {
          serde_json::Value::String(raw) => serde_json::from_str::<serde_json::Value>(&raw).ok()?,
          other => other,
        };
        Some((parsed.get("x")?.as_f64()?, parsed.get("y")?.as_f64()?))
      })
    } else {
      None
    };
    let input_snapshot = if span.snapshots_enabled() {
      if let Some(recorder) = self.active_trace_recorder() {
        let name = format!("input@{}", span.call_id());
        crate::snapshotter::capture_page_snapshot(&recorder, self, span.call_id(), &name).await;
        Some(name)
      } else {
        None
      }
    } else {
      None
    };
    span.mark_input(input_snapshot, point);
  }

  /// Run `op` as a traced page action: open the span (with its before
  /// snapshot), await the call, finish with the after snapshot. The
  /// viewer shows it as `page.<method>`, and the debugger's gate stops
  /// on it like any other action.
  ///
  /// Costs nothing when nobody is recording, observing or gating —
  /// [`crate::trace::begin_action`] returns `None` and the future is
  /// awaited as-is.
  pub(crate) fn traced<'a, T: 'a>(
    &'a self,
    method: &'a str,
    params: serde_json::Value,
    op: impl std::future::Future<Output = Result<T>> + 'a,
  ) -> impl std::future::Future<Output = Result<T>> + 'a {
    self.traced_as("Page", method, params, op)
  }

  /// [`Self::traced`] for the smaller surfaces hanging off a page
  /// (`Keyboard`, `Mouse`, `Touchscreen`): same span, own title.
  /// Boxed rather than `async fn`: every wrapped call would otherwise
  /// carry the span machinery inside its own state machine, and the
  /// futures of the whole page API would grow by kilobytes each.
  pub(crate) fn traced_as<'a, T: 'a>(
    &'a self,
    class: &'static str,
    method: &'a str,
    params: serde_json::Value,
    op: impl std::future::Future<Output = Result<T>> + 'a,
  ) -> impl std::future::Future<Output = Result<T>> + 'a {
    Box::pin(async move {
      let span = if crate::trace::call_origins_wanted() {
        let composite = self.context_ref.as_ref().map(super::context::ContextRef::composite);
        let span = crate::trace::begin_action(
          composite.as_deref(),
          class,
          method,
          Some(format!("page@{}", self.backend_page_id())),
          params,
        );
        crate::trace::open_action(self.snapshot_before(span).await).await
      } else {
        None
      };
      let result = crate::trace::within_action(crate::abort::guard(op)).await;
      if let Some(span) = span {
        self.snapshot_after_and_finish(span, result.as_ref().err()).await;
      }
      self.slow_mo_pause(class, method).await;
      result
    })
  }

  /// Wait out the browser's `slowMo` after an action Playwright slows down:
  /// the ones `protocolMetainfo.ts` marks `slowMo: true`, which are input,
  /// focus and navigation. Reads, evaluation and waits run at full speed.
  pub(crate) async fn slow_mo_pause(&self, class: &str, method: &str) {
    if !slows_down(class, method) {
      return;
    }
    let Some(context) = self.context_ref.as_ref() else {
      return;
    };
    let slow_mo = context.state.read().await.slow_mo;
    if let Some(ms) = slow_mo.filter(|ms| *ms > 0) {
      tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
  }

  /// Capture the "after" DOM snapshot, stamp it, and finish the span.
  pub(crate) async fn snapshot_after_and_finish(
    &self,
    mut span: crate::trace::ActionSpan,
    error: Option<&crate::error::FerriError>,
  ) {
    if span.snapshots_enabled()
      && let Some(recorder) = self.active_trace_recorder()
    {
      let name = format!("after@{}", span.call_id());
      crate::snapshotter::capture_page_snapshot(&recorder, self, span.call_id(), &name).await;
      span.set_after_snapshot(name);
    }
    span.finish(error);
  }

  fn active_trace_recorder(&self) -> Option<std::sync::Arc<crate::trace::TraceRecorder>> {
    let composite = self.context_ref.as_ref()?.composite();
    crate::trace::recorder_for(&composite)
  }

  /// `(child_frame_id, parent_frame_id)` for every live child frame —
  /// the pairs [`crate::snapshotter`] annotates with `markIframe`
  /// before capturing.
  pub(crate) fn trace_child_frame_list(&self) -> Vec<(std::sync::Arc<str>, std::sync::Arc<str>)> {
    self.with_frame_cache(|c| {
      c.live_ids()
        .filter_map(|id| c.parent_id(&id).map(|parent| (id, parent)))
        .collect()
    })
  }

  /// `(frame_id, is_main)` for every live frame, main frame first.
  pub(crate) fn trace_frame_list(&self) -> Vec<(std::sync::Arc<str>, bool)> {
    let (main_id, ids) = self.with_frame_cache(|c| {
      let ids: Vec<std::sync::Arc<str>> = c.live_ids().collect();
      (c.main_frame_id(), ids)
    });
    let mut out: Vec<(std::sync::Arc<str>, bool)> = Vec::with_capacity(ids.len().max(1));
    out.push((std::sync::Arc::clone(&main_id), true));
    out.extend(ids.into_iter().filter(|id| *id != main_id).map(|id| (id, false)));
    out
  }

  /// Access the underlying backend page (escape hatch).
  #[must_use]
  pub fn inner(&self) -> &AnyPage {
    &self.inner
  }

  /// Set the default timeout for all operations (milliseconds).
  pub fn set_default_timeout(&self, ms: u64) {
    self.default_timeout.store(ms, Ordering::Relaxed);
  }

  /// Get the default timeout (milliseconds).
  #[must_use]
  pub fn default_timeout(&self) -> u64 {
    match self.default_timeout.load(Ordering::Relaxed) {
      UNSET_TIMEOUT => self
        .context_ref
        .as_ref()
        .map(crate::context::ContextRef::default_timeout)
        .filter(|ms| *ms != 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS),
      v => v,
    }
  }

  /// Set the default timeout for navigation-family operations
  /// (`goto`, `reload`, `go_back`, `go_forward`, `wait_for_url`). Mirrors
  /// Playwright's `page.setDefaultNavigationTimeout(timeout)`. Overrides
  /// the non-navigation default returned by [`Self::default_timeout`] for
  /// navigation calls only. Passing `0` means "no timeout" (Playwright
  /// parity).
  pub fn set_default_navigation_timeout(&self, ms: u64) {
    self.default_navigation_timeout.store(ms, Ordering::Relaxed);
  }

  /// Current effective navigation timeout (milliseconds). If
  /// [`Self::set_default_navigation_timeout`] was not called, returns the
  /// same value as [`Self::default_timeout`].
  #[must_use]
  pub fn default_navigation_timeout(&self) -> u64 {
    let page_nav = self.default_navigation_timeout.load(Ordering::Relaxed);
    if page_nav != UNSET_TIMEOUT {
      return page_nav;
    }
    let page_default = self.default_timeout.load(Ordering::Relaxed);
    if page_default != UNSET_TIMEOUT {
      return page_default;
    }
    let context = self.context_ref.as_ref();
    context
      .map(crate::context::ContextRef::default_navigation_timeout)
      .filter(|ms| *ms != 0)
      .or_else(|| {
        context
          .map(crate::context::ContextRef::default_timeout)
          .filter(|ms| *ms != 0)
      })
      .unwrap_or(DEFAULT_TIMEOUT_MS)
  }

  /// Playwright: `page.viewportSize(): null | { width: number, height:
  /// number }` — a synchronous accessor over the client-tracked emulated
  /// viewport (types.d.ts; client keeps `_viewportSize`, never queries
  /// the browser). `None` when no viewport emulation is applied.
  #[must_use]
  pub fn viewport_size(&self) -> Option<(i64, i64)> {
    *self.viewport.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Seed the tracked viewport with the emulation the page was created
  /// under (see [`crate::context::ContextRef::new_page`]'s
  /// `effective_viewport`).
  pub(crate) fn set_cached_viewport(&self, viewport: Option<(i64, i64)>) {
    *self.viewport.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = viewport;
  }

  /// Whether selector-taking methods on this page resolve strictly.
  /// Locator APIs ignore this and are always strict.
  #[must_use]
  pub fn strict_selectors(&self) -> bool {
    self.strict_selectors.load(std::sync::atomic::Ordering::Relaxed)
  }

  pub(crate) fn set_strict_selectors(&self, strict: bool) {
    self
      .strict_selectors
      .store(strict, std::sync::atomic::Ordering::Relaxed);
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────

  /// Close this page. After closing, most operations will fail.
  ///
  /// Accepts `Option<`[`crate::options::PageCloseOptions`]`>` — mirrors
  /// Playwright's `page.close({ runBeforeUnload, reason } = {})`.
  /// `runBeforeUnload=true` fires the page's `beforeunload` handlers
  /// before unloading. `reason`, if set, is stored on the `Page` and
  /// surfaces through any `TargetClosed` error returned to in-flight
  /// operations on this page. Pass `None` for the common no-options case.
  ///
  /// # Errors
  ///
  /// Returns an error if the page cannot be closed.
  #[track_caller]
  pub fn close(&self) -> crate::action::Action<'_, crate::options::PageCloseOptions, ()> {
    let page = self;
    crate::action::Action::new(move |opts| Box::pin(async move { page.close_impl(Some(opts)).await }))
  }

  /// Implementation of [`Self::close`].
  #[tracing::instrument(skip(self, opts))]
  pub(crate) async fn close_impl(&self, opts: Option<crate::options::PageCloseOptions>) -> Result<()> {
    // Closing a closed page does nothing, as in Playwright; the browser
    // has nothing left to close and some backends answer with an error.
    if self.is_closed() {
      return Ok(());
    }
    let opts = opts.unwrap_or_default();
    if let Some(reason) = opts.reason.clone() {
      // Poisoned mutex is recoverable here — the stored reason is just
      // metadata for downstream `TargetClosed` errors, not a correctness-
      // critical invariant.
      if let Ok(mut guard) = self.close_reason.lock() {
        *guard = Some(reason);
      }
    }
    self.inner.close_page(opts).await?;
    // Playwright emits 'close' on the page once it is closed.
    self.inner.events().emit(PageEvent::Close);

    // Remove closed page from context's page list so context.pages() stays accurate.
    if let Some(ctx) = &self.context_ref {
      let close_context = {
        let mut state = ctx.state.write().await;
        let context_pages = state.context_pages.clone();
        match state.context_mut_checked(&ctx.name) {
          Ok(browser_ctx) => {
            browser_ctx.prune_closed_pages();
            context_pages.set(&ctx.key.to_composite(), &browser_ctx.pages);
            browser_ctx.owned_by_page && browser_ctx.pages.is_empty()
          },
          Err(_) => false,
        }
      };
      // A page `browser.newPage()` opened takes its context with it.
      if close_context {
        ctx.close().await?;
      }
    }

    Ok(())
  }

  /// Reason passed to the most recent [`Page::close`] call, if any. Used by
  /// error-surfacing code to attach a human-readable explanation to
  /// `TargetClosed` errors emitted after close.
  #[must_use]
  pub fn close_reason(&self) -> Option<String> {
    self.close_reason.lock().ok().and_then(|g| g.clone())
  }

  /// Check if this page has been closed.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  /// Video handle for this page when recording is enabled on the
  /// owning context. Playwright:
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:4756`
  /// — `video(): null | Video`. Returns `None` for pages in contexts
  /// that were not created with `recordVideo`. Recording is driven by
  /// `start_screencast`, which every backend (CDP, `BiDi`, Playwright
  /// `WebKit`) implements, so any context created with `recordVideo`
  /// yields a handle.
  #[must_use]
  pub fn video(&self) -> Option<Arc<crate::video::Video>> {
    self.video.lock().ok().and_then(|g| g.clone())
  }

  /// Attach a [`crate::video::Video`] handle. Called by
  /// [`crate::state::BrowserState::register_opened_page`] when a page
  /// is opened in a `record_video`-enabled context. Silent no-op on
  /// mutex poisoning (non-correctness-critical; the handle simply
  /// won't be exposed).
  pub(crate) fn attach_video(&self, video: Arc<crate::video::Video>) {
    if let Ok(mut guard) = self.video.lock() {
      *guard = Some(video);
    }
  }

  // ── Input device accessors ────────────────────────────────────────────

  /// Get the Keyboard interface for this page.
  #[must_use]
  pub fn keyboard(&self) -> Keyboard<'_> {
    Keyboard { page: self }
  }

  /// Get the Mouse interface for this page.
  #[must_use]
  pub fn mouse(&self) -> Mouse<'_> {
    Mouse { page: self }
  }

  /// Get the Touchscreen interface for this page.
  #[must_use]
  pub fn touchscreen(&self) -> Touchscreen<'_> {
    Touchscreen { page: self }
  }

  // ── Screencast (video recording) ──

  /// Start streaming screencast frames. Returns a channel of
  /// `(jpeg_bytes, timestamp_secs)` pairs; the timestamp is the
  /// backend's own (Chrome's `metadata.timestamp` on CDP). The stream is
  /// shared with any trace or video recording on this page, and a second
  /// call replaces the first.
  ///
  /// # Errors
  ///
  /// Returns an error if screencast cannot be started on the backend.
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>> {
    let params = crate::backend::screencast::ScreencastParams::new(quality, max_width, max_height);
    let mut frames = self.inner.screencast_hub()?.start_user(&self.inner, params).await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
      while let Some(frame) = frames.recv().await {
        if tx.send((frame.data, frame.timestamp)).is_err() {
          break;
        }
      }
    });
    Ok(rx)
  }

  /// Stop the screencast started by [`Self::start_screencast`].
  ///
  /// # Errors
  ///
  /// Returns an error if the backend has no screencast.
  pub async fn stop_screencast(&self) -> Result<()> {
    self.inner.screencast_hub()?.stop_user(&self.inner).await;
    Ok(())
  }

  /// A screencast stream of the caller's own, for a viewer that relays
  /// frames somewhere else.
  ///
  /// Unlike [`Self::start_screencast`], which is the page's one
  /// script-facing stream and is replaced by the next call, every stream
  /// returned here is independent: it shares the page's backend screencast
  /// with traces, videos and other viewers, and ends when it is stopped or
  /// dropped.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend cannot screencast.
  pub async fn screencast_frames(&self, quality: u8, max_width: u32, max_height: u32) -> Result<ScreencastFrames> {
    let params = crate::backend::screencast::ScreencastParams::new(quality, max_width, max_height);
    let (subscription, frames) = self.inner.subscribe_screencast(params).await?;
    Ok(ScreencastFrames {
      subscription: Some(subscription),
      frames,
    })
  }
}

/// A stream from [`Page::screencast_frames`].
pub struct ScreencastFrames {
  subscription: Option<crate::backend::screencast::ScreencastSubscription>,
  frames: tokio::sync::mpsc::UnboundedReceiver<crate::backend::screencast::ScreencastFrame>,
}

impl ScreencastFrames {
  /// The next frame as `(jpeg_bytes, timestamp_secs)`, the same shape
  /// [`Page::start_screencast`] yields. `None` once the stream ended,
  /// including when the page closed.
  pub async fn next(&mut self) -> Option<(Vec<u8>, f64)> {
    self.frames.recv().await.map(|frame| (frame.data, frame.timestamp))
  }

  /// Leave the screencast, stopping the backend stream if nothing else
  /// reads it.
  pub async fn stop(mut self) {
    if let Some(subscription) = self.subscription.take() {
      subscription.stop().await;
    }
  }
}

impl std::fmt::Debug for ScreencastFrames {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ScreencastFrames").finish_non_exhaustive()
  }
}

impl std::fmt::Debug for Page {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Page").finish()
  }
}

/// JS global backing a [`crate::options::WebStorageKind`]:
/// `localStorage` / `sessionStorage`.
fn web_storage_global(kind: crate::options::WebStorageKind) -> &'static str {
  match kind {
    crate::options::WebStorageKind::Local => "localStorage",
    crate::options::WebStorageKind::Session => "sessionStorage",
  }
}

/// Encode a Rust string as a JS string literal for safe interpolation
/// into an evaluated expression — equivalent to `JSON.stringify(s)`.
fn web_storage_js_string(s: &str) -> String {
  serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Lighthouse's `UrlUtils.equalWithExcludedFragments`: a request URL
/// never carries a fragment and `document.location.href` may, so the
/// two are compared with both dropped.
fn same_document_url(request_url: &str, document_url: &str) -> bool {
  let strip = |url: &str| {
    reqwest::Url::parse(url).ok().map(|mut url| {
      url.set_fragment(None);
      url
    })
  };
  match (strip(request_url), strip(document_url)) {
    (Some(a), Some(b)) => a == b,
    _ => false,
  }
}

pub(crate) fn page_state_observer(
  kind: crate::backend::BackendKind,
  cache: Arc<Mutex<FrameCache>>,
  observed: Arc<Mutex<crate::observed::ObservedBuffers>>,
  network_idle: Option<Arc<crate::network_idle::NetworkIdle>>,
) -> impl Fn(&PageEvent) + Send + Sync {
  let wire_tracks_frames = matches!(
    kind,
    crate::backend::BackendKind::CdpPipe
      | crate::backend::BackendKind::CdpWs
      | crate::backend::BackendKind::WebKit
      | crate::backend::BackendKind::Bidi
  );
  move |event| {
    if let Some(network_idle) = &network_idle {
      network_idle.observe(event);
    }
    match event {
      PageEvent::FrameAttached(info) => {
        if !wire_tracks_frames && let Ok(mut g) = cache.lock() {
          g.attach(info.clone());
        }
      },
      PageEvent::FrameDetached { frame_id } => {
        if !wire_tracks_frames && let Ok(mut g) = cache.lock() {
          g.detach(frame_id);
        }
      },
      PageEvent::FrameNavigated(info) => {
        // A main-frame navigation starts a new `since-navigation`
        // window for `consoleMessages()` / `pageErrors()`.
        if info.parent_frame_id.is_none()
          && let Ok(mut o) = observed.lock()
        {
          o.mark_navigation();
        }
        if !wire_tracks_frames && let Ok(mut g) = cache.lock() {
          g.navigated(info.clone());
        }
      },
      PageEvent::FrameNavigatedWithinDocument(info) => {
        // Same document, new URL: no `since-navigation` reset, no
        // subtree detach — just keep the tracked URL fresh so
        // `page.url()` / `waitForURL` observe SPA route changes.
        if !wire_tracks_frames && let Ok(mut g) = cache.lock() {
          g.navigated_within(&info.frame_id, &info.url);
        }
      },
      PageEvent::Console(msg) => {
        if let Ok(mut o) = observed.lock() {
          o.push_console(msg.clone());
        }
      },
      PageEvent::PageError(err) => {
        if let Ok(mut o) = observed.lock() {
          o.push_error(err.clone());
        }
      },
      PageEvent::Worker(worker) => {
        if let Ok(mut o) = observed.lock() {
          o.push_worker(worker.clone());
        }
      },
      PageEvent::Request(request) => {
        if let Ok(mut o) = observed.lock() {
          o.push_request(request.clone());
        }
      },
      _ => {},
    }
  }
}

/// Drop guard that runs a cleanup callback unless disarmed.
struct Unregister(Option<Box<dyn FnOnce() + Send>>);

impl Unregister {
  fn new(remove: impl FnOnce() + Send + 'static) -> Self {
    Self(Some(Box::new(remove)))
  }
}

impl Drop for Unregister {
  fn drop(&mut self) {
    if let Some(remove) = self.0.take() {
      remove();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::input_devices::{TypeToken, parse_named_keys};

  #[test]
  fn parse_named_keys_disabled_yields_only_chars() {
    assert_eq!(
      parse_named_keys("a{Enter}b", false),
      vec![
        TypeToken::Char('a'),
        TypeToken::Char('{'),
        TypeToken::Char('E'),
        TypeToken::Char('n'),
        TypeToken::Char('t'),
        TypeToken::Char('e'),
        TypeToken::Char('r'),
        TypeToken::Char('}'),
        TypeToken::Char('b'),
      ]
    );
  }

  #[test]
  fn parse_named_keys_extracts_single_key() {
    assert_eq!(
      parse_named_keys("Hello{Enter}World", true),
      vec![
        TypeToken::Char('H'),
        TypeToken::Char('e'),
        TypeToken::Char('l'),
        TypeToken::Char('l'),
        TypeToken::Char('o'),
        TypeToken::Key("Enter".to_string()),
        TypeToken::Char('W'),
        TypeToken::Char('o'),
        TypeToken::Char('r'),
        TypeToken::Char('l'),
        TypeToken::Char('d'),
      ]
    );
  }

  #[test]
  fn parse_named_keys_extracts_modifier_combo() {
    assert_eq!(
      parse_named_keys("{Control+a}x", true),
      vec![TypeToken::Key("Control+a".to_string()), TypeToken::Char('x')]
    );
  }

  #[test]
  fn parse_named_keys_double_brace_is_literal() {
    assert_eq!(
      parse_named_keys("a{{b", true),
      vec![TypeToken::Char('a'), TypeToken::Char('{'), TypeToken::Char('b')]
    );
  }

  #[test]
  fn parse_named_keys_double_brace_then_key() {
    // `{{` -> literal `{`, then `Enter}` is a plain char run (no opening brace).
    assert_eq!(
      parse_named_keys("{{Enter}", true),
      vec![
        TypeToken::Char('{'),
        TypeToken::Char('E'),
        TypeToken::Char('n'),
        TypeToken::Char('t'),
        TypeToken::Char('e'),
        TypeToken::Char('r'),
        TypeToken::Char('}'),
      ]
    );
  }

  #[test]
  fn parse_named_keys_unterminated_brace_is_literal() {
    assert_eq!(
      parse_named_keys("a{bc", true),
      vec![
        TypeToken::Char('a'),
        TypeToken::Char('{'),
        TypeToken::Char('b'),
        TypeToken::Char('c'),
      ]
    );
  }

  #[test]
  fn parse_named_keys_adjacent_keys() {
    assert_eq!(
      parse_named_keys("{Tab}{Enter}", true),
      vec![TypeToken::Key("Tab".to_string()), TypeToken::Key("Enter".to_string())]
    );
  }
}

/// Whether Playwright waits out `slowMo` after `class.method`: the methods
/// `protocolMetainfo.ts` flags `slowMo: true`, under the names this crate
/// traces them by (a locator action runs the frame method of the same name).
fn slows_down(class: &str, method: &str) -> bool {
  const FRAME: &[&str] = &[
    "blur",
    "check",
    "click",
    "dragAndDrop",
    "drop",
    "dblclick",
    "dispatchEvent",
    "fill",
    "focus",
    "goto",
    "hover",
    "press",
    "pressSequentially",
    "selectOption",
    "setInputFiles",
    "tap",
    "type",
    "uncheck",
    "setChecked",
  ];
  const ELEMENT: &[&str] = &[
    "check",
    "click",
    "dblclick",
    "dispatchEvent",
    "fill",
    "focus",
    "hover",
    "press",
    "scrollIntoViewIfNeeded",
    "selectOption",
    "selectText",
    "setInputFiles",
    "tap",
    "type",
    "uncheck",
    "setChecked",
  ];
  match class {
    "Frame" => FRAME.contains(&method),
    "Locator" => FRAME.contains(&method) || ELEMENT.contains(&method),
    "ElementHandle" => ELEMENT.contains(&method),
    "Page" => matches!(method, "goto" | "goBack" | "goForward" | "reload") || FRAME.contains(&method),
    "Keyboard" => matches!(method, "down" | "up" | "insertText" | "type" | "press"),
    "Mouse" => matches!(method, "move" | "down" | "up" | "click" | "dblclick" | "wheel"),
    "Touchscreen" => method == "tap",
    _ => false,
  }
}

#[cfg(test)]
mod slow_mo_tests {
  use super::slows_down;

  #[test]
  fn input_and_navigation_slow_down_reads_do_not() {
    assert!(slows_down("Locator", "click"));
    assert!(slows_down("Locator", "fill"));
    assert!(slows_down("Page", "goto"));
    assert!(slows_down("Keyboard", "press"));
    assert!(slows_down("Mouse", "wheel"));
    assert!(!slows_down("Locator", "textContent"));
    assert!(!slows_down("Frame", "evaluate"));
    assert!(!slows_down("Page", "screenshot"));
  }
}
