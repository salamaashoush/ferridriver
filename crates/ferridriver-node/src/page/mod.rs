//! Page class -- NAPI binding for `ferridriver::Page`.

mod events;
mod input;
mod network;

pub(crate) use events::JsUrl;
pub(crate) use events::{MatcherSpec, PredReturn, PredRoute, UrlArg, resolve_pred, url_condition};
pub(crate) use input::{Keyboard, Mouse, ScreencastFrame, SnapshotForAiResult, Touchscreen};
pub(crate) use network::{RouteFromHarOptionsJs, UnrouteAllOptions};
pub(crate) use network::{parse_har_options, parse_unroute_behavior};

use crate::abort::Bag;
use crate::error::IntoNapi;
use crate::types::{
  AccessibilityAuditOptions, AccessibilityReport, GotoOptions, MetricData, PageQualityOptions, PageQualityReport,
  PageToolGroup, ScreenshotOptions, SnapshotForAiOptions,
};
use std::sync::{Arc, Mutex};

use ferridriver::protocol::SerializedArgument;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// Lower an optional [`crate::types::NapiEvaluateArg`] into a
/// [`SerializedArgument`] for a call without `exposeFunctions`. `None`
/// maps to the `undefined` sentinel so the utility script sees the user
/// function called with zero positional args.
pub(crate) fn build_serialized_argument(arg: Option<crate::types::NapiEvaluateArg>) -> Result<SerializedArgument> {
  build_evaluate_argument(arg, false)
}

/// [`build_serialized_argument`] for a call that takes `options`.
pub(crate) fn build_evaluate_argument(
  arg: Option<crate::types::NapiEvaluateArg>,
  expose_functions: bool,
) -> Result<SerializedArgument> {
  arg.map_or_else(
    || Ok(SerializedArgument::default()),
    |a| a.into_argument(expose_functions),
  )
}

#[napi]
pub struct Page {
  pub(super) inner: Arc<ferridriver::Page>,
  pub(super) mouse_position: Arc<Mutex<(f64, f64)>>,
  pub(super) predicate_routes: Arc<Mutex<Vec<PredRoute>>>,
  /// `page.on`/`page.once` registrations, kept so `off(event, listener)`
  /// can resolve the core `ListenerId` from the JS function identity.
  pub(super) listener_regs: crate::emitter::ListenerRegistry<events::PageWaitForEventResult>,
  /// `page.route` registrations, kept so `unroute(url, handler)` can
  /// resolve the core handler identity from the JS function identity.
  route_regs: Arc<Mutex<Vec<network::RouteReg>>>,
}

impl Page {
  pub(crate) fn wrap(inner: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      mouse_position: Arc::new(Mutex::new((0.0, 0.0))),
      predicate_routes: Arc::new(Mutex::new(Vec::new())),
      listener_regs: crate::emitter::ListenerRegistry::new(),
      route_regs: Arc::new(Mutex::new(Vec::new())),
    }
  }

  pub(crate) fn inner_arc(&self) -> Arc<ferridriver::Page> {
    Arc::clone(&self.inner)
  }
}

crate::macros::napi_locator_queries!(Page, optional_frame_locator);
crate::macros::napi_selector_surface!(Page);
crate::macros::napi_event_emitter!(
  target: Page,
  arg: events::PageWaitForEventResult,
  ts_on: "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'dialogclosed' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'crash' | 'pageerror' | 'popup' | 'worker', listener: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Worker | Error) => void",
  ts_off: "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'dialogclosed' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'crash' | 'pageerror' | 'popup' | 'worker', listener?: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Worker | Error) => void",
  full
);

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  reason = "napi-derive takes JS strings, functions and option bags only by value"
)]
impl Page {
  /// Playwright: `page.coverage`. Chromium only.
  #[napi(getter)]
  pub fn coverage(&self) -> crate::coverage::Coverage {
    crate::coverage::Coverage::wrap(self.inner.coverage())
  }

  #[napi(getter)]
  pub fn webmcp(&self) -> crate::web_mcp::WebMcp {
    crate::web_mcp::WebMcp::wrap(self.inner.webmcp())
  }

  /// Playwright: `page.clock` — the owning context's fake-time
  /// controller (`page.clock` IS `context.clock`).
  #[napi(getter)]
  pub fn clock(&self) -> Result<crate::clock::Clock> {
    let ctx = self
      .inner
      .context()
      .cloned()
      .ok_or_else(|| napi::Error::from_reason("page has no associated browser context"))?;
    Ok(crate::clock::Clock::wrap(ctx))
  }

