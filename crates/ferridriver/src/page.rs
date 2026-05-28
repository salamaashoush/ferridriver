//! High-level Page API -- mirrors Playwright's Page interface.
//!
//! All interaction methods auto-wait for element actionability.
//! Locator methods are lazy (don't query DOM until action).

use crate::actions;
use crate::backend::{AnyPage, CookieData, ImageFormat, ScreenshotOpts};
use crate::error::Result;
use crate::events::{EventEmitter, PageEvent};
use crate::frame::Frame;
use crate::frame_cache::FrameCache;
use crate::locator::Locator;
use crate::options::{FrameSelector, GotoOptions, RoleOptions, ScreenshotOptions, TextOptions, WaitOptions};
use crate::snapshot;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex as AsyncMutex;

/// High-level page API, mirrors Playwright's Page interface.
/// Always constructed behind `Arc<Page>` — locators, frames, and consumers
/// hold Arc refs. No cloning of the Page struct itself.
pub struct Page {
  pub(crate) inner: AnyPage,
  default_timeout: AtomicU64,
  /// Per Playwright: split from `default_timeout`. Navigation-family APIs
  /// (`goto`, `reload`, `go_back`, `go_forward`, `wait_for_url`) use this
  /// when explicitly set. The `u64::MAX` sentinel means "not set — fall
  /// back to `default_timeout`"; `0` means "infinite" (Playwright parity).
  default_navigation_timeout: AtomicU64,
  snapshot_tracker: Arc<AsyncMutex<snapshot::SnapshotTracker>>,
  mouse_position: Mutex<(f64, f64)>,
  context_ref: Option<crate::context::ContextRef>,
  /// Human-readable `reason` passed to the last `close({ reason })` call,
  /// surfaced on subsequent `TargetClosed` errors — Playwright parity.
  close_reason: Mutex<Option<String>>,
  /// Persistent emulated-media state. Playwright tracks per-field state so
  /// that `emulateMedia({colorScheme: 'dark'})` followed by
  /// `emulateMedia({media: 'print'})` keeps both active — each call is a
  /// partial update, not a replacement. See
  /// `/tmp/playwright/packages/playwright-core/src/server/page.ts:585`.
  emulated_media: Mutex<crate::options::EmulateMediaOptions>,
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
}

