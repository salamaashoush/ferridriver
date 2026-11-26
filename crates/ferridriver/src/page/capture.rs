//! Page screenshots, PDF, accessibility snapshot, viewport, emulation, tracing, and storage state.

use std::sync::Arc;

use crate::backend::CookieData;
use crate::error::Result;
use crate::locator::Locator;
use crate::options::ScreenshotOptions;
use crate::snapshot;

use super::{Page, ScreenshotTurn, aria_snapshot_params};

impl Page {
  // ── Screenshots ─────────────────────────────────────────────────────────

  /// Take a screenshot of the page. Mirrors Playwright's
  /// `page.screenshot(options?: PageScreenshotOptions)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280`.
  ///
  /// The `screenshotter` module does the capture; this writes `path` and
  /// bounds the whole of it by `timeout`.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] if the capture
  /// exceeds `opts.timeout` milliseconds; otherwise propagates any
  /// backend-specific failure.
  #[track_caller]
  pub fn screenshot(&self) -> crate::action::Action<'_, ScreenshotOptions, Vec<u8>> {
    let page = self;
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .traced("screenshot", serde_json::json!({}), page.screenshot_impl(opts))
          .await
      })
    })
  }

  /// Inject the selector engine into this page if it is not already
  /// there.
  ///
  /// Public because a caller that installs the DOM-side half of a
  /// capture itself — the screenshot MATCHER, which wraps an element
  /// capture in the page's own mask and style — has to guarantee the
  /// same precondition `screenshot()` guarantees.
  ///
  /// # Errors
  ///
  /// Returns an error if the injection evaluate fails.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    self.inner.ensure_engine_injected().await
  }

  /// This page's turn to be captured: one screenshot of a page at a
  /// time. In a browser that draws only the tab in front it is also one
  /// page at a time, and the page is brought to the front for it.
  pub(crate) async fn screenshot_turn(&self) -> Result<ScreenshotTurn<'_>> {
    let queue = self.screenshot_queue.lock().await;
    let front = match self.context() {
      Some(context) => context.front_tab_turn().await,
      None => None,
    };
    let front = match front {
      Some(turn) => {
        let held = turn.lock_owned().await;
        self.inner.bring_to_front_drawn().await?;
        Some(held)
      },
      None => None,
    };
    Ok(ScreenshotTurn {
      _queue: queue,
      _front: front,
    })
  }

  /// Implementation of [`Self::screenshot`].
  pub(crate) async fn screenshot_impl(&self, opts: ScreenshotOptions) -> Result<Vec<u8>> {
    // Masks resolve through the injected selector engine.
    if !opts.mask.is_empty() {
      self.inner.ensure_engine_injected().await?;
    }
    let timeout = opts.timeout.unwrap_or_else(|| self.default_timeout());
    let bytes = crate::operation_budget::scoped(
      timeout,
      || "screenshot".into(),
      Box::pin(crate::screenshotter::screenshot_page(self, &opts)),
    )
    .await?;
    if let Some(ref path) = opts.path {
      if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
      {
        let _ = tokio::fs::create_dir_all(parent).await;
      }
      tokio::fs::write(path, &bytes)
        .await
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
    self.locator(selector).screenshot().await
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
  #[track_caller]
  pub fn pdf(&self) -> crate::action::Action<'_, crate::options::PdfOptions, Vec<u8>> {
    let page = self;
    crate::action::Action::new(move |opts| {
      Box::pin(async move { page.traced("pdf", serde_json::json!({}), page.pdf_impl(opts)).await })
    })
  }

  /// Implementation of [`Self::pdf`].
  pub(crate) async fn pdf_impl(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    let path = opts.path.clone();
    let bytes = self.inner.pdf(opts).await?;
    if let Some(path) = path {
      if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
      {
        tokio::fs::create_dir_all(parent).await?;
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
  #[track_caller]
  pub fn snapshot_for_ai(&self) -> crate::action::Action<'_, snapshot::SnapshotOptions, snapshot::SnapshotForAI> {
    let page = self;
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .traced("snapshotForAI", serde_json::json!({}), page.snapshot_for_ai_impl(opts))
          .await
      })
    })
  }

  /// Implementation of [`Self::snapshot_for_ai`].
  pub(crate) async fn snapshot_for_ai_impl(&self, opts: snapshot::SnapshotOptions) -> Result<snapshot::SnapshotForAI> {
    let mut tracker = self.snapshot_tracker.lock().await;
    Box::pin(snapshot::build_snapshot_for_ai(&self.inner, &opts, &mut tracker)).await
  }

  /// Playwright: `page.ariaSnapshot(options?: TimeoutOptions & { mode?: 'ai' |
  /// 'default', depth?: number, boxes?: boolean }): Promise<string>` — the
  /// main frame's `body` (or `frameset`) as YAML, with the iframes an AI
  /// snapshot gives refs to merged in.
  ///
  /// # Errors
  ///
  /// Returns an error if the document has no body within the timeout or
  /// the snapshot cannot be built.
  #[track_caller]
  pub fn aria_snapshot(self: &Arc<Self>) -> crate::action::Action<'_, crate::options::AriaSnapshotOptions, String> {
    let page = self;
    crate::action::Action::new(move |opts: crate::options::AriaSnapshotOptions| {
      Box::pin(async move {
        let params = aria_snapshot_params(&opts);
        page
          .traced(
            "ariaSnapshot",
            params,
            page.aria_snapshot_root().aria_snapshot_impl(opts),
          )
          .await
      })
    })
  }

  /// Playwright: `page.ariaSnapshotJSON(options?): Promise<AriaSnapshotJSON>`,
  /// the tree [`Self::aria_snapshot`] renders.
  ///
  /// # Errors
  ///
  /// As [`Self::aria_snapshot`].
  #[track_caller]
  pub fn aria_snapshot_json(
    self: &Arc<Self>,
  ) -> crate::action::Action<'_, crate::options::AriaSnapshotOptions, Vec<serde_json::Value>> {
    let page = self;
    crate::action::Action::new(move |opts: crate::options::AriaSnapshotOptions| {
      Box::pin(async move {
        let params = aria_snapshot_params(&opts);
        page
          .traced(
            "ariaSnapshotJSON",
            params,
            page.aria_snapshot_root().aria_snapshot_json_impl(opts),
          )
          .await
      })
    })
  }

  /// Playwright's `body,frameset`, non-strict: a frameset document has
  /// several.
  fn aria_snapshot_root(self: &Arc<Self>) -> Locator {
    self.locator("body,frameset").first()
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  /// Set the viewport size by width and height.
  ///
  /// # Errors
  ///
  /// Returns an error if the viewport emulation fails.
  pub async fn set_viewport_size(&self, width: i64, height: i64) -> Result<()> {
    self
      .traced(
        "setViewportSize",
        serde_json::json!({ "viewportSize": { "width": width, "height": height } }),
        async {
          self
            .inner
            .emulate_viewport(&crate::options::ViewportConfig {
              width,
              height,
              ..Default::default()
            })
            .await?;
          *self.viewport.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((width, height));
          Ok(())
        },
      )
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
  #[track_caller]
  pub fn drag_and_drop(
    self: &Arc<Self>,
    source_selector: &str,
    target_selector: &str,
  ) -> crate::action::Action<'static, crate::options::DragAndDropOptions, ()> {
    let page = Arc::clone(self);
    let source_selector = source_selector.to_string();
    let target_selector = target_selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { Box::pin(page.drag_and_drop_impl(&source_selector, &target_selector, Some(opts))).await })
    })
  }

  /// Implementation of [`Self::drag_and_drop`].
  pub(crate) async fn drag_and_drop_impl(
    self: &Arc<Self>,
    source_selector: &str,
    target_selector: &str,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let source = self.locator(source_selector);
    let target = self.locator(target_selector);
    let (source, target) = match opts.strict {
      Some(s) => (source.strict(s), target.strict(s)),
      None => (source, target),
    };
    source.drag_to_impl(&target, Some(opts)).await
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
    self.inner.press_key(key, 0).await
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
    // Purely host-side: no protocol command carries it, the selector
    // methods just read it when they build their locator.
    if let Some(strict) = opts.strict_selectors {
      self.set_strict_selectors(strict);
    }
    // Track the emulated viewport for the sync `viewport_size` accessor
    // (`viewport: null` opts out — the accessor then reports None).
    *self.viewport.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
      opts.resolved_viewport().map(|vp| (vp.width, vp.height));
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
  #[track_caller]
  pub fn emulate_media(&self) -> crate::action::Action<'_, crate::options::EmulateMediaOptions, ()> {
    let page = self;
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .traced("emulateMedia", serde_json::json!({}), page.emulate_media_impl(&opts))
          .await
      })
    })
  }

  /// Implementation of [`Self::emulate_media`]. Each call is a partial
  /// update: `{colorScheme: 'dark'}` then `{media: 'print'}` keeps both.
  pub(crate) async fn emulate_media_impl(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    self.inner.emulate_media(opts).await
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
    self
      .traced(
        "setExtraHTTPHeaders",
        serde_json::json!({ "headers": headers.keys().collect::<Vec<_>>() }),
        self.inner.set_extra_http_headers(headers),
      )
      .await
  }

  /// Set (or clear) the HTTP credentials answered to auth challenges.
  /// Backs [`crate::ContextRef::set_http_credentials`]
  /// (Playwright `browserContext.setHTTPCredentials(creds | null)`).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend cannot apply the credentials.
  pub async fn set_http_credentials(&self, creds: Option<Vec<crate::options::HttpCredentials>>) -> Result<()> {
    self.inner.set_http_credentials(creds).await
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  /// Start a Chrome performance trace. `None` records
  /// [`crate::trace_categories::DEFAULT`], the set `DevTools` and
  /// Lighthouse use.
  ///
  /// Chromium only.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Unsupported`] on the `BiDi` and `WebKit`
  /// backends, whose protocols have no `Tracing` domain.
  pub async fn start_tracing(&self, categories: Option<&[String]>) -> Result<()> {
    self.inner.start_tracing(categories).await
  }

  /// End the trace and return every event recorded, in wire order.
  ///
  /// Feed the result to `ferridriver-perf` to turn it into metrics and
  /// insights.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Unsupported`] on the `BiDi` and `WebKit` backends.
  pub async fn stop_tracing(&self) -> Result<Vec<serde_json::Value>> {
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
    self.restore_cookies(state).await?;
    if let Some(origins) = state.get("origins").and_then(|v| v.as_array()) {
      for origin_entry in origins {
        let origin = origin_entry.get("origin").and_then(|v| v.as_str()).unwrap_or("");
        if origin.is_empty() {
          continue;
        }
        self.restore_origin_storage(origin, Some(origin_entry)).await?;
      }
    }
    Ok(())
  }

  /// Add the cookies of a storage state to this page's context.
  pub(crate) async fn restore_cookies(&self, state: &serde_json::Value) -> Result<()> {
    let Some(cookies) = state.get("cookies").and_then(|v| v.as_array()) else {
      return Ok(());
    };
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
    Ok(())
  }

  /// Put this page on an empty document of `origin`, which is what
  /// reading or writing that origin's storage takes. The document is
  /// served here: a network error page has an opaque origin, and the
  /// origin's own server need not answer at its root.
  pub(crate) async fn goto_blank_origin(&self, origin: &str) -> Result<()> {
    let current_origin = self
      .inner
      .evaluate("location.origin")
      .await
      .ok()
      .flatten()
      .and_then(|v| v.as_str().map(str::to_string))
      .unwrap_or_default();
    if current_origin == origin {
      return Ok(());
    }
    let matcher = crate::url_matcher::UrlMatcher::glob(format!("{origin}/**"))?;
    let handler: crate::route::RouteHandler = Arc::new(|route| {
      route.fulfill(crate::route::FulfillResponse {
        status: 200,
        content_type: Some("text/html".into()),
        body: b"<!doctype html><html></html>".to_vec(),
        ..Default::default()
      });
    });
    let handler_id = crate::route::route_handler_id(&handler);
    match self
      .inner
      .route(crate::route::RegisteredRoute::new(matcher.clone(), handler, Some(1)))
      .await
    {
      Ok(()) => {},
      // Classic WebDriver cannot answer a request itself, so the document
      // of the origin has to be the origin's own.
      Err(error) if error.is_unsupported() => {
        return self
          .inner
          .goto(origin, crate::backend::NavLifecycle::Load, 10_000, None)
          .await
          .map(|_| ());
      },
      Err(error) => return Err(error),
    }
    let navigation = self
      .inner
      .goto(origin, crate::backend::NavLifecycle::Load, 10_000, None)
      .await;
    let cleanup = self
      .inner
      .unroute(&matcher, crate::route::RouteScope::Page, Some(handler_id))
      .await;
    navigation?;
    cleanup
  }

  /// Run Playwright's `StorageScript.restore` for `origin` from a
  /// document of that origin: it empties the origin's storage and puts
  /// back the `localStorage`, `IndexedDB` and private file system of
  /// `entry`, or nothing when the state has no entry for the origin.
  pub(crate) async fn restore_origin_storage(&self, origin: &str, entry: Option<&serde_json::Value>) -> Result<()> {
    self.goto_blank_origin(origin).await?;
    let entry = entry.map_or_else(|| String::from("undefined"), ToString::to_string);
    let restore =
      crate::selectors::storage_script_call(self.inner.kind(), &format!("script.restore({entry}).then(() => true)"));
    self.inner.evaluate(&restore).await?;
    Ok(())
  }
}