  /// Playwright: `page.opener(): Promise<null | Page>` — the page that
  /// opened this popup via `window.open` / `target=_blank`; `null` for
  /// non-popup pages and once the opener has closed. The lookup is a
  /// synchronous weak upgrade; the yield keeps the promise-returning
  /// Playwright signature without an unused-async lint.
  #[napi(ts_return_type = "Promise<Page | null>")]
  pub async fn opener(&self) -> Option<crate::identity::PageObject> {
    tokio::task::yield_now().await;
    self.inner.opener().map(crate::identity::PageObject)
  }

  #[napi(js_name = "context")]
  pub fn context(&self) -> Result<crate::context::BrowserContext> {
    let ctx = self
      .inner
      .context()
      .cloned()
      .ok_or_else(|| napi::Error::from_reason("page has no associated browser context"))?;
    Ok(crate::context::BrowserContext::wrap(ctx))
  }

  /// Playwright: `page.request` — the owning context's context-bound
  /// API request client (`page.request` IS `context.request`). Cookies
  /// flow both ways between these requests and the browser context.
  #[napi(getter)]
  pub fn request(&self) -> Result<crate::http_client::HttpClient> {
    let client = self
      .inner
      .http_client()
      .ok_or_else(|| napi::Error::from_reason("page has no associated browser context"))?;
    Ok(crate::http_client::HttpClient::wrap(client))
  }

  #[napi(getter)]
  pub fn keyboard(&self) -> Keyboard {
    Keyboard {
      page: Arc::clone(&self.inner),
    }
  }

  #[napi(getter)]
  pub fn mouse(&self) -> Mouse {
    Mouse {
      page: Arc::clone(&self.inner),
      position: Arc::clone(&self.mouse_position),
    }
  }

  /// Playwright: `page.touchscreen: Touchscreen` — exposes
  /// virtual-touch dispatch independent of `Mouse` / `Keyboard`. On
  /// CDP backends the touch event is delivered via
  /// `Input.dispatchTouchEvent`; on backends without touch, falls
  /// back to a synthesized `PointerEvent`.
  #[napi(getter)]
  pub fn touchscreen(&self) -> Touchscreen {
    Touchscreen {
      page: Arc::clone(&self.inner),
    }
  }

  /// Playwright: `page.getByRef(ref: string): Locator`.
  #[napi(js_name = "getByRef", ts_args_type = "ref: string")]
  pub fn get_by_ref(&self, aria_ref: String) -> crate::locator::Locator {
    crate::locator::Locator::wrap(self.inner.get_by_ref(&aria_ref))
  }