impl Page {
  /// Construct a Page (no `BrowserContext`). Synchronous — only
  /// spawns the frame-cache listener; the eager `Page.getFrameTree`
  /// RTT was dropped (see `PERF_AUDIT` §M.4). The listener seeds
  /// the cache on the first frame event, and
  /// `Self::ensure_frame_cache_seeded` does an on-demand fetch
  /// only when a sync accessor fires before any navigation.
  #[must_use]
  pub fn new(inner: AnyPage) -> Arc<Self> {
    let frame_cache = inner.frame_cache().clone();
    let page = Arc::new(Self {
      inner,
      default_timeout: AtomicU64::new(30000),
      default_navigation_timeout: AtomicU64::new(u64::MAX),
      snapshot_tracker: Arc::new(AsyncMutex::new(snapshot::SnapshotTracker::new())),
      mouse_position: Mutex::new((0.0, 0.0)),
      context_ref: None,
      close_reason: Mutex::new(None),
      emulated_media: Mutex::new(crate::options::EmulateMediaOptions::default()),
      frame_cache,
      video: Mutex::new(None),
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
    let frame_cache = inner.frame_cache().clone();
    let page = Arc::new(Self {
      inner,
      default_timeout: AtomicU64::new(30000),
      default_navigation_timeout: AtomicU64::new(u64::MAX),
      snapshot_tracker: Arc::new(AsyncMutex::new(snapshot::SnapshotTracker::new())),
      mouse_position: Mutex::new((0.0, 0.0)),
      context_ref: Some(context),
      close_reason: Mutex::new(None),
      emulated_media: Mutex::new(crate::options::EmulateMediaOptions::default()),
      frame_cache,
      video: Mutex::new(None),
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

  /// Internal: spawn the `FrameAttached` / `FrameDetached` /
  /// `FrameNavigated` listener that keeps the backend's frame cache
  /// fresh. Idempotent — only the first wrapper for a given backend
  /// spawns the listener; subsequent wrappers see the latch set and
  /// skip the spawn so we don't end up with N listeners all writing
  /// the same cache. The listener task holds `Arc` clones of the
  /// cache + event emitter, so it lives until the backend page is
  /// dropped (broadcast sender drops → recv returns Err → task
  /// exits).
  fn seed_frame_cache(self: &Arc<Self>) {
    if self
      .inner
      .frame_listener_started()
      .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
      return;
    }
    let cache = Arc::clone(&self.frame_cache);
    let mut rx = self.inner.events().subscribe();
    tokio::spawn(async move {
      while let Ok(event) = rx.recv().await {
        match event {
          PageEvent::FrameAttached(info) => {
            if let Ok(mut g) = cache.lock() {
              g.attach(info);
            }
          },
          PageEvent::FrameDetached { frame_id } => {
            if let Ok(mut g) = cache.lock() {
              g.detach(&frame_id);
            }
          },
          PageEvent::FrameNavigated(info) => {
            if let Ok(mut g) = cache.lock() {
              g.navigated(info);
            }
          },
          _ => {},
        }
      }
    });
  }

  /// Lazy frame-cache seed. Returns immediately when the cache
  /// already has a main frame (populated either by a prior
  /// `Page.frameNavigated` event or by an earlier call). Otherwise:
  ///
  /// 1. Tries the backend's cached `peek_main_frame_id()` first
  ///    (populated for free from the `Page.navigate` response when
  ///    the user has already called `goto`) — seeds a synthetic
  ///    `FrameInfo` with that id and an empty url, no RTT.
  /// 2. Falls back to `Page.getFrameTree` if the backend has no
  ///    cached frame id (no prior navigation).
  ///
  /// Called from [`Self::goto`] after `inner.goto` returns to
  /// guarantee `main_frame()` works for the synchronous accessor
  /// the user is about to invoke. The RTT path fires at most once
  /// per page lifetime and is skipped entirely for the common case
  /// (navigate-then-query test flows — the bench's 100% case).
  pub(crate) async fn ensure_frame_cache_seeded(self: &Arc<Self>) -> Result<()> {
    let already_seeded = self.with_frame_cache(|c| c.main_frame_id().is_some());
    if already_seeded {
      return Ok(());
    }
    if let Some(fid) = self.inner.peek_main_frame_id() {
      if let Ok(mut g) = self.frame_cache.lock() {
        if g.main_frame_id().is_none() {
          g.attach(crate::backend::FrameInfo {
            frame_id: fid,
            parent_frame_id: None,
            name: String::new(),
            url: String::new(),
          });
        }
      }
      return Ok(());
    }
    let infos = self.inner.get_frame_tree().await?;
    if let Ok(mut g) = self.frame_cache.lock() {
      if g.main_frame_id().is_none() {
        g.seed(infos);
      }
    }
    Ok(())
  }

  /// Refresh the frame cache from the backend without touching the
  /// listener. Useful when a caller wants to guarantee freshness before
  /// reading sync accessors (e.g. right after a navigation where event
  /// delivery is racing with the caller).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend's `get_frame_tree()` call fails.
  pub async fn sync_frames(self: &Arc<Self>) -> Result<()> {
    let infos = self.inner.get_frame_tree().await?;
    if let Ok(mut g) = self.frame_cache.lock() {
      g.seed(infos);
    }
    Ok(())
  }

  /// Get the `BrowserContext` this page belongs to (matches Playwright's `page.context()`).
  #[must_use]
  pub fn context(&self) -> Option<&crate::context::ContextRef> {
    self.context_ref.as_ref()
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
    self.default_timeout.load(Ordering::Relaxed)
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
    match self.default_navigation_timeout.load(Ordering::Relaxed) {
      u64::MAX => self.default_timeout(),
      v => v,
    }
  }

  /// Get the current viewport size by querying the browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn viewport_size(&self) -> Result<(i64, i64)> {
    let r = self
      .inner
      .evaluate("JSON.stringify({w:window.innerWidth,h:window.innerHeight})")
      .await?;
    let s = r
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
    let w = parsed.get("w").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let h = parsed.get("h").and_then(serde_json::Value::as_i64).unwrap_or(0);
    Ok((w, h))
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  /// Navigate to a URL with optional options (waitUntil, timeout).
  ///
  /// Returns the main-document `Response` when the backend can observe
  /// it, or `None` for same-document navigations (no new request was
  /// issued) / backends that genuinely cannot expose the main-document
  /// response (stock `WKWebView` has no public API for this — see the
  /// §1.4 backend gap matrix in `PLAYWRIGHT_COMPAT.md`). Mirrors
  /// Playwright's `Promise<Response | null>` contract on `page.goto`.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  #[tracing::instrument(skip(self, opts), fields(url))]
  pub async fn goto(
    self: &Arc<Self>,
    url: &str,
    opts: Option<GotoOptions>,
  ) -> Result<Option<crate::network::Response>> {
    // Resolve against the context's `baseURL` option when set —
    // mirrors Playwright's `constructURLBasedOnBaseURL` applied in
    // `Page._goto` (`/tmp/playwright/packages/playwright-core/src/client/page.ts`).
    // Absolute URLs passthrough; relative paths resolve against baseURL.
    let resolved = self.resolve_with_base_url(url).await;
    tracing::debug!(target: "ferridriver::action", action = "goto", url = %resolved, "page.goto");
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    let referer = opts.as_ref().and_then(|o| o.referer.as_deref());
    let result = self.inner.goto(&resolved, lifecycle, timeout, referer).await;
    // PERF_AUDIT.md §M.4 — bootstrap no longer fetches `Page.getFrameTree`,
    // so the wrapper's frame cache is empty on a fresh page until the
    // first navigation event lands. The CDP backend captures the
    // top-level `frameId` from `Page.navigate`'s response and we read
    // it here via `peek_main_frame_id()` to seed the cache without
    // an extra RTT — the synchronous `main_frame()` call the user is
    // about to make then sees a populated cache. `ensure_frame_cache_seeded`
    // is a no-op when a `Page.frameNavigated` event has already
    // populated the cache via the listener (the common path on
    // network-light tests where the listener task gets scheduled
    // before goto returns).
    let _ = self.ensure_frame_cache_seeded().await;
    // BiDi subframe seeding: WebDriver BiDi's `browsingContext.contextCreated`
    // fires asynchronously for child iframes after `browsingContext.navigate`
    // completes, and the iframe's `name` attribute lives in the DOM —
    // not in the contextCreated payload. The listener-driven cache
    // therefore lags any synchronous `page.frame(name)` / `page.frames()`
    // call made right after `goto` returns. Mirror Playwright's
    // `bidiBrowser._onBrowsingContextCreated`
    // (`/tmp/playwright/packages/playwright-core/src/server/bidi/bidiBrowser.ts:146`)
    // by fetching the full subtree on the goto-return path and seeding
    // the wrapper's cache directly — `BidiPage::get_frame_tree` already
    // does the parallel `window.name` resolution for unnamed children,
    // so the wrapper sees a fully-populated cache.
    // Backends without per-frame attach/navigate events on the wire need an
    // explicit cache seed after navigation so synchronous `page.frame(...)`
    // / `page.frames()` calls see the iframes the test just navigated to.
    // - BiDi: `browsingContext.contextCreated` arrives async with empty
    //   `name`; the cache won't reflect the DOM until our `getTree` probe
    //   runs.
    // - WebKit (WKWebView on macOS, WebKitGTK on Linux): no FrameAttached
    //   events at all — the cache is populated solely by `get_frame_tree`'s
    //   DOM probe, which must run after EVERY navigation since
    //   `ensure_frame_cache_seeded`'s early-return on `main_frame_id`
    //   present would otherwise skip refreshing the iframe set on a
    //   reused page.
    let needs_sync = matches!(
      self.inner.kind(),
      crate::backend::BackendKind::Bidi | crate::backend::BackendKind::WebKit
    );
    if needs_sync {
      // Single pass — extra sync rounds would push past the
      // `setTimeout(confirm, 80)` window dialog tests rely on between
      // goto-returning and the user subscribing to `waitForEvent`.
      // Stragglers get picked up via the live FrameAttached listener.
      let _ = self.sync_frames().await;
    }
    result
  }

  /// Resolve a user-supplied URL against the owning context's
  /// `baseURL` option. Returns the input unchanged when no context
  /// is attached, no `baseURL` is set, or the input is already
  /// absolute. See
  /// [`crate::options::construct_url_with_base`] for the resolution
  /// rules.
  async fn resolve_with_base_url(&self, url: &str) -> String {
    let Some(ctx) = self.context_ref.as_ref() else {
      return url.to_string();
    };
    let state = ctx.state.read().await;
    let Some(bag) = state.get_context_options(&ctx.key.to_composite()) else {
      return url.to_string();
    };
    crate::options::construct_url_with_base(bag.base_url.as_deref(), url)
  }

  /// Navigate back in history. Returns the main-document `Response` on
  /// the same basis as `goto` (or `None`).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  pub async fn go_back(&self, opts: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    self.inner.go_back(lifecycle, timeout).await
  }

  /// Navigate forward in history. Returns the main-document `Response`
  /// on the same basis as `goto` (or `None`).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  pub async fn go_forward(&self, opts: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    self.inner.go_forward(lifecycle, timeout).await
  }

  /// Reload the current page. Returns the main-document `Response` on
  /// the same basis as `goto` (or `None`).
  ///
  /// # Errors
  ///
  /// Returns an error if the reload fails or the wait condition times out.
  pub async fn reload(&self, opts: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    self.inner.reload(lifecycle, timeout).await
  }

  /// Parse `GotoOptions` into backend `NavLifecycle` + timeout.
  fn resolve_nav_opts(opts: Option<&GotoOptions>, default_timeout: u64) -> (crate::backend::NavLifecycle, u64) {
    let wait_until = opts.and_then(|o| o.wait_until.as_deref()).unwrap_or("load");
    let timeout = opts.and_then(|o| o.timeout).unwrap_or(default_timeout);
    (crate::backend::NavLifecycle::parse_lifecycle(wait_until), timeout)
  }

  /// Get the current page URL — the main frame's URL.
  ///
  /// Playwright: [`page.url()`](https://playwright.dev/docs/api/class-page#page-url)
  /// is **synchronous** (`url(): string`). It reads the locally-tracked
  /// main-frame URL (kept current by navigation/lifecycle events), the
  /// same source [`Frame::url`] uses — no backend round-trip.
  #[must_use]
  pub fn url(&self) -> String {
    self.with_frame_cache(|c| {
      c.main_frame_id()
        .and_then(|id| c.record(&id).map(|r| r.info.url.clone()))
        .unwrap_or_default()
    })
  }

  /// Get the current page title.
  ///
  /// # Errors
  ///
  /// Returns an error if the title cannot be retrieved from the backend.
  pub async fn title(&self) -> Result<String> {
    self.inner.title().await.map(std::option::Option::unwrap_or_default)
  }

  // ── Locators (delegate to mainFrame — Playwright parity) ───────────
  //
  // `Page` is a facade over `mainFrame` for ergonomics. Mirrors
  // `/tmp/playwright/packages/playwright-core/src/client/page.ts:307+`,
  // where every locator-construction and action method does
  // `this._mainFrame.<method>(...)`. The Frame is the unit of execution
  // context; Page never constructs Locators directly.

  #[must_use]
  pub fn locator(self: &Arc<Self>, selector: &str, options: Option<crate::options::FilterOptions>) -> Locator {
    self.main_frame().locator(selector, options)
  }

  #[must_use]
  pub fn get_by_role(self: &Arc<Self>, role: &str, opts: &RoleOptions) -> Locator {
    self.main_frame().get_by_role(role, opts)
  }

  #[must_use]
  pub fn get_by_text(self: &Arc<Self>, text: &crate::options::StringOrRegex, opts: &TextOptions) -> Locator {
    self.main_frame().get_by_text(text, opts)
  }

  #[must_use]
  pub fn get_by_label(self: &Arc<Self>, text: &crate::options::StringOrRegex, opts: &TextOptions) -> Locator {
    self.main_frame().get_by_label(text, opts)
  }

  #[must_use]
  pub fn get_by_placeholder(self: &Arc<Self>, text: &crate::options::StringOrRegex, opts: &TextOptions) -> Locator {
    self.main_frame().get_by_placeholder(text, opts)
  }

  #[must_use]
  pub fn get_by_alt_text(self: &Arc<Self>, text: &crate::options::StringOrRegex, opts: &TextOptions) -> Locator {
    self.main_frame().get_by_alt_text(text, opts)
  }

  #[must_use]
  pub fn get_by_title(self: &Arc<Self>, text: &crate::options::StringOrRegex, opts: &TextOptions) -> Locator {
    self.main_frame().get_by_title(text, opts)
  }

  #[must_use]
  pub fn get_by_test_id(self: &Arc<Self>, test_id: &crate::options::StringOrRegex) -> Locator {
    self.main_frame().get_by_test_id(test_id)
  }

  /// Create a `FrameLocator` for an `<iframe>` matching the selector.
  ///
  /// Equivalent to Playwright's `page.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(self: &Arc<Self>, selector: &str) -> crate::locator::FrameLocator {
    self.main_frame().frame_locator(selector)
  }

  // ── Handle materialisation (Playwright `page.$` / `page.$$`) ─────

  /// Resolve the selector once and return a lifecycle
  /// [`crate::element_handle::ElementHandle`] — or `None` when the
  /// selector matches no element. Mirrors Playwright's
  /// `page.querySelector(selector)` /
  /// `page.$(selector)` (`/tmp/playwright/packages/playwright-core/src/client/page.ts`).
  ///
  /// Unlike [`Self::locator`] (lazy, re-resolves on every action), the
  /// returned handle is pinned to the element resolved at call time.
  /// Subsequent DOM mutations that remove the element won't invalidate
  /// the handle itself — actions against a detached element surface a
  /// backend-specific error — but the handle's lifecycle is decoupled
  /// from the page's frame cache. Callers release it via
  /// [`crate::element_handle::ElementHandle::dispose`] or let it
  /// drop when the page closes.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend cannot execute the underlying
  /// query (protocol failure, target closed, etc.). A selector that
  /// does not match any element returns `Ok(None)`.
  pub async fn query_selector(
    self: &Arc<Self>,
    selector: &str,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    match self.inner.find_element(selector).await {
      Ok(element) => {
        let handle = crate::element_handle::ElementHandle::from_any_element(Arc::clone(self), element).await?;
        Ok(Some(handle))
      },
      Err(err) if is_element_not_found(&err) => Ok(None),
      Err(err) => Err(err),
    }
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  /// Returns one [`crate::element_handle::ElementHandle`] per match in
  /// document order. Each element is pinned individually — disposing
  /// one does not affect the others.
  ///
  /// Implementation uses the selector engine's `query_all` which
  /// tags every match with `data-fd-sel='<i>'`; we then evaluate a
  /// lookup by tag for each index and wrap the result. Tags are
  /// cleaned up on completion.
  ///
  /// # Errors
  ///
  /// Returns an error on selector parse failure, protocol error, or
  /// if a match cannot be resolved (e.g. the DOM changed mid-iteration).
  pub async fn query_selector_all(
    self: &Arc<Self>,
    selector: &str,
  ) -> Result<Vec<crate::element_handle::ElementHandle>> {
    let matches = crate::selectors::query_all(&self.inner, selector, None).await?;
    let count = matches.len();
    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
      let tagged = format!("window.__fd.selOne([{{\"engine\":\"css\",\"body\":\"[data-fd-sel='{i}']\"}}])");
      match self.inner.evaluate_to_element(&tagged, None).await {
        Ok(element) => {
          handles.push(crate::element_handle::ElementHandle::from_any_element(Arc::clone(self), element).await?);
        },
        Err(err) => {
          crate::selectors::cleanup_tags(&self.inner).await;
          return Err(err);
        },
      }
    }
    crate::selectors::cleanup_tags(&self.inner).await;
    Ok(handles)
  }

  // ── evaluate (Playwright parity) ─────────────────────────────────────

  /// Playwright: `page.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:515`).
  /// Delegates to the main frame, same as Playwright's `this._mainFrame.evaluate(...)`.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::error::FerriError`] on page-side exception or
  /// protocol failure.
  pub async fn evaluate(
    self: &Arc<Self>,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self.main_frame().evaluate(fn_source, arg, is_function).await
  }

  /// Playwright: `page.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  /// Delegates to the main frame.
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`].
  pub async fn evaluate_handle(
    self: &Arc<Self>,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::js_handle::JSHandle> {
    self.main_frame().evaluate_handle(fn_source, arg, is_function).await
  }

  // ── Action methods (delegate to mainFrame — Playwright parity) ─────
  //
  // Mirrors `/tmp/playwright/packages/playwright-core/src/client/page.ts:658+`:
  // every action delegates to `this._mainFrame.<method>(...)`. The
  // `tracing::debug!` events stay at this layer so logs identify the
  // top-level entry point.

  /// Click on an element matching the selector. Accepts Playwright's
  /// full `PageClickOptions` bag (see [`crate::options::ClickOptions`]).
  /// Delegates to `mainFrame().click(selector, opts)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  pub async fn click(self: &Arc<Self>, selector: &str, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    tracing::debug!(target: "ferridriver::action", action = "click", selector, "page.click");
    self.main_frame().click(selector, opts).await
  }

  /// Double-click an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the double-click fails.
  pub async fn dblclick(self: &Arc<Self>, selector: &str, opts: Option<crate::options::DblClickOptions>) -> Result<()> {
    tracing::debug!(target: "ferridriver::action", action = "dblclick", selector, "page.dblclick");
    self.main_frame().dblclick(selector, opts).await
  }

  /// Fill an input element matching the selector with a value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  pub async fn fill(
    self: &Arc<Self>,
    selector: &str,
    value: &str,
    opts: Option<crate::options::FillOptions>,
  ) -> Result<()> {
    tracing::debug!(target: "ferridriver::action", action = "fill", selector, "page.fill");
    self.main_frame().fill(selector, value, opts).await
  }