  /// Set the default timeout for all operations (milliseconds).
  #[napi]
  pub fn set_default_timeout(&self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  /// Set the default timeout for navigation-family operations
  /// (`goto`, `reload`, `goBack`, `goForward`, `waitForURL`). Mirrors
  /// Playwright's `page.setDefaultNavigationTimeout(timeout)` — distinct
  /// from `setDefaultTimeout`, which applies to non-navigation actions.
  /// `0` = no timeout.
  #[napi]
  pub fn set_default_navigation_timeout(&self, ms: f64) {
    self.inner.set_default_navigation_timeout(crate::types::f64_to_u64(ms));
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  /// Navigate to `url`. Returns the main-document `Response`, or `null`
  /// for same-document navigations (no new main-document request was
  /// issued). Mirrors Playwright's `Promise<Response | null>`.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn goto(&self, url: String, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let resp = self
      .inner
      .goto(&url)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, Arc::clone(&self.inner))))
  }

  /// Navigate back in history. Returns the main-document `Response`
  /// on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn go_back(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let resp = self
      .inner
      .go_back()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, Arc::clone(&self.inner))))
  }

  /// Navigate forward in history. Returns the main-document
  /// `Response` on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn go_forward(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let resp = self
      .inner
      .go_forward()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, Arc::clone(&self.inner))))
  }

  /// Reload the current page. Returns the main-document `Response`
  /// on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn reload(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let resp = self.inner.reload().bag(options)?.await.map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, Arc::clone(&self.inner))))
  }

  /// Playwright: `page.url(): string` — synchronous.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// Playwright: `page.video(): null | Video` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:4756`.
  /// Returns a live [`crate::video::Video`] handle when the owning
  /// context was created with `recordVideo`, or `null` otherwise. If a
  /// backend cannot produce the recording, a handle is still returned
  /// but its `path()` / `saveAs()` / `delete()` reject with a typed
  /// error explaining the reason.
  #[napi(ts_return_type = "Video | null")]
  pub fn video(&self) -> Option<crate::video::Video> {
    self.inner.video().map(crate::video::Video::from_core)
  }

  // ── Element handles ─────────────────────────────────────────────────────

  /// Playwright: `page.querySelector(selector): Promise<ElementHandle | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts`).
  #[napi]
  pub async fn query_selector(
    &self,
    selector: String,
    options: Option<crate::types::QuerySelectorOptions>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    self.dollar(selector, options).await
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  #[napi]
  pub async fn query_selector_all(&self, selector: String) -> Result<Vec<crate::element_handle::ElementHandle>> {
    self.dollar_dollar(selector).await
  }

  // ── Frames (sync, Playwright parity — task 3.8) ─────────────────────

  /// Main frame of this page. Playwright: `page.mainFrame(): Frame`
  /// (non-null). The frame cache is seeded inside `Page::new` /
  /// `Page::with_context` before the Page is handed out.
  #[napi(ts_return_type = "Frame")]
  pub fn main_frame(&self) -> crate::identity::FrameObject {
    crate::identity::FrameObject(self.inner.main_frame())
  }

  /// All frames in the page (main frame + all iframes).
  /// Playwright: `page.frames(): Frame[]` (sync).
  #[napi(ts_return_type = "Frame[]")]
  pub fn frames(&self) -> Vec<crate::identity::FrameObject> {
    self
      .inner
      .frames()
      .into_iter()
      .map(crate::identity::FrameObject)
      .collect()
  }

  /// Find a frame by name or URL. Mirrors Playwright's
  /// `page.frame(string | { name?, url? }): Frame | null` (sync).
  /// The URL field is an exact-match string for now; task 3.12 extends
  /// it to the full `string | RegExp` union.
  #[napi(
    ts_args_type = "selector: string | { name?: string | null | undefined; url?: string | null | undefined }",
    ts_return_type = "Frame | null"
  )]
  pub fn frame(&self, selector: crate::types::FrameSelectorArg) -> Option<crate::identity::FrameObject> {
    let core_sel: ferridriver::options::FrameSelector = match selector {
      napi::Either::A(name) => ferridriver::options::FrameSelector::by_name(name),
      napi::Either::B(bag) => bag.into(),
    };
    if core_sel.is_empty() {
      return None;
    }
    self.inner.frame(core_sel).map(crate::identity::FrameObject)
  }

  /// Type `text` into the first element matching `selector`. Accepts
  /// Playwright's full `PageTypeOptions` bag.
  #[napi]
  pub async fn type_text(
    &self,
    selector: String,
    text: String,
    options: Option<crate::types::TypeOptions>,
  ) -> Result<()> {
    self
      .inner
      .r#type(&selector, &text)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn markdown(&self) -> Result<String> {
    self.inner.markdown().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `page.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`.
  #[napi(
    ts_args_type = "options?: { timeout?: number, signal?: AbortSignal, url?: string | RegExp, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }",
    ts_return_type = "Promise<Response | null>"
  )]
  pub fn wait_for_navigation(
    &self,
    env: napi::Env,
    options: Option<crate::types::WaitForNavigationOptions>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<Option<crate::network::Response>>> {
    // Listening from the call: the navigation is usually started before
    // the promise is awaited.
    let navigation = self.inner.wait_for_navigation().bag(options);
    let page = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let resp = navigation?.await.map_err(crate::error::to_napi)?;
      Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, page)))
    })
    .build(&env)
  }

  // ── Accessibility ───────────────────────────────────────────────────────

  /// Audit the page with axe-core, covering what Lighthouse's whole
  /// accessibility category covers.
  ///
  /// axe-core is fetched by `ferridriver install axe` rather than
  /// bundled, so this throws until it has been.
  #[napi]
  pub async fn check_accessibility(&self, options: Option<AccessibilityAuditOptions>) -> Result<AccessibilityReport> {
    let report = self
      .inner
      .check_accessibility()
      .maybe_options(options.map(Into::into))
      .await
      .into_napi()?;
    Ok(report.into())
  }

  /// The tools this page offers about itself, from its own answer to a
  /// `devtoolstooldiscovery` event. Empty where it offers none.
  ///
  /// Not a protocol call, so this answers the same on every backend.
  #[napi]
  pub async fn developer_tools(&self) -> Result<Vec<PageToolGroup>> {
    Ok(
      self
        .inner
        .developer_tools()
        .await
        .into_napi()?
        .into_iter()
        .map(Into::into)
        .collect(),
    )
  }

  /// Call one of them. `params` has to satisfy that tool's own
  /// `inputSchema`; what comes back is whatever it returned, with the
  /// values JSON cannot carry replaced.
  #[napi(
    ts_args_type = "name: string, params?: Record<string, unknown>",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn execute_developer_tool(
    &self,
    name: String,
    params: Option<serde_json::Value>,
  ) -> Result<serde_json::Value> {
    self.inner.execute_developer_tool(&name, params).await.into_napi()
  }

  /// Capture a V8 heap snapshot of this page, garbage collected first,
  /// and hand back a handle with the queries on it.
  ///
  /// Chromium-only: the format is V8's, and neither `JavaScriptCore` nor
  /// `SpiderMonkey` writes anything a reader of it could open, so this
  /// throws `Unsupported` on the `webkit` and `bidi` backends.
  #[napi]
  pub async fn take_heap_snapshot(&self) -> Result<crate::heap::HeapSnapshot> {
    let snapshot = self.inner.take_heap_snapshot().await.into_napi()?;
    Ok(crate::heap::HeapSnapshot::new(snapshot))
  }

  /// Audit the page against the ten Lighthouse checks that score a
  /// live page: `doctype`, `meta-description`, `canonical`,
  /// `crawlable-anchors`, `link-text`, `image-aspect-ratio`,
  /// `image-size-responsive`, `paste-preventing-inputs`,
  /// `http-status-code` and `is-crawlable`.
  ///
  /// The last two read the main document's own response rather than the
  /// DOM, and are absent from the report for a document that arrived
  /// without one.
  #[napi]
  pub async fn check_page_quality(&self, options: Option<PageQualityOptions>) -> Result<PageQualityReport> {
    let report = self
      .inner
      .check_page_quality()
      .maybe_options(options.map(Into::into))
      .await
      .into_napi()?;
    Ok(report.into())
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self, mut options: Option<ScreenshotOptions>) -> Result<Buffer> {
    let signal = crate::abort::take(&mut options);
    let mask_selectors: Vec<String> = options
      .as_ref()
      .and_then(|o| o.mask.as_ref())
      .map(|m| m.iter().map(|l| l.selector.clone()).collect())
      .unwrap_or_default();
    let mut opts: ferridriver::options::ScreenshotOptions = options.map_or_else(Default::default, Into::into);
    opts.mask = mask_selectors.into_iter().map(|sel| self.inner.locator(&sel)).collect();
    let bytes = self
      .inner
      .screenshot()
      .options(opts)
      .maybe_signal(signal)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(bytes.into())
  }

  #[napi]
  pub async fn screenshot_element(&self, selector: String) -> Result<Buffer> {
    let bytes = self
      .inner
      .screenshot_element(&selector)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(bytes.into())
  }

  /// Generate a PDF of the page (Chrome-family backends only).
  /// Playwright API: `page.pdf(options?)` — accepts the full `PDFOptions`
  /// shape (`format`, `path`, `scale`, `width`/`height` as `string|number`,
  /// `margin`, `headerTemplate`, `footerTemplate`, `pageRanges`, etc.).
  #[napi]
  pub async fn pdf(&self, options: Option<crate::types::PdfOptions>) -> Result<Buffer> {
    let rust_opts: ferridriver::options::PdfOptions = options.unwrap_or_default().try_into()?;
    let bytes = self
      .inner
      .pdf()
      .options(rust_opts)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(bytes.into())
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  /// Playwright: `page.setViewportSize({ width, height })` — a single
  /// object, not two positional numbers.
  #[napi]
  pub async fn set_viewport_size(&self, size: crate::context::NapiViewportSize) -> Result<()> {
    self
      .inner
      .set_viewport_size(
        crate::types::f64_to_i64(size.width),
        crate::types::f64_to_i64(size.height),
      )
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Input devices ───────────────────────────────────────────────────────

  #[napi]
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.inner.click_at(x, y).await.map_err(crate::error::to_napi)?;
    *self
      .mouse_position
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  /// Click at coordinates with specific button and click count.
  /// button: "left", "right", "middle", "back", "forward"
  #[napi]
  pub async fn click_at_opts(&self, x: f64, y: f64, button: String, click_count: Option<i32>) -> Result<()> {
    let count = u32::try_from(click_count.unwrap_or(1))
      .map_err(|_| napi::Error::from_reason("click_count must be non-negative"))?;
    self
      .inner
      .click_at_opts(x, y, &button, count)
      .await
      .map_err(crate::error::to_napi)?;
    *self
      .mouse_position
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  /// Move mouse to coordinates without clicking.
  #[napi]
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self.inner.mouse().r#move(x, y).await.map_err(crate::error::to_napi)?;
    *self
      .mouse_position
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  /// Move mouse smoothly with bezier easing.
  #[napi]
  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: Option<i32>,
  ) -> Result<()> {
    let step_count =
      u32::try_from(steps.unwrap_or(10)).map_err(|_| napi::Error::from_reason("steps must be non-negative"))?;
    self
      .inner
      .move_mouse_smooth(from_x, from_y, to_x, to_y, step_count)
      .await
      .map_err(crate::error::to_napi)?;
    *self
      .mouse_position
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = (to_x, to_y);
    Ok(())
  }

  #[napi]
  pub async fn type_str(&self, text: String) -> Result<()> {
    self.inner.keyboard().r#type(&text).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn press_key(&self, key: String) -> Result<()> {
    self.inner.keyboard().press(&key).await.map_err(crate::error::to_napi)
  }

  // ── Emulation ───────────────────────────────────────────────────────────
  //
  // Non-Playwright page-level emulation setters (userAgent, locale,
  // timezone, geolocation, offline, javaScriptEnabled) are
  // intentionally NOT exposed here — they live on `BrowserContext`
  // in Playwright's JS API. Use
  // `browser.newContext({ userAgent, locale, ... })` or the
  // context-level mutators (`context.setGeolocation`,
  // `context.setOffline`, `context.setExtraHTTPHeaders`).

  /// Emulate media features. Mirrors Playwright's
  /// `page.emulateMedia(options?: { media, colorScheme, reducedMotion, forcedColors, contrast })`
  /// per `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`.
  ///
  /// Every field accepts the enum values documented by Playwright, plus
  /// `null` to disable that specific emulation (mirrored in the JS binding
  /// via the option being absent or explicitly `null`).
  #[napi]
  pub async fn emulate_media(&self, options: Option<crate::types::EmulateMediaOptions>) -> Result<()> {
    let opts = options.map(ferridriver::options::EmulateMediaOptions::from);
    self
      .inner
      .emulate_media()
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  /// Start a Chrome performance trace. Omit `categories` to record the
  /// set `DevTools` uses. Chromium only.
  #[napi(ts_args_type = "categories?: Array<string>")]
  pub async fn start_tracing(&self, categories: Option<Vec<String>>) -> Result<()> {
    self
      .inner
      .start_tracing(categories.as_deref())
      .await
      .map_err(crate::error::to_napi)
  }

  /// End the trace and return every event recorded.
  #[napi(ts_return_type = "Promise<Array<any>>")]
  pub async fn stop_tracing(&self) -> Result<Vec<serde_json::Value>> {
    self.inner.stop_tracing().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    let metrics = self.inner.metrics().await.map_err(crate::error::to_napi)?;
    Ok(metrics.iter().map(MetricData::from).collect())
  }
}

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  clippy::unused_async,
  clippy::unused_async_trait_impl,
  reason = "napi-derive takes JS strings, functions and option bags only by value, and turns only an async fn into the Promise the Playwright signature returns"
)]
impl Page {
  // ── Misc ────────────────────────────────────────────────────────────────