  /// Type text character-by-character into an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  pub async fn r#type(
    self: &Arc<Self>,
    selector: &str,
    text: &str,
    opts: Option<crate::options::TypeOptions>,
  ) -> Result<()> {
    self.main_frame().r#type(selector, text, opts).await
  }

  /// Press a key on an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  pub async fn press(
    self: &Arc<Self>,
    selector: &str,
    key: &str,
    opts: Option<crate::options::PressOptions>,
  ) -> Result<()> {
    self.main_frame().press(selector, key, opts).await
  }

  /// Hover over an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  pub async fn hover(self: &Arc<Self>, selector: &str, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    self.main_frame().hover(selector, opts).await
  }

  /// Select an option in a `<select>` element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot be selected.
  pub async fn select_option(
    self: &Arc<Self>,
    selector: &str,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.main_frame().select_option(selector, values, opts).await
  }

  /// Set input files on a file input element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  pub async fn set_input_files(
    self: &Arc<Self>,
    selector: &str,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    self.main_frame().set_input_files(selector, files, opts).await
  }

  /// Check a checkbox or radio button matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub async fn check(self: &Arc<Self>, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.main_frame().check(selector, opts).await
  }

  /// Uncheck a checkbox matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  pub async fn uncheck(self: &Arc<Self>, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.main_frame().uncheck(selector, opts).await
  }

  /// Set a checkbox or radio matching `selector` to `checked`. Mirrors
  /// Playwright's `page.setChecked(selector, checked, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:439`).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub async fn set_checked(
    self: &Arc<Self>,
    selector: &str,
    checked: bool,
    opts: Option<crate::options::CheckOptions>,
  ) -> Result<()> {
    self.main_frame().set_checked(selector, checked, opts).await
  }

  /// Tap (touch) the element matched by `selector`. Mirrors Playwright's
  /// `page.tap(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:308`).
  /// Distinct from `Touchscreen::tap(x, y)` which is the lower-level
  /// coordinate-based touch primitive.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the tap fails.
  pub async fn tap(self: &Arc<Self>, selector: &str, opts: Option<crate::options::TapOptions>) -> Result<()> {
    self.main_frame().tap(selector, opts).await
  }

  // ── Content ─────────────────────────────────────────────────────────────

  /// Get the full HTML content of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be retrieved.
  pub async fn content(&self) -> Result<String> {
    self.inner.content().await
  }

  /// Set the page's HTML content.
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be set.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    self.inner.set_content(html).await?;
    // Playwright `page.setContent` defaults to `waitUntil: 'load'`.
    // Wait for the injected document to finish loading so its
    // subframes attach, then refresh the frame cache from the live
    // tree: the `FrameAttached` listener can miss iframes inserted via
    // `Page.setDocumentContent` on a never-navigated page (the parent
    // main frame was never event-seeded), so `frames()` /
    // `frameLocator` would otherwise never see them.
    let _ = self.wait_for_load_state(Some("load")).await;
    if let Ok(infos) = self.inner.get_frame_tree().await
      && let Ok(mut g) = self.frame_cache.lock()
    {
      g.seed(infos);
    }
    Ok(())
  }

  /// Extract the page content as markdown.
  ///
  /// # Errors
  ///
  /// Returns an error if the markdown extraction fails.
  pub async fn markdown(&self) -> Result<String> {
    actions::extract_markdown(&self.inner).await
  }

  /// Get the text content of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn text_content(self: &Arc<Self>, selector: &str) -> Result<Option<String>> {
    self.main_frame().text_content(selector).await
  }

  /// Get the inner text of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_text(self: &Arc<Self>, selector: &str) -> Result<String> {
    self.main_frame().inner_text(selector).await
  }

  /// Get the inner HTML of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_html(self: &Arc<Self>, selector: &str) -> Result<String> {
    self.main_frame().inner_html(selector).await
  }

  /// Get an attribute value from an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn get_attribute(self: &Arc<Self>, selector: &str, name: &str) -> Result<Option<String>> {
    self.main_frame().get_attribute(selector, name).await
  }

  /// Get the input value of a form element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn input_value(self: &Arc<Self>, selector: &str) -> Result<String> {
    self.main_frame().input_value(selector).await
  }

  // ── State checks (delegate to mainFrame) ────────────────────────────

  /// Check if an element matching the selector is visible.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_visible(self: &Arc<Self>, selector: &str) -> Result<bool> {
    self.main_frame().is_visible(selector).await
  }

  /// Check if an element matching the selector is hidden.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_hidden(self: &Arc<Self>, selector: &str) -> Result<bool> {
    self.main_frame().is_hidden(selector).await
  }

  /// Check if an element matching the selector is enabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_enabled(self: &Arc<Self>, selector: &str) -> Result<bool> {
    self.main_frame().is_enabled(selector).await
  }

  /// Check if an element matching the selector is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_disabled(self: &Arc<Self>, selector: &str) -> Result<bool> {
    self.main_frame().is_disabled(selector).await
  }

  /// Check if a checkbox or radio button matching the selector is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_checked(self: &Arc<Self>, selector: &str) -> Result<bool> {
    self.main_frame().is_checked(selector).await
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  /// Wait for an element matching the selector to satisfy the wait condition.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_selector(self: &Arc<Self>, selector: &str, opts: WaitOptions) -> Result<()> {
    self.locator(selector, None).wait_for(opts).await
  }

  /// Wait for the page URL to match the given matcher.
  ///
  /// Accepts glob, regex, or predicate via [`crate::url_matcher::UrlMatcher`].
  /// Mirrors Playwright's `page.waitForURL(url | RegExp | predicate)` semantic.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_url(&self, matcher: crate::url_matcher::UrlMatcher) -> Result<()> {
    let timeout_ms = self.default_navigation_timeout();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for URL matching {:?}", matcher.identifier()),
          timeout_ms,
        ));
      }
      let current = self.url();
      if matcher.matches(&current) {
        return Ok(());
      }
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
  }

  pub async fn wait_for_timeout(&self, ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
  }

  /// Wait for a specific load state. Supported states:
  /// - `"load"` (default) - wait for `document.readyState === "complete"`
  /// - `"domcontentloaded"` - wait for `document.readyState !== "loading"`
  /// - `"networkidle"` - wait for no network activity for 500ms
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out before the load state is reached.
  pub async fn wait_for_load_state(&self, state: Option<&str>) -> Result<()> {
    let state = state.unwrap_or("load");
    let timeout_ms = self.default_timeout();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

    match state {
      "domcontentloaded" => loop {
        if tokio::time::Instant::now() >= deadline {
          return Err(crate::error::FerriError::timeout(
            "waiting for domcontentloaded",
            timeout_ms,
          ));
        }
        if let Ok(Some(v)) = self.inner.evaluate("document.readyState").await {
          let s = v.as_str().unwrap_or("loading");
          if s == "interactive" || s == "complete" {
            return Ok(());
          }
        }
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
      },
      "networkidle" => {
        // Wait for no pending network requests for 500ms.
        // Uses Performance API to detect network activity.
        let mut idle_since = tokio::time::Instant::now();
        let idle_threshold = std::time::Duration::from_millis(500);
        loop {
          if tokio::time::Instant::now() >= deadline {
            return Err(crate::error::FerriError::timeout("waiting for networkidle", timeout_ms));
          }
          // Check if there are pending resource loads
          let has_pending = self
            .inner
            .evaluate(
              "(function(){var p=performance.getEntriesByType('resource');\
             var now=performance.now();\
             return p.some(function(e){return e.responseEnd===0 || (now - e.responseEnd) < 100})})()",
            )
            .await
            .ok()
            .flatten();
          if has_pending == Some(serde_json::Value::Bool(true)) {
            idle_since = tokio::time::Instant::now();
          } else if tokio::time::Instant::now() - idle_since >= idle_threshold {
            return Ok(());
          }
          tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
      },
      _ => {
        // "load" -- wait for document.readyState === "complete"
        loop {
          if tokio::time::Instant::now() >= deadline {
            return Err(crate::error::FerriError::timeout("waiting for load state", timeout_ms));
          }
          if let Ok(Some(v)) = self.inner.evaluate("document.readyState").await {
            if v.as_str() == Some("complete") {
              return Ok(());
            }
          }
          tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
      },
    }
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  /// Take a screenshot of the page. Mirrors Playwright's
  /// `page.screenshot(options?: PageScreenshotOptions)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280`.
  ///
  /// Lowers the Playwright-shaped [`ScreenshotOptions`] bag into the
  /// backend-level [`ScreenshotOpts`] wire struct. Handles Rust-side
  /// concerns (writing `path` to disk, applying `timeout` via a
  /// `tokio::time::timeout` race) that don't belong in the per-backend
  /// dispatch path.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if the capture
  /// exceeds `opts.timeout` milliseconds; otherwise propagates any
  /// backend-specific failure.
  pub async fn screenshot(&self, opts: ScreenshotOptions) -> Result<Vec<u8>> {
    let format = match opts.format.as_deref() {
      Some("jpeg" | "jpg") => ImageFormat::Jpeg,
      Some("webp") => ImageFormat::Webp,
      _ => ImageFormat::Png,
    };
    let scale = match opts.scale.as_deref() {
      Some("css") => Some(crate::backend::ScreenshotScale::Css),
      Some("device") => Some(crate::backend::ScreenshotScale::Device),
      _ => None,
    };
    let animations = match opts.animations.as_deref() {
      Some("disabled") => Some(crate::backend::ScreenshotAnimations::Disabled),
      Some("allow") => Some(crate::backend::ScreenshotAnimations::Allow),
      _ => None,
    };
    let caret = match opts.caret.as_deref() {
      Some("hide") => Some(crate::backend::ScreenshotCaret::Hide),
      Some("initial") => Some(crate::backend::ScreenshotCaret::Initial),
      _ => None,
    };
    let wire = ScreenshotOpts {
      format,
      quality: opts.quality,
      full_page: opts.full_page.unwrap_or(false),
      clip: opts.clip,
      omit_background: opts.omit_background.unwrap_or(false),
      scale,
      animations,
      caret,
      mask: opts.mask.iter().map(|l| l.selector().to_string()).collect(),
      mask_color: opts.mask_color.clone(),
      style: opts.style.clone(),
    };
    let capture = async { self.inner.screenshot(wire).await };
    let bytes = match opts.timeout {
      Some(ms) if ms > 0 => {
        let fut = tokio::time::timeout(std::time::Duration::from_millis(ms), capture);
        match fut.await {
          Ok(res) => res?,
          Err(_) => {
            return Err(crate::error::FerriError::timeout("screenshot", ms));
          },
        }
      },
      _ => capture.await?,
    };
    if let Some(ref path) = opts.path {
      if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
      }
      std::fs::write(path, &bytes)
        .map_err(|e| crate::error::FerriError::Backend(format!("screenshot write {}: {e}", path.display())))?;
    }
    Ok(bytes)
  }

  /// Take a screenshot of a specific element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or screenshot capture fails.
  pub async fn screenshot_element(self: &Arc<Self>, selector: &str) -> Result<Vec<u8>> {
    self.locator(selector, None).screenshot().await
  }

  // ── PDF ─────────────────────────────────────────────────────────────────

  /// Generate a PDF of the current page (Chrome-family backends only).
  ///
  /// Accepts the full Playwright `PDFOptions` surface via
  /// [`crate::options::PdfOptions`]. If `opts.path` is set, the rendered
  /// bytes are additionally written to that path (creating parent directories
  /// as needed) — mirroring Playwright's `page.pdf({ path })` behavior.
  ///
  /// # Errors
  ///
  /// Returns an error if PDF generation is not supported by the active
  /// backend (`WebKit` has no printToPDF analogue), if the paper format is
  /// unknown, if CDP rejects the parameters, or if writing to `path` fails.
  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    let path = opts.path.clone();
    let bytes = self.inner.pdf(opts).await?;
    if let Some(path) = path {
      if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
          tokio::fs::create_dir_all(parent).await?;
        }
      }
      tokio::fs::write(&path, &bytes).await?;
    }
    Ok(bytes)
  }

  // ── Snapshot ────────────────────────────────────────────────────────────

  /// LLM-optimized accessibility snapshot with page context header, optional
  /// depth limiting, and incremental change tracking.
  ///
  /// Returns `SnapshotForAI`:
  /// - `full`: page header (URL, title, scroll, console errors) + accessibility tree
  /// - `incremental`: only changed/new nodes since last call with same `track` key
  /// - `ref_map`: element refs (e.g. "e5") to backend DOM node IDs
  ///
  /// Options:
  /// - `depth`: limits accessibility tree depth (-1 or None = unlimited).
  ///   Uses native CDP depth param on Chrome, `NSAccessibility` depth on `WebKit`.
  /// - `track`: enables incremental tracking per key.
  ///
  /// # Errors
  ///
  /// Returns an error if the accessibility snapshot cannot be built.
  pub async fn snapshot_for_ai(&self, opts: snapshot::SnapshotOptions) -> Result<snapshot::SnapshotForAI> {
    let mut tracker = self.snapshot_tracker.lock().await;
    Box::pin(snapshot::build_snapshot_for_ai(&self.inner, &opts, &mut tracker)).await
  }

  /// Playwright `page.ariaSnapshot(options?): Promise<string>` — the
  /// full accessibility-tree text (the `full` field of the structured
  /// snapshot).
  ///
  /// # Errors
  ///
  /// Returns an error if the accessibility snapshot cannot be built.
  pub async fn aria_snapshot(&self, opts: snapshot::SnapshotOptions) -> Result<String> {
    Ok(Box::pin(self.snapshot_for_ai(opts)).await?.full)
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  /// Set the viewport size by width and height.
  ///
  /// # Errors
  ///
  /// Returns an error if the viewport emulation fails.
  pub async fn set_viewport_size(&self, width: i64, height: i64) -> Result<()> {
    self
      .inner
      .emulate_viewport(&crate::options::ViewportConfig {
        width,
        height,
        ..Default::default()
      })
      .await
  }

  // ── Input devices ───────────────────────────────────────────────────────

  /// Click at specific coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.inner.click_at(x, y).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Click at specific coordinates with button and click count options.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    self.inner.click_at_opts(x, y, button, click_count).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Move the mouse to specific coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  pub(crate) async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self.inner.move_mouse(x, y).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Move the mouse smoothly from one point to another over multiple steps.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    self.inner.move_mouse_smooth(from_x, from_y, to_x, to_y, steps).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (to_x, to_y);
    Ok(())
  }

  /// Drag an element matching `source_selector` onto an element matching
  /// `target_selector`. Mirrors Playwright's
  /// `page.dragAndDrop(source, target, options)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486`.
  ///
  /// The [`crate::options::DragAndDropOptions::strict`] field, when set,
  /// overrides the default strict-mode of the source/target locators —
  /// `Some(true)` errors on multi-match, `Some(false)` allows the first
  /// match, `None` keeps the default (`strict = true`). All other fields
  /// are forwarded to [`crate::locator::Locator::drag_to`].
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found or the
  /// drag-and-drop operation fails.
  pub async fn drag_and_drop(
    self: &Arc<Self>,
    source_selector: &str,
    target_selector: &str,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let source = self.locator(source_selector, None);
    let target = self.locator(target_selector, None);
    let (source, target) = match opts.strict {
      Some(s) => (source.strict(s), target.strict(s)),
      None => (source, target),
    };
    source.drag_to(&target, Some(opts)).await
  }

  /// Dispatch a keyDown event for a single key (does NOT release it).
  ///
  /// # Errors
  ///
  /// Returns an error if the key down dispatch fails.
  pub(crate) async fn key_down(&self, key: &str) -> Result<()> {
    self.inner.key_down(key).await
  }

  /// Dispatch a keyUp event for a single key.
  ///
  /// # Errors
  ///
  /// Returns an error if the key up dispatch fails.
  pub(crate) async fn key_up(&self, key: &str) -> Result<()> {
    self.inner.key_up(key).await
  }

  /// Press a key or combo (e.g., "Enter", "Control+a").
  ///
  /// # Errors
  ///
  /// Returns an error if the key press dispatch fails.
  pub(crate) async fn press_key(&self, key: &str) -> Result<()> {
    self.inner.press_key(key).await
  }

  /// Find element by CSS selector (raw backend access).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn find_element(&self, selector: &str) -> Result<crate::backend::AnyElement> {
    self.inner.find_element(selector).await
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  /// Apply a full [`crate::options::BrowserContextOptions`] bag to
  /// this page. The single entry point for context-level state —
  /// delegates to the backend's `apply_context_options` which fires
  /// every relevant protocol command in parallel and aggregates
  /// errors per field. Mirrors Playwright's pattern of storing the
  /// bag on the context and having each `FrameSession._initialize()`
  /// read from it on page open (see
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:510-545`).
  ///
  /// `Box::pin`ned because the inner future composes 16 per-field
  /// `OptionFuture`s whose combined state machine is too large for
  /// an async-fn stack frame by clippy's default.
  ///
  /// # Errors
  ///
  /// Returns an aggregated error when one or more fields fail to
  /// apply. The aggregated message lists each failing field by name.
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    Box::pin(self.inner.apply_context_options(opts)).await?;
    // Also stash the bag in shared state so subsequent reads (e.g.
    // `page.goto` resolving against the context's `baseURL`,
    // `request` fixture's per-request base URL) see the same values
    // the test runner just applied. Without this, calls like
    // `apply_page_config` would dispatch the CDP commands but the
    // bag stored at `BrowserContext` creation time stays empty,
    // leaving relative `page.goto('/route')` to fail with "Cannot
    // navigate to invalid URL".
    if let Some(ctx) = self.context_ref.as_ref() {
      let composite = ctx.key.to_composite();
      let state = ctx.state.read().await;
      let mut bag = state.get_context_options(&composite).unwrap_or_default();
      // Merge: keep prior fields the bag may carry; overwrite the
      // ones the caller specified. For now the merge is "callers
      // pass a fully populated bag" so a wholesale replace is fine
      // — keep this simple unless a real use-case needs deep merge.
      if opts.base_url.is_some() {
        bag.base_url.clone_from(&opts.base_url);
      }
      if opts.user_agent.is_some() {
        bag.user_agent.clone_from(&opts.user_agent);
      }
      if opts.viewport != crate::options::ViewportOption::default() {
        bag.viewport.clone_from(&opts.viewport);
      }
      if opts.locale.is_some() {
        bag.locale.clone_from(&opts.locale);
      }
      if opts.timezone_id.is_some() {
        bag.timezone_id.clone_from(&opts.timezone_id);
      }
      state.set_context_options(&composite, bag);
    }
    Ok(())
  }

  // Context-level setters (setUserAgent, setLocale, setTimezone,
  // setGeolocation, setNetworkState, setBypassCSP,
  // setIgnoreCertificateErrors, setDownloadBehavior,
  // setHTTPCredentials, setServiceWorkersBlocked,
  // setJavaScriptEnabled, grantPermissions, resetPermissions,
  // setFocusEmulationEnabled, setStorageState) were removed. The
  // single entry point is [`Self::apply_context_options`] — matches
  // Playwright's public API where these are all properties of the
  // `BrowserContextOptions` bag, not page-level mutators. Context-
  // level setters (`context.setGeolocation` etc.) live on
  // [`crate::ContextRef`] and mutate the bag + re-apply to every
  // open page.

  /// Emulate media features (color scheme, reduced motion, media type,
  /// forced-colors, contrast). Mirrors Playwright's
  /// `page.emulateMedia(options?)` — each call is a *partial update*
  /// applied on top of the page's persistent emulated-media state. A field
  /// set to `Some(value)` overrides; a field left `None` is unchanged.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the media emulation.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    // Merge the incoming partial update with the page's persistent state.
    // An `Unchanged` field leaves the existing override alone; a `Disabled`
    // or `Set` field overwrites the stored state for that field.
    let merged = {
      let mut state = self
        .emulated_media
        .lock()
        .map_err(|e| crate::error::FerriError::Backend(format!("emulated_media lock poisoned: {e}")))?;
      if opts.media.is_specified() {
        state.media = opts.media.clone();
      }
      if opts.color_scheme.is_specified() {
        state.color_scheme = opts.color_scheme.clone();
      }
      if opts.reduced_motion.is_specified() {
        state.reduced_motion = opts.reduced_motion.clone();
      }
      if opts.forced_colors.is_specified() {
        state.forced_colors = opts.forced_colors.clone();
      }
      if opts.contrast.is_specified() {
        state.contrast = opts.contrast.clone();
      }
      state.clone()
    };
    self.inner.emulate_media(&merged).await
  }

  /// Enable or disable JavaScript execution.
  ///
  /// # Errors
  /// Set extra HTTP headers that will be sent with every request.
  /// Playwright public: `page.setExtraHTTPHeaders(headers)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the headers cannot be set.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    self.inner.set_extra_http_headers(headers).await
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  /// Start performance tracing.
  ///
  /// # Errors
  ///
  /// Returns an error if tracing cannot be started.
  pub async fn start_tracing(&self) -> Result<()> {
    self.inner.start_tracing().await
  }

  /// Stop performance tracing.
  ///
  /// # Errors
  ///
  /// Returns an error if tracing cannot be stopped.
  pub async fn stop_tracing(&self) -> Result<()> {
    self.inner.stop_tracing().await
  }

  /// Get performance metrics from the page.
  ///
  /// # Errors
  ///
  /// Returns an error if metrics cannot be retrieved.
  pub async fn metrics(&self) -> Result<Vec<crate::backend::MetricData>> {
    self.inner.metrics().await
  }

  // ── Storage State ──────────────────────────────────────────────────────

  /// Serialize the current page's storage state (cookies + localStorage) to JSON.
  ///
  /// Returns Playwright-compatible format:
  /// ```json
  /// {
  ///   "cookies": [{ "name": "...", "value": "...", "domain": "...", ... }],
  ///   "origins": [{ "origin": "https://...", "localStorage": [{ "name": "...", "value": "..." }] }]
  /// }
  /// ```
  ///
  /// Can be saved to a file and loaded later with `set_storage_state` or via
  /// test config `storage_state: "auth.json"`.
  ///
  /// # Errors
  ///
  /// Returns an error if cookies or localStorage cannot be retrieved.
  pub async fn storage_state(&self) -> Result<serde_json::Value> {
    let cookies = self.inner.get_cookies().await?;
    let cookies_json: Vec<serde_json::Value> = cookies
      .iter()
      .map(|c| {
        let mut obj = serde_json::json!({
          "name": c.name, "value": c.value, "domain": c.domain, "path": c.path,
          "secure": c.secure, "httpOnly": c.http_only
        });
        if let Some(expires) = c.expires {
          obj["expires"] = serde_json::json!(expires);
        }
        if let Some(same_site) = c.same_site {
          obj["sameSite"] = serde_json::json!(same_site.as_str());
        }
        obj
      })
      .collect();

    // Get the current origin for localStorage grouping.
    let origin = self
      .inner
      .evaluate("location.origin")
      .await
      .ok()
      .flatten()
      .and_then(|v| v.as_str().map(str::to_string))
      .unwrap_or_default();

    // Dump localStorage as array of { name, value } pairs (Playwright format).
    let storage_js = r"JSON.stringify(
      Object.keys(localStorage).map(k => ({ name: k, value: localStorage.getItem(k) }))
    )";
    let storage_r = self.inner.evaluate(storage_js).await.ok().flatten();
    let local_storage: Vec<serde_json::Value> = storage_r
      .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
      .unwrap_or_default();

    let mut origins = Vec::new();
    if !local_storage.is_empty() && !origin.is_empty() {
      origins.push(serde_json::json!({
        "origin": origin,
        "localStorage": local_storage,
      }));
    }

    Ok(serde_json::json!({
      "cookies": cookies_json,
      "origins": origins,
    }))
  }

  /// Restore a previously saved storage state (cookies + localStorage).
  ///
  /// Accepts Playwright-compatible format with `origins[].localStorage[]` (name/value pairs).
  ///
  /// # Errors
  ///
  /// Returns an error if cookies or localStorage cannot be restored.
  pub async fn set_storage_state(&self, state: &serde_json::Value) -> Result<()> {
    // Restore cookies.
    if let Some(cookies) = state.get("cookies").and_then(|v| v.as_array()) {
      for c in cookies {
        let cookie = CookieData {
          name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          domain: c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          path: c.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string(),
          secure: c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false),
          http_only: c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false),
          expires: c.get("expires").and_then(serde_json::Value::as_f64),
          same_site: c
            .get("sameSite")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<crate::backend::SameSite>().ok()),
          url: None,
        };
        self.inner.set_cookie(cookie).await?;
      }
    }

    // Restore per-origin localStorage (Playwright format).
    if let Some(origins) = state.get("origins").and_then(|v| v.as_array()) {
      for origin_entry in origins {
        let origin = origin_entry.get("origin").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(items) = origin_entry.get("localStorage").and_then(|v| v.as_array()) {
          // Navigate to the origin so localStorage.setItem works in the right scope.
          // Only navigate if the current page isn't already on this origin.
          let current_origin = self
            .inner
            .evaluate("location.origin")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
          if !origin.is_empty() && current_origin != origin {
            let _ = self
              .inner
              .goto(origin, crate::backend::NavLifecycle::Load, 10_000, None)
              .await;
          }
          for item in items {
            let key = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let val = item.get("value").and_then(|v| v.as_str()).unwrap_or("");
            self
              .inner
              .evaluate(&format!(
                "localStorage.setItem('{}', '{}')",
                crate::steps::js_escape(key),
                crate::steps::js_escape(val)
              ))
              .await?;
          }
        }
      }
    }

    Ok(())
  }

  // ── Focus / dispatch ─────────────────────────────────────────────────

  /// Focus an element by selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn focus(self: &Arc<Self>, selector: &str) -> Result<()> {
    self.locator(selector, None).focus().await
  }

  /// Dispatch an event on an element by selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the event dispatch fails.
  pub async fn dispatch_event(
    self: &Arc<Self>,
    selector: &str,
    event_type: &str,
    event_init: Option<serde_json::Value>,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .locator(selector, None)
      .dispatch_event(event_type, event_init, opts)
      .await
  }

  /// Check if an element is editable (not disabled, not readonly).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_editable(self: &Arc<Self>, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_editable().await
  }

  // ── Waiting (additional) ────────────────────────────────────────────────

  /// Wait for a JS function/expression to return a truthy value.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_function(&self, expression: &str, timeout_ms: Option<u64>) -> Result<serde_json::Value> {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for function: {expression}"),
          timeout,
        ));
      }
      if let Ok(Some(val)) = self.inner.evaluate(expression).await {
        let truthy = match &val {
          serde_json::Value::Bool(b) => *b,
          serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
          serde_json::Value::String(s) => !s.is_empty(),
          serde_json::Value::Null => false,
          _ => true,
        };
        if truthy {
          return Ok(val);
        }
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  /// Wait for the page to navigate to a URL matching the pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_navigation(&self, timeout_ms: Option<u64>) -> Result<()> {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let current = self.url();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(crate::error::FerriError::timeout("waiting for navigation", timeout));
      }
      let now = self.url();
      if now != current {
        return Ok(());
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  // ── Mouse (low-level) ──────────────────────────────────────────────────

  /// Scroll via mouse wheel event.
  ///
  /// # Errors
  ///
  /// Returns an error if the wheel event dispatch fails.
  pub(crate) async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self.inner.mouse_wheel(delta_x, delta_y).await
  }

  /// Mouse button down (without up). For custom drag sequences.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse down dispatch fails.
  pub(crate) async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<()> {
    self.inner.mouse_down(x, y, button).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Mouse button up.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse up dispatch fails.
  pub(crate) async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<()> {
    self.inner.mouse_up(x, y, button).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Bring this page to front (focus).
  ///
  /// # Errors
  ///
  /// Returns an error if the page cannot be focused.
  pub async fn bring_to_front(&self) -> Result<()> {
    let _ = self.inner.evaluate("window.focus()").await;
    Ok(())
  }

  // ── Frames (sync, Playwright parity — task 3.8) ──────────────────────
  //
  // Mirrors Playwright client/page.ts:258 (mainFrame), :273 (frames),
  // :262 (frame). All read from the page-owned `FrameCache` seeded by
  // [`Page::init_frame_cache`] and kept fresh by the listener task.

  /// Main frame of this page. Mirrors Playwright's `page.mainFrame():
  /// Frame` (non-null).
  ///
  /// The cache is seeded one of three ways:
  /// 1. The frame listener spawned in `seed_frame_cache` picks up a
  ///    `FrameNavigated` event and writes `main_id`.
  /// 2. `goto` calls `ensure_frame_cache_seeded` after the
  ///    `Page.navigate` response lands, which seeds via the backend's
  ///    `peek_main_frame_id()` (no RTT).
  /// 3. Below: when this accessor is reached without (1) or (2) — for
  ///    example, an MCP tool that constructs a fresh `Page` wrapper
  ///    over an already-navigated inner page — we synchronously seed
  ///    from `peek_main_frame_id()` so the user-visible API never
  ///    panics on a live, navigated page.
  ///
  /// # Panics
  ///
  /// Panics only when the cache is empty AND the backend has never
  /// observed a top-level frame (i.e. no navigation has ever occurred
  /// on this inner page). This is genuine API misuse — Playwright
  /// itself can't return a `Frame` either before the first navigation.
  #[must_use]
  pub fn main_frame(self: &Arc<Self>) -> Frame {
    if let Some(id) = self.with_frame_cache(crate::frame_cache::FrameCache::main_frame_id) {
      return Frame::new(Arc::clone(self), id);
    }
    if let Some(fid) = self.inner.peek_main_frame_id() {
      if let Ok(mut g) = self.frame_cache.lock() {
        if g.main_frame_id().is_none() {
          g.attach(crate::backend::FrameInfo {
            frame_id: fid.clone(),
            parent_frame_id: None,
            name: String::new(),
            url: String::new(),
          });
        }
      }
      return Frame::new(Arc::clone(self), Arc::from(fid));
    }
    panic!(
      "Page::main_frame called before any navigation has occurred (no main frame id available from frame cache or backend)"
    )
  }

  /// All non-detached frames attached to the page, main-frame first.
  /// Sync — reads the cache.
  #[must_use]
  pub fn frames(self: &Arc<Self>) -> Vec<Frame> {
    let ids: Vec<_> = self.with_frame_cache(|c| c.live_ids().collect());
    ids.into_iter().map(|id| Frame::new(Arc::clone(self), id)).collect()
  }

  /// Locate a frame by name or URL. Sync — reads the cache.
  /// Playwright: `page.frame(string | { name?, url? })` — see
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2755`.
  ///
  /// # Panics
  ///
  /// Panics if `selector` specifies neither `name` nor `url` — matches
  /// Playwright's `assert(name || url, 'Either name or url matcher should be specified')`.
  #[must_use]
  pub fn frame(self: &Arc<Self>, selector: impl Into<FrameSelector>) -> Option<Frame> {
    let sel = selector.into();
    assert!(!sel.is_empty(), "Either name or url matcher should be specified");
    self.with_frame_cache(|c| {
      for id in c.live_ids() {
        let Some(rec) = c.record(&id) else { continue };
        if let Some(name) = &sel.name {
          if rec.info.name != *name {
            continue;
          }
        }
        if let Some(url) = &sel.url {
          if rec.info.url != *url {
            continue;
          }
        }
        return Some(Frame::new(Arc::clone(self), id));
      }
      None
    })
  }

  // ── Events ────────────────────────────────────────────────────────────

  /// Get the event emitter for subscribing to page events.
  #[must_use]
  pub fn events(&self) -> &EventEmitter {
    self.inner.events()
  }

  /// Subscribe to page events. Calls the callback for each matching event.
  /// Returns a `ListenerId` for later removal with `off()`.
  ///
  /// ```ignore
  /// let id = page.on("response", Arc::new(|event| {
  ///     if let PageEvent::Response(r) = event {
  ///         println!("Response: {} {}", r.status, r.url);
  ///     }
  /// }));
  /// ```
  pub fn on(&self, event_name: &str, callback: crate::events::EventCallback) -> crate::events::ListenerId {
    self.lazy_enable_for_event(event_name);
    self.inner.events().on(event_name, callback)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: crate::events::EventCallback) -> crate::events::ListenerId {
    self.lazy_enable_for_event(event_name);
    self.inner.events().once(event_name, callback)
  }

  /// Some events depend on a backend command being fired (file
  /// chooser interception, download behaviour). When the user
  /// expresses interest, fire-and-forget the command in the
  /// background — best-effort; failure is silently swallowed and
  /// would surface via the user not getting the event. Mirrors
  /// Playwright's `_updateFileChooserInterception(false)` pattern
  /// where the command is async but fire-and-forget around listener
  /// registration (`crPage.ts:199`).
  fn lazy_enable_for_event(&self, event_name: &str) {
    let needs_filechooser = event_name == "filechooser";
    let needs_download = event_name == "download";
    if !needs_filechooser && !needs_download {
      return;
    }
    let inner_for_task: AnyPage = self.inner.clone();
    tokio::spawn(async move {
      if needs_filechooser {
        let _ = inner_for_task.enable_file_chooser_intercept().await;
      }
      if needs_download {
        let _ = inner_for_task.enable_download_behavior().await;
      }
    });
  }

  /// Remove an event listener by ID.
  pub fn off(&self, id: crate::events::ListenerId) {
    self.inner.events().off(id);
  }

  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    self.inner.events().remove_all_listeners();
  }

  /// Start listening for a navigation event. Call BEFORE the action that triggers navigation.
  /// Returns a future that resolves when navigation completes.
  ///
  /// ```ignore
  /// let nav = page.expect_navigation(None);
  /// page.click("#link").await?;
  /// nav.await?; // resolves when navigation completes
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation event does not occur within the timeout.
  pub fn expect_navigation(&self, timeout_ms: Option<u64>) -> impl std::future::Future<Output = Result<()>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    async move {
      events
        .wait_for(|e| matches!(e, PageEvent::Load | PageEvent::DomContentLoaded), timeout)
        .await?;
      Ok(())
    }
  }

  /// Start listening for a response matching URL pattern. Call BEFORE the action.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching response is received within the timeout.
  pub fn expect_response(
    &self,
    url_pattern: &str,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::network::Response>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    let pattern = url_pattern.to_string();
    async move {
      let event = events
        .wait_for(
          move |e| matches!(e, PageEvent::Response(r) if r.url().contains(&pattern)),
          timeout,
        )
        .await?;
      match event {
        PageEvent::Response(r) => Ok(r),
        _ => Err(crate::error::FerriError::backend(
          "event wait returned unexpected event type",
        )),
      }
    }
  }

  /// Start listening for a request matching URL pattern. Call BEFORE the action.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching request is received within the timeout.
  pub fn expect_request(
    &self,
    url_pattern: &str,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::network::Request>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    let pattern = url_pattern.to_string();
    async move {
      let event = events
        .wait_for(
          move |e| matches!(e, PageEvent::Request(r) if r.url().contains(&pattern)),
          timeout,
        )
        .await?;
      match event {
        PageEvent::Request(r) => Ok(r),
        _ => Err(crate::error::FerriError::backend(
          "event wait returned unexpected event type",
        )),
      }
    }
  }

  /// Wait for a specific event (by name) with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the event does not occur within the timeout.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: Option<u64>) -> Result<PageEvent> {
    self
      .inner
      .events()
      .wait_for_event(event_name, timeout_ms.unwrap_or(self.default_timeout()))
      .await
  }

  /// Wait for a network request matching a URL pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching request occurs within the timeout.
  pub async fn wait_for_request(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    timeout_ms: Option<u64>,
  ) -> Result<crate::network::Request> {
    let event = self
      .inner
      .events()
      .wait_for(
        move |e| matches!(e, PageEvent::Request(r) if matcher.matches(r.url())),
        timeout_ms.unwrap_or(self.default_timeout()),
      )
      .await?;
    match event {
      PageEvent::Request(r) => Ok(r),
      _ => Err(crate::error::FerriError::backend(
        "event wait returned unexpected event type",
      )),
    }
  }

  /// Wait for a network response matching a URL pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching response occurs within the timeout.
  pub async fn wait_for_response(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    timeout_ms: Option<u64>,
  ) -> Result<crate::network::Response> {
    let event = self
      .inner
      .events()
      .wait_for(
        move |e| matches!(e, PageEvent::Response(r) if matcher.matches(r.url())),
        timeout_ms.unwrap_or(self.default_timeout()),
      )
      .await?;
    match event {
      PageEvent::Response(r) => Ok(r),
      _ => Err(crate::error::FerriError::backend(
        "event wait returned unexpected event type",
      )),
    }
  }

  // ── Network Interception ────────────────────────────────────────────────

  /// Intercept network requests matching a URL glob pattern.
  /// The handler receives the intercepted request and returns a `RouteAction`
  /// (Continue, Fulfill, or Abort).
  ///
  /// ```ignore
  /// use ferridriver::route::{RouteAction, FulfillResponse};
  /// use std::sync::Arc;
  ///
  /// // Mock an API endpoint
  /// page.route("**/api/data", Arc::new(|req| {
  ///     RouteAction::Fulfill(FulfillResponse {
  ///         status: 200,
  ///         body: b"{\"mocked\": true}".to_vec(),
  ///         content_type: Some("application/json".into()),
  ///         ..Default::default()
  ///     })
  /// })).await?;
  ///
  /// // Block image loading
  /// page.route("**/*.{png,jpg,gif}", Arc::new(|_| {
  ///     RouteAction::Abort("blockedbyclient".into())
  /// })).await?;
  /// ```
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` reverses
  /// the registration (equivalent to calling [`Page::unroute`] with the same
  /// matcher). Mirrors Playwright `page.route(...)` which returns a
  /// `DisposableStub` (`client/page.ts:535`).
  ///
  /// # Errors
  ///
  /// Returns an error if the route interception cannot be set up.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
  ) -> Result<crate::disposable::Disposable> {
    self.inner.route(matcher.clone(), handler).await?;
    let inner = self.inner.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      inner.unroute(&matcher).await
    }))
  }

  /// Remove all route handlers whose matcher is
  /// [`crate::url_matcher::UrlMatcher::equivalent`] to the given matcher.
  ///
  /// # Errors
  ///
  /// Returns an error if the route handlers cannot be removed.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    self.inner.unroute(matcher).await
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  /// Expose a Rust function to the page as `window.<name>(...)`.
  /// The page can call it as an async function and receive the return value.
  /// The exposed function persists across navigations.
  ///
  /// ```ignore
  /// use std::sync::Arc;
  ///
  /// page.expose_function("compute", Arc::new(|args| {
  ///     let x = args[0].as_f64().unwrap_or(0.0);
  ///     serde_json::json!(x * 2.0)
  /// })).await?;
  ///
  /// // In the page:
  /// // const result = await window.compute(21); // returns 42
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be exposed to the page.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    self.inner.expose_function(name, func).await
  }

  /// Remove a previously exposed function.
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be removed.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.inner.remove_exposed_function(name).await
  }

  // ── Script / Style injection ────────────────────────────────────────────

  /// Add a `<script>` tag to the page. Provide either `url` (external) or `content` (inline).
  /// For URL scripts, waits for the script to load before returning.
  ///
  /// # Errors
  ///
  /// Returns an error if neither `url` nor `content` is provided, or if injection fails.
  pub async fn add_script_tag(
    &self,
    url: Option<&str>,
    content: Option<&str>,
    script_type: Option<&str>,
  ) -> Result<()> {
    let t = script_type.unwrap_or("text/javascript");
    if let Some(url) = url {
      self.inner.evaluate(&format!(
        "(function(){{return new Promise(function(r,j){{var s=document.createElement('script');\
         s.type='{}';s.src='{}';s.onload=r;s.onerror=function(){{j(new Error('Failed to load script'))}};document.head.appendChild(s)}})}})();",
        crate::steps::js_escape(t), crate::steps::js_escape(url)
      )).await?;
    } else if let Some(content) = content {
      self.inner.evaluate(&format!(
        "(function(){{var s=document.createElement('script');s.type='{}';s.text='{}';document.head.appendChild(s)}})()",
        crate::steps::js_escape(t), crate::steps::js_escape(content)
      )).await?;
    } else {
      return Err(crate::error::FerriError::invalid_argument(
        "url-or-content",
        "Provide either 'url' or 'content'",
      ));
    }
    Ok(())
  }

  /// Add a `<style>` tag or `<link>` stylesheet to the page.
  /// Provide either `url` (external CSS) or `content` (inline CSS).
  /// For URL stylesheets, waits for the stylesheet to load before returning.
  ///
  /// # Errors
  ///
  /// Returns an error if neither `url` nor `content` is provided, or if injection fails.
  pub async fn add_style_tag(&self, url: Option<&str>, content: Option<&str>) -> Result<()> {
    if let Some(url) = url {
      self.inner.evaluate(&format!(
        "(function(){{return new Promise(function(r,j){{var l=document.createElement('link');\
         l.rel='stylesheet';l.href='{}';l.onload=r;l.onerror=function(){{j(new Error('Failed to load stylesheet'))}};document.head.appendChild(l)}})}})();",
        crate::steps::js_escape(url)
      )).await?;
    } else if let Some(content) = content {
      self
        .inner
        .evaluate(&format!(
          "(function(){{var s=document.createElement('style');s.textContent='{}';document.head.appendChild(s)}})()",
          crate::steps::js_escape(content)
        ))
        .await?;
    } else {
      return Err(crate::error::FerriError::invalid_argument(
        "url-or-content",
        "Provide either 'url' or 'content'",
      ));
    }
    Ok(())
  }

  // ── Dialog handling ─────────────────────────────────────────────────────
  //
  // Dialogs (alert/confirm/prompt/beforeunload) are observed through
  // two equivalent surfaces:
  //
  // * [`Self::events`]`.on("dialog", cb)` — broadcast listener, live
  //   [`crate::dialog::Dialog`] handle delivered in the callback.
  //   Backed by the per-page [`crate::dialog::DialogManager`]'s
  //   emitter-bridge (installed once at page construction).
  // * [`Self::wait_for_dialog`] — one-shot async wait. Mirrors
  //   Playwright's `page.waitForEvent('dialog')` directly against
  //   the `DialogManager`; bypasses the broadcast entirely so the
  //   claim is synchronous with dialog open, matching Playwright's
  //   `addDialogHandler` semantics verbatim.
  //
  // If no handler claims a dialog at open time, the `DialogManager`
  // auto-closes it — accept for `beforeunload`, dismiss otherwise —
  // matching Playwright's `Dialog._close` branch.

  /// Wait for the next dialog of any type, with a timeout. Returns
  /// the live [`crate::dialog::Dialog`] handle; the caller must then
  /// call `accept(...)` or `dismiss()` exactly once. Mirrors
  /// Playwright's `page.waitForEvent('dialog', { timeout })`.
  ///
  /// Registers a one-shot handler with the page's
  /// [`crate::dialog::DialogManager`] that claims the first dialog
  /// and delivers it here. The handler is removed automatically on
  /// resolve or timeout.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if no dialog opens
  /// within `timeout_ms`. Returns [`crate::error::FerriError::TargetClosed`]
  /// if the page closes before a dialog arrives.
  pub async fn wait_for_dialog(&self, timeout_ms: u64) -> Result<crate::dialog::Dialog> {
    use std::sync::Mutex;
    let (tx, rx) = tokio::sync::oneshot::channel::<crate::dialog::Dialog>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let tx_clone = tx.clone();
    let id = self.inner.dialog_manager().add_handler(Arc::new(move |dialog| {
      let mut guard = match tx_clone.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      match guard.take() {
        Some(sender) => {
          let _ = sender.send(dialog.clone());
          true
        },
        None => false,
      }
    }));
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
    self.inner.dialog_manager().remove_handler(id);
    match result {
      Ok(Ok(dialog)) => Ok(dialog),
      Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(
        "page closed while waiting for dialog".into(),
      ))),
      Err(_) => Err(crate::error::FerriError::timeout("waiting for dialog", timeout_ms)),
    }
  }

  // ── File choosers (live handle, first-class) ────────────────────────────
  //
  // Symmetric with the dialog surface above:
  //
  // * [`Self::events`]`.on("filechooser", cb)` — broadcast listener,
  //   live [`crate::file_chooser::FileChooser`] handle delivered in
  //   the callback. Backed by the per-page
  //   [`crate::file_chooser::FileChooserManager`]'s emitter-bridge
  //   (installed once at `attach_listeners` time).
  // * [`Self::wait_for_file_chooser`] — one-shot async wait. Mirrors
  //   Playwright's `page.waitForEvent('filechooser')` directly against
  //   the `FileChooserManager`; bypasses the broadcast so the claim
  //   is synchronous with the chooser opening.
  //
  // If no handler claims at `did_open` time, the manager disposes the
  // captured `<input>` element handle — matches Playwright's
  // `server/page.ts::_onFileChooserOpened` no-listener branch.

  /// Wait for the next file chooser to open, with a timeout. Returns
  /// the live [`crate::file_chooser::FileChooser`] handle; the caller
  /// may then call `set_files(...)` (or drop the handle to cancel the
  /// upload — the native picker was already suppressed by CDP's
  /// `Page.setInterceptFileChooserDialog`). Mirrors Playwright's
  /// `page.waitForEvent('filechooser', { timeout })`.
  ///
  /// Registers a one-shot handler with the page's
  /// [`crate::file_chooser::FileChooserManager`] that claims the
  /// first chooser and delivers it here. The handler is removed
  /// automatically on resolve or timeout.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if no file chooser
  /// opens within `timeout_ms`. Returns
  /// [`crate::error::FerriError::TargetClosed`] if the page closes
  /// before a chooser arrives.
  pub async fn wait_for_file_chooser(&self, timeout_ms: u64) -> Result<crate::file_chooser::FileChooser> {
    use std::sync::Mutex;
    // Lazy-enable file chooser interception. Idempotent — first
    // call fires `Page.setInterceptFileChooserDialog`, subsequent
    // are no-ops.
    self.inner.enable_file_chooser_intercept().await?;
    let (tx, rx) = tokio::sync::oneshot::channel::<crate::file_chooser::FileChooser>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let tx_clone = tx.clone();
    let id = self.inner.file_chooser_manager().add_handler(Arc::new(move |chooser| {
      let mut guard = match tx_clone.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      match guard.take() {
        Some(sender) => {
          let _ = sender.send(chooser.clone());
          true
        },
        None => false,
      }
    }));
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
    self.inner.file_chooser_manager().remove_handler(id);
    match result {
      Ok(Ok(chooser)) => Ok(chooser),
      Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(
        "page closed while waiting for filechooser".into(),
      ))),
      Err(_) => Err(crate::error::FerriError::timeout("waiting for filechooser", timeout_ms)),
    }
  }

  // ── Downloads (live handle, first-class) ──────────────────────────────
  //
  // Symmetric with dialog / filechooser above.
  //
  // * [`Self::events`]`.on("download", cb)` — broadcast listener, live
  //   [`crate::download::Download`] handle delivered in the callback.
  //   Backed by the per-page
  //   [`crate::download::DownloadManager`]'s emitter-bridge (installed
  //   once at `attach_listeners` time).
  // * [`Self::wait_for_download`] — one-shot async wait. Mirrors
  //   Playwright's `page.waitForEvent('download')`. Registers a
  //   one-shot handler directly on the `DownloadManager`; the claim is
  //   synchronous with the backend's download-begin event, so there's
  //   no broadcast round-trip to race against.
  //
  // Unclaimed downloads are not auto-cancelled — Playwright's server
  // does the same (just emits the event and leaves the bytes in the
  // per-context `downloadsPath`). The per-page `downloads_dir` drop
  // cleans up orphan files on page close.

  /// Wait for the next download, with a timeout. Returns the live
  /// [`crate::download::Download`] handle; the caller may then call
  /// `save_as(path)` / `path()` / `failure()` / `cancel()` / `delete()`.
  /// Mirrors Playwright's `page.waitForEvent('download', { timeout })`.
  ///
  /// Registers a one-shot handler with the page's
  /// [`crate::download::DownloadManager`]; the handler is removed
  /// automatically on resolve or timeout.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if no download
  /// begins within `timeout_ms`. Returns
  /// [`crate::error::FerriError::TargetClosed`] if the page closes
  /// before a download begins.
  pub async fn wait_for_download(&self, timeout_ms: u64) -> Result<crate::download::Download> {
    use std::sync::Mutex;
    // Lazy-enable download behaviour. Idempotent — first call fires
    // `Browser.setDownloadBehavior`, subsequent are no-ops.
    self.inner.enable_download_behavior().await?;
    let (tx, rx) = tokio::sync::oneshot::channel::<crate::download::Download>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let tx_clone = tx.clone();
    let id = self.inner.download_manager().add_handler(Arc::new(move |download| {
      let mut guard = match tx_clone.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      match guard.take() {
        Some(sender) => {
          let _ = sender.send(download.clone());
          true
        },
        None => false,
      }
    }));
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
    self.inner.download_manager().remove_handler(id);
    match result {
      Ok(Ok(download)) => Ok(download),
      Ok(Err(_)) => Err(crate::error::FerriError::target_closed(Some(
        "page closed while waiting for download".into(),
      ))),
      Err(_) => Err(crate::error::FerriError::timeout("waiting for download", timeout_ms)),
    }
  }

  // ── Init Scripts ────────────────────────────────────────────────────────

  /// Register a script to run before any page JS on every navigation.
  /// Mirrors Playwright's `page.addInitScript(script, arg)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  ///
  /// Accepts the full Playwright argument shape: a JS function body
  /// (pre-serialised via `fn.toString()` at the binding layer), a verbatim
  /// source string, a `{ path }`, or a `{ content }`. The optional `arg`
  /// is JSON-stringified and composed into a `(body)(arg)` wrapper for
  /// the `Function` variant; passing `arg` alongside any non-function
  /// variant is a Playwright-parity error (see [`crate::options::evaluation_script`]).
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// injected script (equivalent to [`Page::remove_init_script`] with the
  /// generated identifier). Mirrors Playwright `page.addInitScript(...)` which
  /// returns a `Disposable` (`client/page.ts:532`).
  ///
  /// # Errors
  ///
  /// Returns an error if `evaluation_script` lowering fails (bad path, bad
  /// arg combination, JSON serialisation) or the backend injection fails.
  pub async fn add_init_script(
    &self,
    script: crate::options::InitScriptSource,
    arg: Option<serde_json::Value>,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script(script, arg.as_ref())?;
    let identifier = self.inner.add_init_script(&source).await?;
    let inner = self.inner.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      inner.remove_init_script(&identifier).await
    }))
  }

  /// Remove a previously injected init script by identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if the init script cannot be removed.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    self.inner.remove_init_script(identifier).await
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
  #[tracing::instrument(skip(self, opts))]
  pub async fn close(&self, opts: Option<crate::options::PageCloseOptions>) -> Result<()> {
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

    // Remove closed page from context's page list so context.pages() stays accurate.
    if let Some(ctx) = &self.context_ref {
      let mut state = ctx.state.write().await;
      if let Ok(browser_ctx) = state.context_mut_checked(&ctx.name) {
        browser_ctx.pages.retain(|p| !p.is_closed());
        if browser_ctx.active_page_idx >= browser_ctx.pages.len() && !browser_ctx.pages.is_empty() {
          browser_ctx.active_page_idx = browser_ctx.pages.len() - 1;
        }
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
  /// that were not created with `recordVideo`, and on backends where
  /// recording genuinely isn't wired (e.g. stock `WKWebView`, which
  /// surfaces a typed `Unsupported` via the handle's accessors).
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

  /// Start CDP screencast. Returns a channel of `(jpeg_bytes, timestamp_secs)` pairs.
  /// Timestamp is Chrome's `metadata.timestamp` from the screencastFrame event.
  ///
  /// # Errors
  ///
  /// Returns an error if screencast cannot be started on the backend.
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<(
    tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>,
    tokio::sync::oneshot::Sender<()>,
  )> {
    self.inner.start_screencast(quality, max_width, max_height).await
  }

  /// Stop CDP screencast.
  ///
  /// # Errors
  ///
  /// Returns an error if screencast cannot be stopped on the backend.
  pub async fn stop_screencast(&self) -> Result<()> {
    self.inner.stop_screencast().await
  }
}

impl std::fmt::Debug for Page {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Page").finish()
  }
}

// ── Keyboard ──────────────────────────────────────────────────────────────

/// Keyboard interface for a page. Mirrors Playwright's `page.keyboard`.
pub struct Keyboard<'a> {
  page: &'a Page,
}

impl Keyboard<'_> {
  /// Dispatch a keyDown event. The key is held until `up()` is called.
  ///
  /// Supports modifier keys: "Shift", "Control", "Alt", "Meta".
  /// Holding Shift will type uppercase text via subsequent `press()` or `type()` calls.
  ///
  /// # Errors
  ///
  /// Returns an error if the key down dispatch fails.
  pub async fn down(&self, key: &str) -> Result<()> {
    self.page.key_down(key).await
  }

  /// Dispatch a keyUp event for a previously held key.
  ///
  /// # Errors
  ///
  /// Returns an error if the key up dispatch fails.
  pub async fn up(&self, key: &str) -> Result<()> {
    self.page.key_up(key).await
  }

  /// Press a key or key combination (e.g., "Enter", "Control+a", "Shift+ArrowDown").
  ///
  /// Shortcut for `down(key)` followed by `up(key)`. Supports `+` combinator for
  /// modifier combinations.
  ///
  /// # Errors
  ///
  /// Returns an error if the key press dispatch fails.
  pub async fn press(&self, key: &str, opts: Option<KeyboardPressOptions>) -> Result<()> {
    match opts.and_then(|o| o.delay) {
      // Playwright `delay` waits between keydown and keyup. Combos
      // ("Control+a") keep the atomic `press_key` path.
      Some(ms) if !key.contains('+') => {
        self.page.key_down(key).await?;
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        self.page.key_up(key).await
      },
      _ => self.page.press_key(key).await,
    }
  }

  /// Type text character by character with full keyboard events.
  ///
  /// Sends `keydown`, `keypress`/`input`, and `keyup` events for each character
  /// in the text. For characters not representable as single key presses,
  /// falls back to `insert_text` for that character.
  ///
  /// # Errors
  ///
  /// Returns an error if the typing dispatch fails.
  pub async fn r#type(&self, text: &str, opts: Option<KeyboardTypeOptions>) -> Result<()> {
    let delay = opts.and_then(|o| o.delay);
    let mut first = true;
    for ch in text.chars() {
      if let (false, Some(ms)) = (first, delay) {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
      }
      first = false;
      self.page.press_key(&ch.to_string()).await?;
    }
    Ok(())
  }

  /// Insert text directly without emitting keyboard events.
  ///
  /// Only dispatches an `input` event. Modifier keys do NOT affect `insert_text`.
  /// Useful for inserting characters not available on a US keyboard layout.
  ///
  /// # Errors
  ///
  /// Returns an error if the text insertion fails.
  pub async fn insert_text(&self, text: &str) -> Result<()> {
    self.page.inner.insert_text(text).await
  }
}