  #[napi]
  pub async fn bring_to_front(&self) -> Result<()> {
    self.inner.bring_to_front().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `page.requestGC(): Promise<void>`. Forces a
  /// garbage-collection pass in the page's JS engine.
  #[napi(js_name = "requestGC")]
  pub async fn request_gc(&self) -> Result<()> {
    self.inner.request_gc().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `page.requests(): Promise<Request[]>`.
  #[napi]
  pub async fn requests(&self) -> Vec<crate::network::Request> {
    self
      .inner
      .requests()
      .into_iter()
      .map(|request| crate::network::Request::from_core_with_page(request, Arc::clone(&self.inner)))
      .collect()
  }

  /// Playwright: `page.workers(): Worker[]`.
  #[napi(ts_return_type = "Worker[]")]
  pub fn workers(&self) -> Vec<crate::identity::WorkerObject> {
    self
      .inner
      .workers()
      .into_iter()
      .map(crate::identity::WorkerObject)
      .collect()
  }

  /// Playwright: `page.consoleMessages(options?: { filter?: 'all' |
  /// 'since-navigation' }): Promise<ConsoleMessage[]>`. Defaults to
  /// `since-navigation`, i.e. only messages logged after the last
  /// main-frame navigation.
  #[napi]
  pub async fn console_messages(
    &self,
    options: Option<crate::types::ObservedFilterOptions>,
  ) -> Vec<crate::console_message::ConsoleMessage> {
    let filter = ferridriver::observed::ObservedFilter::parse(options.and_then(|o| o.filter).as_deref());
    self
      .inner
      .console_messages(filter)
      .into_iter()
      .map(crate::console_message::ConsoleMessage::from_core)
      .collect()
  }

  /// Playwright: `page.clearConsoleMessages(): Promise<void>`.
  #[napi]
  pub async fn clear_console_messages(&self) {
    self.inner.clear_console_messages();
  }

  /// Playwright: `page.pageErrors(options?: { filter?: 'all' |
  /// 'since-navigation' }): Promise<Error[]>`. Each entry materialises
  /// as a native JS `Error` (name / message / stack populated from the
  /// page-side exception).
  #[napi(ts_return_type = "Promise<Array<Error>>")]
  pub async fn page_errors(
    &self,
    options: Option<crate::types::ObservedFilterOptions>,
  ) -> Vec<crate::web_error::JsErrorValue> {
    let filter = ferridriver::observed::ObservedFilter::parse(options.and_then(|o| o.filter).as_deref());
    self
      .inner
      .page_errors(filter)
      .iter()
      .map(|e| crate::web_error::JsErrorValue::from_details(e.error()))
      .collect()
  }

  /// Playwright: `page.clearPageErrors(): Promise<void>`.
  #[napi]
  pub async fn clear_page_errors(&self) {
    self.inner.clear_page_errors();
  }

  /// Playwright: `page.localStorage: WebStorage` — the `localStorage`
  /// area for the page's current origin.
  #[napi(getter)]
  pub fn local_storage(&self) -> crate::web_storage::WebStorage {
    crate::web_storage::WebStorage::new(Arc::clone(&self.inner), ferridriver::options::WebStorageKind::Local)
  }

  /// Playwright: `page.sessionStorage: WebStorage` — the `sessionStorage`
  /// area for the page's current origin.
  #[napi(getter)]
  pub fn session_storage(&self) -> crate::web_storage::WebStorage {
    crate::web_storage::WebStorage::new(Arc::clone(&self.inner), ferridriver::options::WebStorageKind::Session)
  }

  /// Close the page. Accepts the Playwright-identical
  /// `{ runBeforeUnload?, reason? }` options shape.
  #[napi]
  pub async fn close(&self, options: Option<crate::types::PageCloseOptions>) -> Result<()> {
    let opts: Option<ferridriver::options::PageCloseOptions> = options.map(Into::into);
    self
      .inner
      .close()
      .maybe_options(opts)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  #[napi]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Missing methods (batch add) ────────────────────────────────────────

  /// Playwright: `page.viewportSize(): null | { width: number, height:
  /// number }` — a synchronous accessor over the tracked emulated
  /// viewport (`/tmp/playwright/packages/playwright-core/src/client/page.ts`);
  /// `null` when no viewport emulation is applied.
  #[napi(ts_return_type = "{ width: number, height: number } | null")]
  pub fn viewport_size(&self) -> Option<crate::context::NapiViewportSize> {
    self
      .inner
      .viewport_size()
      .map(|(w, h)| crate::context::NapiViewportSize {
        width: f64::from(i32::try_from(w).unwrap_or(i32::MAX)),
        height: f64::from(i32::try_from(h).unwrap_or(i32::MAX)),
      })
  }

  #[napi]
  pub async fn storage_state(&self) -> Result<serde_json::Value> {
    self.inner.storage_state().await.map_err(crate::error::to_napi)
  }

  /// Register a JS snippet to run on every new document (main frame and
  /// iframes) before any page script executes. Mirrors Playwright's
  /// `page.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  ///
  /// `script` is one of:
  /// - a `Function` — `.toString()`'d and wrapped as `(fn)(arg)` where `arg`
  ///   is `JSON.stringify`-serialised. `arg` defaults to `undefined`.
  /// - a `string` — used verbatim; passing `arg` rejects with
  ///   `"Cannot evaluate a string with arguments"`.
  /// - a `{ path?, content? }` object — `content` used verbatim, otherwise
  ///   `path` is read from disk; `arg` must be absent.
  ///
  /// All function/arg lowering lands in Rust core via
  /// [`ferridriver::options::evaluation_script`]; this method is a thin
  /// delegator.
  #[napi(
    ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any, options?: ExposeFunctionsOptions",
    ts_return_type = "Promise<Disposable>"
  )]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
    options: Option<crate::types::ExposeFunctionsOptions>,
  ) -> Result<crate::disposable::Disposable> {
    let disposable = if options.and_then(|o| o.expose_functions).unwrap_or(false) {
      self
        .inner
        .add_init_script_exposing_functions(script.into(), arg.into_exposed()?)
        .await
    } else {
      self.inner.add_init_script(script.into(), arg.into_json()).await
    }
    .map_err(crate::error::to_napi)?;
    Ok(crate::disposable::Disposable::wrap(disposable))
  }