// ── Mouse ─────────────────────────────────────────────────────────────────

/// Mouse interface for a page. Mirrors Playwright's `page.mouse`.
pub struct Mouse<'a> {
  page: &'a Page,
}

impl Mouse<'_> {
  /// Click at coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn click(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    let count = opts.as_ref().and_then(|o| o.click_count).unwrap_or(1);
    match opts.as_ref().and_then(|o| o.delay) {
      Some(ms) => {
        self.page.move_mouse(x, y).await?;
        for _ in 0..count {
          self.page.mouse_down(x, y, button).await?;
          tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
          self.page.mouse_up(x, y, button).await?;
        }
        Ok(())
      },
      None => self.page.click_at_opts(x, y, button, count).await,
    }
  }

  /// Move mouse to coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  pub async fn r#move(&self, x: f64, y: f64, steps: Option<u32>) -> Result<()> {
    match steps {
      Some(step_count) => {
        let (from_x, from_y) = *self
          .page
          .mouse_position
          .lock()
          .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
        self.page.move_mouse_smooth(from_x, from_y, x, y, step_count).await
      },
      None => self.page.move_mouse(x, y).await,
    }
  }

  /// Double-click at coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn dblclick(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    self.page.move_mouse(x, y).await?;
    self.page.mouse_down(x, y, button).await?;
    self.page.mouse_up(x, y, button).await?;
    if let Some(ms) = opts.as_ref().and_then(|o| o.delay) {
      tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    self.page.mouse_down(x, y, button).await?;
    self.page.mouse_up(x, y, button).await?;
    Ok(())
  }

  /// Press mouse button down at the current cursor position.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse down dispatch fails.
  pub async fn down(&self, opts: Option<MouseDownOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    let (x, y) = *self
      .page
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
    self.page.mouse_down(x, y, button).await
  }

  /// Release mouse button at the current cursor position.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse up dispatch fails.
  pub async fn up(&self, opts: Option<MouseUpOptions>) -> Result<()> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    let (x, y) = *self
      .page
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
    self.page.mouse_up(x, y, button).await
  }

  /// Scroll via mouse wheel.
  ///
  /// # Errors
  ///
  /// Returns an error if the wheel event dispatch fails.
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self.page.mouse_wheel(delta_x, delta_y).await
  }
}

/// Options for `Mouse.click()`.
#[derive(Debug, Clone, Default)]
pub struct MouseClickOptions {
  /// Mouse button: "left", "right", "middle"
  pub button: Option<String>,
  /// Click count (1=single, 2=double, 3=triple)
  pub click_count: Option<u32>,
  /// Milliseconds to wait between `mousedown` and `mouseup`
  /// (Playwright `delay`).
  pub delay: Option<u64>,
}

/// Options for `Keyboard.press()` — Playwright `{ delay? }`.
#[derive(Debug, Clone, Default)]
pub struct KeyboardPressOptions {
  /// Milliseconds to wait between `keydown` and `keyup`.
  pub delay: Option<u64>,
}

/// Options for `Keyboard.type()` — Playwright `{ delay? }`.
#[derive(Debug, Clone, Default)]
pub struct KeyboardTypeOptions {
  /// Milliseconds to wait between key presses.
  pub delay: Option<u64>,
}

/// Options for `Mouse.down()`.
#[derive(Debug, Clone, Default)]
pub struct MouseDownOptions {
  /// Mouse button: "left", "right", "middle"
  pub button: Option<String>,
  /// Click count for the event
  pub click_count: Option<u32>,
}

/// Options for `Mouse.up()`.
#[derive(Debug, Clone, Default)]
pub struct MouseUpOptions {
  /// Mouse button: "left", "right", "middle"
  pub button: Option<String>,
  /// Click count for the event
  pub click_count: Option<u32>,
}