  #[napi]
  pub async fn remove_init_script(&self, identifier: String) -> Result<()> {
    self
      .inner
      .remove_init_script(&identifier)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `page.exposeFunction(name, callback)`. Binds
  /// `window[name]` to a page-side proxy that calls `callback` with the
  /// spread call arguments and resolves to what it returns, or rejects
  /// with what it threw.
  #[napi(ts_args_type = "name: string, callback: Function", ts_return_type = "Promise<void>")]
  pub fn expose_function(
    &self,
    env: napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Unknown<'_>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let binding = crate::exposed::binding(env, callback, false)?;
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      inner
        .expose_binding(&name, binding)
        .await
        .map_err(crate::error::to_napi)
    })
    .build(&env)
  }

  /// Playwright: `page.exposeBinding(name, callback)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:371`).
  /// Like [`Self::expose_function`], but the callback first receives a
  /// `BindingSource` (the live `{ context, page, frame }`).
  #[napi(
    ts_args_type = "name: string, callback: (source: { context: BrowserContext, page: Page, frame: Frame }, ...args: any[]) => any",
    ts_return_type = "Promise<void>"
  )]
  pub fn expose_binding(
    &self,
    env: napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Unknown<'_>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let binding = crate::exposed::binding(env, callback, true)?;
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      inner
        .expose_binding(&name, binding)
        .await
        .map_err(crate::error::to_napi)
    })
    .build(&env)
  }

  /// Playwright: `page.pause(): Promise<void>` (`client/page.ts:858`).
  /// ferridriver has no Inspector/recorder UI, so this rejects with a
  /// typed `Unsupported` error rather than silently pretending to pause.
  #[napi]
  pub fn pause(&self) -> Result<()> {
    self.inner.pause().map_err(crate::error::to_napi)
  }

  /// ferridriver-specific (NOT Playwright). Playwright's public
  /// accessibility API is `ariaSnapshot` (returns a string); this
  /// richer structured shape backs the MCP server's incremental
  /// tracking.
  ///
  /// Returns `{ full, incremental?, refMap }`:
  /// - `full` — always present, the complete accessibility tree as
  ///   a YAML-ish string with `[ref=eN]` labels.
  /// - `incremental` — present only when the same `track` key is
  ///   reused; lists nodes that changed since the last call.
  /// - `refMap` — `{ "eN": backendNodeId }` so callers can map the
  ///   labels back to live DOM nodes.
  #[napi(
    js_name = "snapshotForAI",
    ts_args_type = "options?: { depth?: number, track?: string }",
    ts_return_type = "Promise<{ full: string; incremental?: string; refMap: Record<string, number> }>"
  )]
  pub async fn snapshot_for_ai(&self, options: Option<SnapshotForAiOptions>) -> Result<SnapshotForAiResult> {
    let opts = options.map(Into::into);
    let snap = self
      .inner
      .snapshot_for_ai()
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(SnapshotForAiResult {
      full: snap.full,
      incremental: snap.incremental,
      ref_map: snap.ref_map,
    })
  }

  /// Playwright: `page.ariaSnapshot(options?): Promise<string>`.
  #[napi(
    js_name = "ariaSnapshot",
    ts_args_type = "options?: { mode?: 'ai' | 'default', depth?: number, boxes?: boolean, timeout?: number, signal?: AbortSignal }",
    ts_return_type = "Promise<string>"
  )]
  pub async fn aria_snapshot(&self, options: Option<crate::types::AriaSnapshotOptions>) -> Result<String> {
    self
      .inner
      .aria_snapshot()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `page.ariaSnapshotJSON(options?): Promise<AriaSnapshotJSON>`.
  #[napi(
    js_name = "ariaSnapshotJSON",
    ts_args_type = "options?: { mode?: 'ai' | 'default', depth?: number, boxes?: boolean, timeout?: number, signal?: AbortSignal }",
    ts_return_type = "Promise<import('./types').AriaSnapshotJSON>"
  )]
  pub async fn aria_snapshot_json(
    &self,
    options: Option<crate::types::AriaSnapshotOptions>,
  ) -> Result<Vec<serde_json::Value>> {
    self
      .inner
      .aria_snapshot_json()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  /// ferridriver-specific (NOT Playwright): `startScreencast(quality,
  /// maxWidth, maxHeight, callback)`. Begins streaming JPEG frames; the
  /// callback fires for each frame with `{ frame: Buffer, timestamp:
  /// number }` (backed by CDP `Page.startScreencast`). Call
  /// `stopScreencast()` to halt.
  ///
  /// Backed on all backends: CDP-pipe / CDP-raw via
  /// `Page.startScreencast`, `BiDi` and `WebKit` via their respective
  /// screencast protocols.
  #[napi(
    ts_args_type = "quality: number, maxWidth: number, maxHeight: number, callback: (frame: { frame: Buffer; timestamp: number }) => void",
    ts_return_type = "Promise<void>"
  )]
  pub fn start_screencast(
    &self,
    env: napi::Env,
    quality: u32,
    max_width: u32,
    max_height: u32,
    callback: napi::bindgen_prelude::Function<'_, ScreencastFrame, ()>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let q = u8::try_from(quality).map_err(|_| napi::Error::from_reason("quality must fit in u8 (0-100)"))?;
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let mut rx = inner
        .start_screencast(q, max_width, max_height)
        .await
        .map_err(crate::error::to_napi)?;
      tokio::spawn(async move {
        while let Some((bytes, ts)) = rx.recv().await {
          let payload = ScreencastFrame {
            frame: napi::bindgen_prelude::Buffer::from(bytes),
            timestamp: ts,
          };
          tsfn.call(
            payload,
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
          );
        }
      });
      Ok(())
    })
    .build(&env)
  }

  /// Stop the screencast started by `startScreencast`.
  #[napi]
  pub async fn stop_screencast(&self) -> Result<()> {
    self.inner.stop_screencast().await.map_err(crate::error::to_napi)
  }

  #[napi(js_name = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers(&self, headers: crate::types::HashMap<String, String>) -> Result<()> {
    self
      .inner
      .set_extra_http_headers(&headers)
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .inner
      .mouse()
      .wheel(delta_x, delta_y)
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn mouse_down(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    let mouse = self.inner.mouse();
    mouse.r#move(x, y).await.map_err(crate::error::to_napi)?;
    let opts = ferridriver::page::MouseDownOptions {
      button: button.as_deref().map(ferridriver::options::MouseButton::from),
      click_count: None,
    };
    mouse.down().options(opts).await.map_err(crate::error::to_napi)?;
    *self
      .mouse_position
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn mouse_up(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    let mouse = self.inner.mouse();
    mouse.r#move(x, y).await.map_err(crate::error::to_napi)?;
    let opts = ferridriver::page::MouseUpOptions {
      button: button.as_deref().map(ferridriver::options::MouseButton::from),
      click_count: None,
    };
    mouse.up().options(opts).await.map_err(crate::error::to_napi)?;
    *self
      .mouse_position
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    Ok(())
  }

  /// Wait for a network request matching a URL pattern.
  /// Playwright API: `page.waitForRequest(urlOrPredicate)`.
  /// `url` accepts a glob string or a native JS `RegExp`.
  /// Returns the live `Request` object (Playwright parity).
  ///
  /// See `wait_for_response` for the rationale behind the sync
  /// subscribe + `AsyncBlock` return — the JS caller's next line is
  /// typically the `page.evaluate("fetch(...)")` that triggers the
  /// request, and the listener must be armed before that line runs.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((request: Request) => boolean | Promise<boolean>), timeoutMs?: number",
    ts_return_type = "Promise<Request>"
  )]
  pub fn wait_for_request(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, crate::network::Request, PredReturn>,
    >,
    timeout_ms: Option<f64>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::network::Request>> {
    use napi::bindgen_prelude::Either3;
    let mut rx = self.inner.events().subscribe();
    let timeout = timeout_ms.map_or_else(|| self.inner.default_timeout(), crate::types::f64_to_u64);
    let page = Arc::clone(&self.inner);
    let predicate = match &url {
      Either3::C(p) => Some(
        p.build_threadsafe_function::<crate::network::Request>()
          .callee_handled::<false>()
          .weak::<false>()
          .max_queue_size::<0>()
          .build()?,
      ),
      Either3::A(_) | Either3::B(_) => None,
    };
    let spec = match url {
      Either3::A(g) => Some(MatcherSpec::Glob(g)),
      Either3::B(re) => Some(MatcherSpec::Regex {
        source: re.source,
        flags: re.flags,
      }),
      Either3::C(_) => None,
    };
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = match spec {
        Some(s) => Some(s.build()?),
        None => None,
      };
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
      loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
          return Err(napi::Error::from_reason("Timeout while waiting for request"));
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
          Ok(Some(ferridriver::events::PageEvent::Request(r))) => {
            let hit = if let Some(m) = &matcher {
              m.matches(r.url())
            } else if let Some(tsfn) = &predicate {
              let np = crate::network::Request::from_core_with_page(r.clone(), Arc::clone(&page));
              let res = tsfn
                .call_async(np)
                .await
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
              resolve_pred(res).await
            } else {
              false
            };
            if hit {
              return Ok(crate::network::Request::from_core_with_page(r, page));
            }
          },
          Ok(Some(_)) => {},
          Ok(None) => {
            return Err(napi::Error::from_reason("page closed while waiting for request"));
          },
          Err(_) => return Err(napi::Error::from_reason("Timeout while waiting for request")),
        }
      }
    })
    .build(&env)
  }

  #[napi(getter)]
  pub fn default_timeout(&self) -> f64 {
    crate::types::u64_to_f64(self.inner.default_timeout())
  }
}