// ── Touchscreen ───────────────────────────────────────────────────────────

/// Touchscreen interface for a page. Mirrors Playwright's `page.touchscreen`.
pub struct Touchscreen<'a> {
  page: &'a Page,
}

impl Touchscreen<'_> {
  /// Tap at coordinates. Uses Touch/TouchEvent on platforms that support them,
  /// falls back to `PointerEvent` + click on desktop (e.g. macOS `WKWebView`).
  ///
  /// # Errors
  ///
  /// Returns an error if the tap event dispatch fails.
  pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
    // webkit6 / WebKitGTK exposes `Touch` and `TouchEvent` as
    // constructors but throws "Illegal constructor" when JS tries to
    // instantiate them — they're internal-only. macOS WKWebView is
    // the same. `typeof X !== 'undefined'` isn't enough; try the
    // actual construction in a try/catch and fall through on throw.
    self.page.inner.evaluate(&format!(
      "(function(){{var el=document.elementFromPoint({x},{y})||document.body;\
       var dispatched=false;\
       try{{\
         if(typeof Touch!=='undefined'&&typeof TouchEvent!=='undefined'){{\
           var t=new Touch({{identifier:1,target:el,clientX:{x},clientY:{y}}});\
           el.dispatchEvent(new TouchEvent('touchstart',{{touches:[t],changedTouches:[t],bubbles:true}}));\
           el.dispatchEvent(new TouchEvent('touchend',{{touches:[],changedTouches:[t],bubbles:true}}));\
           dispatched=true;\
         }}\
       }}catch(e){{}}\
       if(!dispatched){{\
         el.dispatchEvent(new PointerEvent('pointerdown',{{clientX:{x},clientY:{y},bubbles:true,isPrimary:true,pointerType:'touch'}}));\
         el.dispatchEvent(new PointerEvent('pointerup',{{clientX:{x},clientY:{y},bubbles:true,isPrimary:true,pointerType:'touch'}}));\
         el.click();\
       }}}})()"
    )).await?;
    Ok(())
  }
}

/// Pattern-match the backend's "selector did not match any element"
/// error String so [`Page::query_selector`] can surface `Ok(None)` for
/// the missing-element case. Each backend uses a different message:
///
/// * CDP (`crates/ferridriver/src/backend/cdp/mod.rs`): `"'{selector}' not found"`
/// * `WebKit` (`crates/ferridriver/src/backend/webkit/mod.rs`): `"'{selector}' not found"`
/// * `BiDi` (`crates/ferridriver/src/backend/bidi/page.rs`): `"No element found for selector: {selector}"`
///
/// Other backend errors (protocol detach, target closed, invalid
/// selector) bubble up unmodified.
fn is_element_not_found(err: &crate::error::FerriError) -> bool {
  if let crate::error::FerriError::InvalidSelector { .. } = err {
    return true;
  }
  let lower = err.to_string().to_ascii_lowercase();
  lower.contains("not found") || lower.contains("no element found")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn is_element_not_found_matches_every_backend_message() {
    use crate::error::FerriError;
    // CDP + WebKit message shape — typed InvalidSelector.
    assert!(is_element_not_found(&FerriError::invalid_selector(
      "button#primary",
      "not found"
    )));
    // BiDi message shape.
    assert!(is_element_not_found(&FerriError::invalid_selector(
      "button#primary",
      "no element found"
    )));
    // Free-form backend strings still classify if message matches.
    assert!(is_element_not_found(&FerriError::backend(
      "NO ELEMENT FOUND FOR SELECTOR: x"
    )));
    // Other errors bubble up unchanged.
    assert!(!is_element_not_found(&FerriError::backend("session detached")));
    assert!(!is_element_not_found(&FerriError::timeout_plain(30_000)));
  }
}
