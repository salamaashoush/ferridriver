//! Page class -- NAPI binding for `ferridriver::Page`.

use crate::error::IntoNapi;
use crate::locator::Locator;
use crate::types::{
  DragAndDropOptions, GotoOptions, MetricData, RoleOptions, ScreenshotOptions, SnapshotForAiOptions, TextOptions,
  WaitOptions,
};
use ferridriver::protocol::SerializedArgument;
use napi::Result;
use napi::bindgen_prelude::{Buffer, JsObjectValue as _, JsValue as _};
use napi_derive::napi;
use std::sync::{Arc, Mutex};

/// Lower an optional [`crate::types::NapiEvaluateArg`] into a
/// [`SerializedArgument`]. `None` maps to the `undefined` sentinel so
/// the utility script sees the user function called with zero
/// positional args; `Some(NapiEvaluateArg(_))` returns the wrapped
/// argument directly. The class-instance detection for `JSHandle` /
/// `ElementHandle` happens inside [`crate::types::NapiEvaluateArg`]'s
/// `FromNapiValue` impl — this helper just unwraps.
pub(crate) fn build_serialized_argument(arg: Option<crate::types::NapiEvaluateArg>) -> SerializedArgument {
  arg.map(|a| a.0).unwrap_or_default()
}

/// Return type of `Page::wait_for_event` — Playwright's overloaded
/// `Promise<Request | Response | WebSocket | ...>` union materialised
/// as a 9-way `Either`. Aliased so the `wait_for_event` signature
/// fits under clippy's `type_complexity` ceiling.
pub type PageWaitForEventResult = napi::bindgen_prelude::Either9<
  crate::network::Request,
  crate::network::Response,
  crate::network::WebSocket,
  crate::dialog::Dialog,
  crate::file_chooser::FileChooser,
  crate::download::Download,
  crate::console_message::ConsoleMessage,
  crate::web_error::JsErrorValue,
  serde_json::Value,
>;

/// High-level page API, mirrors Playwright's Page interface.
/// Predicate return: a `(req|res|url) => boolean | Promise<boolean>`
/// function resolves to either arm.
type PredReturn = napi::Either<bool, napi::bindgen_prelude::Promise<bool>>;

/// A `page.route(predicateFn, handler)` registration. The core matcher
/// is always-true (unique `Arc` identity); `pred_ref` keeps the JS
/// function so `unroute(fn)` can match it by `===`.
struct PredRoute {
  matcher: ferridriver::url_matcher::UrlMatcher,
  pred_ref: napi::bindgen_prelude::FunctionRef<JsUrl, PredReturn>,
}

/// Carries a URL string into JS as a real `URL` instance — the
/// `route(predicate)` predicate receives `(url: URL)`. The conversion
/// runs on the JS thread (same `ToNapiValue`-builds-an-object trick as
/// `web_error::JsErrorValue`), so no borrowed handle escapes a
/// threadsafe-function arg transform.
pub struct JsUrl(String);

impl napi::bindgen_prelude::ToNapiValue for JsUrl {
  unsafe fn to_napi_value(raw_env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    let env = napi::Env::from_raw(raw_env);
    let url_ctor = env
      .get_global()?
      .get_named_property_unchecked::<napi::bindgen_prelude::Function<'_, napi::JsString<'_>, ()>>("URL")?;
    let s = env.create_string(&val.0)?;
    let instance = url_ctor.new_instance(s)?;
    Ok(instance.raw())
  }
}

/// A URL matcher captured synchronously but built inside the returned
/// `AsyncBlock`, so an invalid glob/regex rejects the JS promise
/// instead of throwing synchronously (Playwright `route` returns a
/// `Promise`).
enum MatcherSpec {
  Glob(String),
  Regex { source: String, flags: Option<String> },
  Ready(ferridriver::url_matcher::UrlMatcher),
}

impl MatcherSpec {
  fn build(self) -> Result<ferridriver::url_matcher::UrlMatcher> {
    match self {
      Self::Glob(g) => ferridriver::url_matcher::UrlMatcher::glob(g).map_err(crate::error::to_napi),
      Self::Regex { source, flags } => {
        ferridriver::url_matcher::UrlMatcher::regex_from_source(&source, flags.as_deref().unwrap_or(""))
          .map_err(crate::error::to_napi)
      },
      Self::Ready(m) => Ok(m),
    }
  }
}

/// Resolve a predicate's `boolean | Promise<boolean>` return.
async fn resolve_pred(r: PredReturn) -> bool {
  match r {
    napi::Either::A(b) => b,
    napi::Either::B(p) => p.await.unwrap_or(false),
  }
}

#[napi]
pub struct Page {
  inner: Arc<ferridriver::Page>,
  mouse_position: Arc<Mutex<(f64, f64)>>,
  predicate_routes: Arc<Mutex<Vec<PredRoute>>>,
}

impl Page {
  pub(crate) fn wrap(inner: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      mouse_position: Arc::new(Mutex::new((0.0, 0.0))),
      predicate_routes: Arc::new(Mutex::new(Vec::new())),
    }
  }

  #[allow(dead_code)]
  pub(crate) fn inner_ref(&self) -> &ferridriver::Page {
    &self.inner
  }
}

#[napi]
impl Page {
  #[napi(js_name = "context")]
  pub fn context(&self) -> Result<crate::context::BrowserContext> {
    let ctx = self
      .inner
      .context()
      .cloned()
      .ok_or_else(|| napi::Error::from_reason("page has no associated browser context"))?;
    Ok(crate::context::BrowserContext::wrap(ctx))
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

  /// Playwright: `page.frameLocator(selector): FrameLocator`. Targets
  /// an `<iframe>` matching the selector at the page's main-frame
  /// scope. Equivalent to `page.mainFrame().frameLocator(selector)`.
  #[napi]
  pub fn frame_locator(&self, selector: String) -> crate::frame_locator::FrameLocator {
    crate::frame_locator::FrameLocator::wrap(self.inner.frame_locator(&selector))
  }

  /// Set the default timeout for all operations (milliseconds).
  #[napi]
  pub fn set_default_timeout(&self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  /// Set the default timeout for navigation-family operations
  /// (`goto`, `reload`, `goBack`, `goForward`, `waitForUrl`). Mirrors
  /// Playwright's `page.setDefaultNavigationTimeout(timeout)` — distinct
  /// from `setDefaultTimeout`, which applies to non-navigation actions.
  /// `0` = no timeout.
  #[napi]
  pub fn set_default_navigation_timeout(&self, ms: f64) {
    self.inner.set_default_navigation_timeout(crate::types::f64_to_u64(ms));
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  /// Navigate to `url`. Returns the main-document `Response`, or `null`
  /// for same-document navigations / backends that cannot observe the
  /// main-document response (stock `WKWebView` — see the §1.4 backend
  /// gap matrix). Mirrors Playwright's `Promise<Response | null>`.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn goto(&self, url: String, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self.inner.goto(&url, opts).await.map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Navigate back in history. Returns the main-document `Response`
  /// on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn go_back(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self.inner.go_back(opts).await.map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Navigate forward in history. Returns the main-document
  /// `Response` on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn go_forward(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self.inner.go_forward(opts).await.map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Reload the current page. Returns the main-document `Response`
  /// on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn reload(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self.inner.reload(opts).await.map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Playwright: `page.url(): string` — synchronous.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  #[napi]
  pub async fn title(&self) -> Result<String> {
    self.inner.title().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `page.video(): null | Video` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:4756`.
  /// Returns a live [`crate::video::Video`] handle when the owning
  /// context was created with `recordVideo`, or `null` otherwise. On
  /// backends that do not support screencast (stock `WKWebView`), a
  /// handle is still returned but its `path()` / `saveAs()` /
  /// `delete()` reject with a typed error explaining the reason.
  #[napi(ts_return_type = "Video | null")]
  pub fn video(&self) -> Option<crate::video::Video> {
    self.inner.video().map(crate::video::Video::from_core)
  }

  // ── Locators (lazy) ─────────────────────────────────────────────────────

  /// Playwright: `page.locator(selector, options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:324`).
  /// Thin delegator — Rust core's `Page::locator(selector, Option<FilterOptions>)`
  /// owns the filter-application logic. Page/Frame `.locator` accepts
  /// only selector strings; the `string | Locator` overload is on
  /// `Locator.locator`.
  #[napi]
  pub fn locator(&self, selector: String, options: Option<crate::types::FilterOptions>) -> Locator {
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Locator::wrap(self.inner.locator(&selector, opts))
  }

  /// Playwright: `page.querySelector(selector): Promise<ElementHandle | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts`). The
  /// `$` alias is also exposed for parity.
  ///
  /// Returns the first element matching `selector`, pinned to the
  /// [`crate::element_handle::ElementHandle`] returned. `null` when no
  /// element matches. Unlike `page.locator()`, the returned handle
  /// does not re-resolve on each action — callers `dispose()` it when
  /// done.
  #[napi]
  pub async fn query_selector(&self, selector: String) -> Result<Option<crate::element_handle::ElementHandle>> {
    let inner = self.inner.query_selector(&selector).await.into_napi()?;
    Ok(inner.map(crate::element_handle::ElementHandle::wrap))
  }

  /// Alias for [`Self::query_selector`] matching Playwright's `$` shortcut.
  #[napi(js_name = "$")]
  pub async fn dollar(&self, selector: String) -> Result<Option<crate::element_handle::ElementHandle>> {
    self.query_selector(selector).await
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  #[napi]
  pub async fn query_selector_all(&self, selector: String) -> Result<Vec<crate::element_handle::ElementHandle>> {
    let inner_handles = self.inner.query_selector_all(&selector).await.into_napi()?;
    Ok(
      inner_handles
        .into_iter()
        .map(crate::element_handle::ElementHandle::wrap)
        .collect(),
    )
  }

  /// Playwright `$$` shortcut for [`Self::query_selector_all`].
  #[napi(js_name = "$$")]
  pub async fn dollar_dollar(&self, selector: String) -> Result<Vec<crate::element_handle::ElementHandle>> {
    self.query_selector_all(selector).await
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<RoleOptions>) -> Locator {
    let opts: ferridriver::options::RoleOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_text(&crate::types::getby_input_to_rust(text), &opts))
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_label(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_label(&crate::types::getby_input_to_rust(text), &opts))
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_placeholder(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(
      self
        .inner
        .get_by_placeholder(&crate::types::getby_input_to_rust(text), &opts),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_alt_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(
      self
        .inner
        .get_by_alt_text(&crate::types::getby_input_to_rust(text), &opts),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_title(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_title(&crate::types::getby_input_to_rust(text), &opts))
  }

  #[napi(ts_args_type = "testId: string | RegExp")]
  pub fn get_by_test_id(&self, test_id: napi::Either<String, crate::types::JsRegExpLike>) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(&crate::types::getby_input_to_rust(test_id)))
  }

  // ── Frames (sync, Playwright parity — task 3.8) ─────────────────────

  /// Main frame of this page. Playwright: `page.mainFrame(): Frame`
  /// (non-null). The frame cache is seeded inside `Page::new` /
  /// `Page::with_context` before the Page is handed out.
  #[napi]
  pub fn main_frame(&self) -> crate::frame::Frame {
    crate::frame::Frame::wrap(self.inner.main_frame())
  }

  /// All frames in the page (main frame + all iframes).
  /// Playwright: `page.frames(): Frame[]` (sync).
  #[napi]
  pub fn frames(&self) -> Vec<crate::frame::Frame> {
    self.inner.frames().into_iter().map(crate::frame::Frame::wrap).collect()
  }

  /// Find a frame by name or URL. Mirrors Playwright's
  /// `page.frame(string | { name?, url? }): Frame | null` (sync).
  /// The URL field is an exact-match string for now; task 3.12 extends
  /// it to the full `string | RegExp` union.
  #[napi(ts_args_type = "selector: string | { name?: string | null | undefined; url?: string | null | undefined }")]
  pub fn frame(&self, selector: crate::types::FrameSelectorArg) -> Option<crate::frame::Frame> {
    let core_sel: ferridriver::options::FrameSelector = match selector {
      napi::Either::A(name) => ferridriver::options::FrameSelector::by_name(name),
      napi::Either::B(bag) => bag.into(),
    };
    if core_sel.is_empty() {
      return None;
    }
    self.inner.frame(core_sel).map(crate::frame::Frame::wrap)
  }

  // ── Events (Playwright-compatible on/once/waitForEvent) ─────────────

  /// Register an event listener. Returns a listener ID for removal with `off()`.
  ///
  /// Supported events: 'console', 'response', 'request', 'dialog',
  /// 'filechooser', 'download', 'frameattached', 'framedetached',
  /// 'framenavigated', 'load', 'domcontentloaded', 'close', 'pageerror'.
  /// `'pageerror'` delivers a **native JS `Error`** directly (matches
  /// Playwright's `page.on('pageerror', (error: Error) => any)`);
  /// other events deliver a plain snapshot object — use
  /// `waitForEvent(event)` for live class handles (Request / Response
  /// / Dialog / FileChooser / Download / ConsoleMessage).
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: Error | { type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | { isMultiple: boolean } | Record<string, any>) => void"
  )]
  pub fn on(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::web_error::PageListenerArg, ()>,
  ) -> Result<f64> {
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let event_name = event.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      if let Some(arg) = event_to_listener_arg(&event_name, &ev) {
        tsfn.call(arg, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      }
    });
    let id = self.inner.on(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Register a one-time event listener. Auto-removed after first match.
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: Error | { type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>) => void"
  )]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::web_error::PageListenerArg, ()>,
  ) -> Result<f64> {
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let event_name = event.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      if let Some(arg) = event_to_listener_arg(&event_name, &ev) {
        tsfn.call(arg, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      }
    });
    let id = self.inner.once(&event, callback);
    // ListenerId is a sequential counter; it will never exceed 2^53 (f64 mantissa precision).
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Remove an event listener by ID (returned from `on()` or `once()`).
  #[napi]
  pub fn off(&self, listener_id: f64) {
    // listener_id originates from on()/once() which returns a u64 counter
    // round-tripped through f64; the value is always non-negative and integral.
    self
      .inner
      .off(ferridriver::events::ListenerId(crate::types::f64_to_u64(listener_id)));
  }

  /// Remove all event listeners from this page.
  #[napi]
  pub fn remove_all_listeners(&self) {
    self.inner.remove_all_listeners();
  }

  /// Wait for a specific event. Playwright API:
  /// `page.waitForEvent(event, options?)`. Returns a live class
  /// (`Request` / `Response` / `WebSocket` / `Dialog` / `FileChooser` /
  /// `Download` / `ConsoleMessage`) for lifecycle events, a native
  /// `Error` for `'pageerror'` (mirrors Playwright's
  /// `waitForEvent('pageerror'): Promise<Error>`), or a plain snapshot
  /// object for simpler events — matches Playwright's `PageEventsMap`.
  #[napi(
    ts_args_type = "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', timeoutMs?: number",
    ts_return_type = "Promise<Request | Response | WebSocket | Dialog | FileChooser | Download | ConsoleMessage | Error | Record<string, any>>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn wait_for_event(
    &self,
    env: &napi::Env,
    event: String,
    timeout_ms: Option<f64>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<PageWaitForEventResult>> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let event_lc = event.to_ascii_lowercase();
    let page = self.inner.clone();

    // `dialog` / `filechooser` / `download` bypass the broadcast —
    // they register a one-shot handler on the per-page manager. The
    // registration in `wait_for_dialog` etc. is itself synchronous on
    // the first poll, so a sync pre-arm is not required here.
    if matches!(event_lc.as_str(), "dialog" | "filechooser" | "download") {
      return napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
        match event_lc.as_str() {
          "dialog" => {
            let d = page.wait_for_dialog(timeout).await.into_napi()?;
            Ok(napi::bindgen_prelude::Either9::D(crate::dialog::Dialog::from_core(d)))
          },
          "filechooser" => {
            let fc = page.wait_for_file_chooser(timeout).await.into_napi()?;
            Ok(napi::bindgen_prelude::Either9::E(
              crate::file_chooser::FileChooser::from_core(fc),
            ))
          },
          _ => {
            let d = page.wait_for_download(timeout).await.into_napi()?;
            Ok(napi::bindgen_prelude::Either9::F(crate::download::Download::from_core(
              d,
            )))
          },
        }
      })
      .build(env);
    }

    // Broadcast-backed events: subscribe synchronously so the JS
    // caller's subsequent triggering line can't race past us. See
    // `wait_for_response` for the same pattern.
    let mut rx = self.inner.events().subscribe();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let ev = ferridriver::events::drain_until(
        &mut rx,
        move |e| ferridriver::events::event_name_matches(&event_lc, e),
        timeout,
      )
      .await
      .into_napi()?;
      use ferridriver::events::PageEvent;
      Ok(match ev {
        PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => {
          napi::bindgen_prelude::Either9::A(crate::network::Request::from_core_with_page(r, page.clone()))
        },
        PageEvent::Response(r) => {
          napi::bindgen_prelude::Either9::B(crate::network::Response::from_core_with_page(r, page.clone()))
        },
        PageEvent::WebSocket(ws) => napi::bindgen_prelude::Either9::C(crate::network::WebSocket::from_core(ws)),
        PageEvent::Dialog(d) => napi::bindgen_prelude::Either9::D(crate::dialog::Dialog::from_core(d)),
        PageEvent::FileChooser(fc) => {
          napi::bindgen_prelude::Either9::E(crate::file_chooser::FileChooser::from_core(fc))
        },
        PageEvent::Download(d) => napi::bindgen_prelude::Either9::F(crate::download::Download::from_core(d)),
        PageEvent::Console(msg) => {
          napi::bindgen_prelude::Either9::G(crate::console_message::ConsoleMessage::from_core(msg))
        },
        // Playwright's `page.waitForEvent('pageerror')` resolves to a
        // native JS `Error` directly (not a `WebError` wrapper — that
        // class only exists for the context-scoped `'weberror'` surface).
        // `JsErrorValue::to_napi_value` constructs a real `Error`
        // instance inside the JS thread so `instanceof Error === true`.
        PageEvent::PageError(err) => {
          napi::bindgen_prelude::Either9::H(crate::web_error::JsErrorValue::from_details(err.error()))
        },
        other => napi::bindgen_prelude::Either9::I(page_event_to_value(&other)),
      })
    })
    .build(env)
  }

  /// Wait for a network response matching a URL pattern.
  /// Playwright API: `page.waitForResponse(urlOrPredicate)`.
  /// `url` accepts a glob string or a native JS `RegExp`.
  /// Returns the live `Response` object (Playwright parity).
  ///
  /// Listener registration is synchronous: the broadcast receiver is
  /// acquired before the JS `Promise` is constructed, so a follow-up
  /// `page.evaluate("fetch(...)")` on the JS caller side cannot race
  /// past the listener and fire the matching response before we are
  /// subscribed. Without this, an `async fn` boundary would defer the
  /// subscribe to the first poll of the future — which under heavy
  /// parallel load (bun's parallel runner) can land after the
  /// triggering event has already flown. Mirrors
  /// `helper.waitForEvent` in
  /// `/tmp/playwright/packages/playwright-core/src/server/helper.ts:58`.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((response: Response) => boolean | Promise<boolean>), timeoutMs?: number",
    ts_return_type = "Promise<Response>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn wait_for_response(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, crate::network::Response, PredReturn>,
    >,
    timeout_ms: Option<f64>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::network::Response>> {
    use napi::bindgen_prelude::Either3;
    let mut rx = self.inner.events().subscribe();
    let timeout = timeout_ms
      .map(crate::types::f64_to_u64)
      .unwrap_or_else(|| self.inner.default_timeout());
    let page = self.inner.clone();
    let predicate = match &url {
      Either3::C(p) => Some(
        p.build_threadsafe_function::<crate::network::Response>()
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
          return Err(napi::Error::from_reason("Timeout while waiting for response"));
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
          Ok(Ok(ferridriver::events::PageEvent::Response(r))) => {
            let hit = if let Some(m) = &matcher {
              m.matches(r.url())
            } else if let Some(tsfn) = &predicate {
              let np = crate::network::Response::from_core_with_page(r.clone(), page.clone());
              let res = tsfn
                .call_async(np)
                .await
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
              resolve_pred(res).await
            } else {
              false
            };
            if hit {
              return Ok(crate::network::Response::from_core_with_page(r, page));
            }
          },
          Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {},
          Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
            return Err(napi::Error::from_reason("page closed while waiting for response"));
          },
          Err(_) => return Err(napi::Error::from_reason("Timeout while waiting for response")),
        }
      }
    })
    .build(env)
  }

  // ── Page-level actions ──────────────────────────────────────────────────

  /// Click the first element matching `selector`. Accepts Playwright's
  /// full `PageClickOptions` bag — see
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
  #[napi]
  pub async fn click(&self, selector: String, options: Option<crate::types::ClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.click(&selector, opts).await.map_err(crate::error::to_napi)
  }

  /// Double-click the first element matching `selector`. Accepts
  /// Playwright's full `PageDblClickOptions` bag.
  #[napi]
  pub async fn dblclick(&self, selector: String, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .dblclick(&selector, opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Fill the first element matching `selector`. Accepts Playwright's
  /// full `PageFillOptions` bag.
  #[napi]
  pub async fn fill(&self, selector: String, value: String, options: Option<crate::types::FillOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .fill(&selector, &value, opts)
      .await
      .map_err(crate::error::to_napi)
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
    let opts = options.map(Into::into);
    self
      .inner
      .r#type(&selector, &text, opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Press `key` on the first element matching `selector`. Accepts
  /// Playwright's full `PagePressOptions` bag.
  #[napi]
  pub async fn press(&self, selector: String, key: String, options: Option<crate::types::PressOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .press(&selector, &key, opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Hover the first element matching `selector`. Accepts Playwright's
  /// full `PageHoverOptions` bag.
  #[napi]
  pub async fn hover(&self, selector: String, options: Option<crate::types::HoverOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.hover(&selector, opts).await.map_err(crate::error::to_napi)
  }

  /// Select options on the `<select>` matching `selector`. Accepts
  /// Playwright's full value union plus the `PageSelectOptionOptions` bag.
  #[napi]
  pub async fn select_option(
    &self,
    selector: String,
    values: crate::types::NapiSelectOptionInput,
    options: Option<crate::types::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    let opts = options.map(Into::into);
    self
      .inner
      .select_option(&selector, values.0, opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Check a checkbox matching `selector`. Accepts Playwright's full
  /// `PageCheckOptions` bag.
  #[napi]
  pub async fn check(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.check(&selector, opts).await.map_err(crate::error::to_napi)
  }

  /// Uncheck a checkbox matching `selector`. Accepts Playwright's full
  /// `PageUncheckOptions` bag.
  #[napi]
  pub async fn uncheck(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.uncheck(&selector, opts).await.map_err(crate::error::to_napi)
  }

  /// Set the checked state of a checkbox or radio matching `selector`.
  /// Mirrors Playwright's `page.setChecked(selector, checked, options?)`.
  #[napi]
  pub async fn set_checked(
    &self,
    selector: String,
    checked: bool,
    options: Option<crate::types::CheckOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .set_checked(&selector, checked, opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Tap (touch) the element matched by `selector`. Mirrors Playwright's
  /// `page.tap(selector, options?)`. Accepts the full `PageTapOptions` bag.
  #[napi]
  pub async fn tap(&self, selector: String, options: Option<crate::types::TapOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.tap(&selector, opts).await.map_err(crate::error::to_napi)
  }

  // ── Content ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn content(&self) -> Result<String> {
    self.inner.content().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn set_content(&self, html: String) -> Result<()> {
    self.inner.set_content(&html).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn markdown(&self) -> Result<String> {
    self.inner.markdown().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn text_content(&self, selector: String) -> Result<Option<String>> {
    self.inner.text_content(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn inner_text(&self, selector: String) -> Result<String> {
    self.inner.inner_text(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn inner_html(&self, selector: String) -> Result<String> {
    self.inner.inner_html(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn get_attribute(&self, selector: String, name: String) -> Result<Option<String>> {
    self
      .inner
      .get_attribute(&selector, &name)
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn input_value(&self, selector: String) -> Result<String> {
    self.inner.input_value(&selector).await.map_err(crate::error::to_napi)
  }

  // ── State checks ────────────────────────────────────────────────────────

  #[napi]
  pub async fn is_visible(&self, selector: String) -> Result<bool> {
    self.inner.is_visible(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_hidden(&self, selector: String) -> Result<bool> {
    self.inner.is_hidden(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_enabled(&self, selector: String) -> Result<bool> {
    self.inner.is_enabled(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_disabled(&self, selector: String) -> Result<bool> {
    self.inner.is_disabled(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_checked(&self, selector: String) -> Result<bool> {
    self.inner.is_checked(&selector).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_editable(&self, selector: String) -> Result<bool> {
    self.inner.is_editable(&selector).await.map_err(crate::error::to_napi)
  }

  // ── Evaluation ──────────────────────────────────────────────────────────

  /// Playwright: `page.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:515`).
  /// Rich types (`Date` / `RegExp` / `BigInt` / `URL` / `Error` / typed
  /// arrays / `NaN` / `±Infinity` / `undefined` / `-0`) round-trip as
  /// their native JS form — same as Playwright's `parseResult`.
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = build_serialized_argument(arg);
    let result = self
      .inner
      .evaluate(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `page.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:529`).
  #[napi(ts_args_type = "pageFunction: string | Function, arg?: unknown")]
  pub async fn evaluate_handle(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::js_handle::JSHandle> {
    let serialized = build_serialized_argument(arg);
    let handle = self
      .inner
      .evaluate_handle(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_selector(&self, selector: String, options: Option<WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.map_or_else(Default::default, Into::into);
    self
      .inner
      .wait_for_selector(&selector, opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Wait for the page URL to match. Accepts a glob string or a native JS `RegExp`.
  /// Playwright API: `page.waitForURL(url)`.
  #[napi(ts_args_type = "url: string | RegExp")]
  pub async fn wait_for_url(&self, url: napi::Either<String, crate::types::JsRegExpLike>) -> Result<()> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    self.inner.wait_for_url(matcher).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn wait_for_timeout(&self, ms: f64) {
    self.inner.wait_for_timeout(crate::types::f64_to_u64(ms)).await;
  }

  #[napi]
  pub async fn wait_for_load_state(&self, state: Option<String>) -> Result<()> {
    self
      .inner
      .wait_for_load_state(state.as_deref())
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn wait_for_function(&self, expression: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    self
      .inner
      .wait_for_function(&expression, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn wait_for_navigation(&self, timeout_ms: Option<f64>) -> Result<()> {
    self
      .inner
      .wait_for_navigation(timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self, options: Option<ScreenshotOptions>) -> Result<Buffer> {
    let opts: ferridriver::options::ScreenshotOptions = options.map_or_else(Default::default, Into::into);
    let bytes = self.inner.screenshot(opts).await.map_err(crate::error::to_napi)?;
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
      .pdf(rust_opts)
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
      .set_viewport_size(size.width.max(0.0) as i64, size.height.max(0.0) as i64)
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Input devices ───────────────────────────────────────────────────────

  #[napi]
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.inner.click_at(x, y).await.map_err(crate::error::to_napi)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
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
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Move mouse to coordinates without clicking.
  #[napi]
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self
      .inner
      .mouse()
      .r#move(x, y, None)
      .await
      .map_err(crate::error::to_napi)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
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
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (to_x, to_y);
    Ok(())
  }

  /// Drag the element matching `source` onto the element matching
  /// `target`. Mirrors Playwright's
  /// `page.dragAndDrop(source, target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486`.
  #[napi]
  pub async fn drag_and_drop(&self, source: String, target: String, options: Option<DragAndDropOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.drag_and_drop(&source, &target, opts).await.into_napi()
  }

  #[napi]
  pub async fn type_str(&self, text: String) -> Result<()> {
    self.inner.keyboard().r#type(&text, None).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn press_key(&self, key: String) -> Result<()> {
    self.inner.keyboard().press(&key, None).await.map_err(crate::error::to_napi)
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
    let opts: ferridriver::options::EmulateMediaOptions = options.map(Into::into).unwrap_or_default();
    self.inner.emulate_media(&opts).await.map_err(crate::error::to_napi)
  }

  // ── Focus / dispatch ────────────────────────────────────────────────────

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.map_err(crate::error::to_napi)
  }

  /// Dispatch a DOM event of `type` on the element matching `selector`.
  /// Mirrors Playwright's `page.dispatchEvent(selector, type, eventInit?, options?)`.
  #[napi]
  pub async fn dispatch_event(
    &self,
    selector: String,
    event_type: String,
    event_init: Option<serde_json::Value>,
    options: Option<crate::types::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .inner
      .dispatch_event(&selector, &event_type, event_init, options.map(Into::into))
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn start_tracing(&self) -> Result<()> {
    self.inner.start_tracing().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn stop_tracing(&self) -> Result<()> {
    self.inner.stop_tracing().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    let metrics = self.inner.metrics().await.map_err(crate::error::to_napi)?;
    Ok(metrics.iter().map(MetricData::from).collect())
  }

  // ── Misc ────────────────────────────────────────────────────────────────

  #[napi]
  pub async fn bring_to_front(&self) -> Result<()> {
    self.inner.bring_to_front().await.map_err(crate::error::to_napi)
  }

  /// Close the page. Accepts the Playwright-identical
  /// `{ runBeforeUnload?, reason? }` options shape.
  #[napi]
  pub async fn close(&self, options: Option<crate::types::PageCloseOptions>) -> Result<()> {
    let opts: Option<ferridriver::options::PageCloseOptions> = options.map(Into::into);
    self
      .inner
      .close(opts)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  #[napi]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Missing methods (batch add) ────────────────────────────────────────

  #[napi]
  pub async fn viewport_size(&self) -> Result<Vec<i32>> {
    let (w, h) = self.inner.viewport_size().await.map_err(crate::error::to_napi)?;
    let w32 = i32::try_from(w).map_err(|_| napi::Error::from_reason(format!("viewport width {w} exceeds i32::MAX")))?;
    let h32 =
      i32::try_from(h).map_err(|_| napi::Error::from_reason(format!("viewport height {h} exceeds i32::MAX")))?;
    Ok(vec![w32, h32])
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
  #[napi(ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any")]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
  ) -> Result<String> {
    self
      .inner
      .add_init_script(script.into(), arg.0)
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn remove_init_script(&self, identifier: String) -> Result<()> {
    self
      .inner
      .remove_init_script(&identifier)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `page.exposeFunction(name, callback)`. Registers a
  /// JS function on `window` that proxies into the supplied
  /// `callback`. The callback receives the args from the page-side
  /// call as a single array.
  ///
  /// The Rust core's `ExposedFn` is a sync callback that returns the
  /// page-visible value synchronously; NAPI's threadsafe-function
  /// dispatch is async, so this binding is fire-and-forget — the
  /// page-side call resolves to `null` while the JS callback runs
  /// asynchronously.
  #[napi(
    ts_args_type = "name: string, callback: (args: unknown[]) => void",
    ts_return_type = "Promise<void>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)] // napi-derive requires `&Env`
  pub fn expose_function(
    &self,
    env: &napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let cb: ferridriver::events::ExposedFn = std::sync::Arc::new(move |args: Vec<serde_json::Value>| {
      let arg = serde_json::Value::Array(args);
      tsfn.call(arg, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      serde_json::Value::Null
    });
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      inner.expose_function(&name, cb).await.map_err(crate::error::to_napi)
    })
    .build(env)
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
  pub async fn snapshot_for_ai(&self, options: Option<SnapshotForAiOptions>) -> Result<serde_json::Value> {
    let opts = options.map(Into::into).unwrap_or_default();
    let snap = self.inner.snapshot_for_ai(opts).await.map_err(crate::error::to_napi)?;
    let mut obj = serde_json::Map::new();
    obj.insert("full".to_string(), serde_json::Value::String(snap.full));
    if let Some(inc) = snap.incremental {
      obj.insert("incremental".to_string(), serde_json::Value::String(inc));
    }
    let ref_map: serde_json::Map<String, serde_json::Value> = snap
      .ref_map
      .into_iter()
      .map(|(k, v)| (k, serde_json::Value::Number(v.into())))
      .collect();
    obj.insert("refMap".to_string(), serde_json::Value::Object(ref_map));
    Ok(serde_json::Value::Object(obj))
  }

  /// Playwright: `page.ariaSnapshot(options?): Promise<string>`.
  #[napi(
    js_name = "ariaSnapshot",
    ts_args_type = "options?: { depth?: number, track?: string }",
    ts_return_type = "Promise<string>"
  )]
  pub async fn aria_snapshot(&self, options: Option<SnapshotForAiOptions>) -> Result<String> {
    let opts = options.map(Into::into).unwrap_or_default();
    self.inner.aria_snapshot(opts).await.map_err(crate::error::to_napi)
  }

  /// Playwright internal: `page.startScreencast(quality, maxWidth, maxHeight, callback)`.
  /// Begins streaming JPEG frames; the callback fires for each frame
  /// with `{ frame: Buffer, timestamp: number }`. Call
  /// `stopScreencast()` to halt.
  ///
  /// Backends: CDP-pipe / CDP-raw via `Page.startScreencast`. BiDi
  /// and stock `WKWebView` reject with a typed `Unsupported`.
  #[napi(
    ts_args_type = "quality: number, maxWidth: number, maxHeight: number, callback: (frame: { frame: Buffer; timestamp: number }) => void",
    ts_return_type = "Promise<void>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)] // napi-derive requires `&Env`
  pub fn start_screencast(
    &self,
    env: &napi::Env,
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
      // `start_screencast` returns `(rx, shutdown_tx)`. NAPI binding
      // doesn't surface a stop hook here; drop `shutdown_tx` (which
      // Chrome's stop-screencast path drives separately via Page).
      let (mut rx, _shutdown) = inner
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
    .build(env)
  }

  /// Stop the screencast started by `startScreencast`.
  #[napi]
  pub async fn stop_screencast(&self) -> Result<()> {
    self.inner.stop_screencast().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn add_script_tag(
    &self,
    url: Option<String>,
    content: Option<String>,
    script_type: Option<String>,
  ) -> Result<()> {
    self
      .inner
      .add_script_tag(url.as_deref(), content.as_deref(), script_type.as_deref())
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn add_style_tag(&self, url: Option<String>, content: Option<String>) -> Result<()> {
    self
      .inner
      .add_style_tag(url.as_deref(), content.as_deref())
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn set_extra_http_headers(&self, headers: std::collections::HashMap<String, String>) -> Result<()> {
    let mut fx = rustc_hash::FxHashMap::default();
    for (k, v) in headers {
      fx.insert(k, v);
    }
    self
      .inner
      .set_extra_http_headers(&fx)
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
    mouse.r#move(x, y, None).await.map_err(crate::error::to_napi)?;
    let opts = ferridriver::page::MouseDownOptions {
      button,
      click_count: None,
    };
    mouse.down(Some(opts)).await.map_err(crate::error::to_napi)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn mouse_up(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    let mouse = self.inner.mouse();
    mouse.r#move(x, y, None).await.map_err(crate::error::to_napi)?;
    let opts = ferridriver::page::MouseUpOptions {
      button,
      click_count: None,
    };
    mouse.up(Some(opts)).await.map_err(crate::error::to_napi)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Set files on the `<input type=file>` matching `selector`.
  /// Accepts Playwright's full value union + `PageSetInputFilesOptions`.
  #[napi(
    ts_args_type = "selector: string, files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions"
  )]
  pub async fn set_input_files(
    &self,
    selector: String,
    files: crate::types::NapiInputFiles,
    options: Option<crate::types::SetInputFilesOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .set_input_files(&selector, files.0, opts)
      .await
      .map_err(crate::error::to_napi)
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
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn wait_for_request(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, crate::network::Request, PredReturn>,
    >,
    timeout_ms: Option<f64>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::network::Request>> {
    use napi::bindgen_prelude::Either3;
    let mut rx = self.inner.events().subscribe();
    let timeout = timeout_ms
      .map(crate::types::f64_to_u64)
      .unwrap_or_else(|| self.inner.default_timeout());
    let page = self.inner.clone();
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
          Ok(Ok(ferridriver::events::PageEvent::Request(r))) => {
            let hit = if let Some(m) = &matcher {
              m.matches(r.url())
            } else if let Some(tsfn) = &predicate {
              let np = crate::network::Request::from_core_with_page(r.clone(), page.clone());
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
          Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {},
          Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
            return Err(napi::Error::from_reason("page closed while waiting for request"));
          },
          Err(_) => return Err(napi::Error::from_reason("Timeout while waiting for request")),
        }
      }
    })
    .build(env)
  }

  #[napi(getter)]
  pub fn default_timeout(&self) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
      self.inner.default_timeout() as f64
    }
  }

  // ── Network interception ─────────────────────────────────────────────

  /// Route network requests matching a glob pattern.
  ///
  /// The handler receives a `Route` object with request details and must call
  /// one of `route.fulfill()`, `route.continue()`, or `route.abort()`.
  ///
  /// ```js
  /// await page.route('**/api/*', (route) => {
  ///   if (route.url.includes('block')) {
  ///     route.abort();
  ///   } else {
  ///     route.fulfill({ status: 200, body: '{"ok":true}', contentType: 'application/json' });
  ///   }
  /// });
  /// ```
  /// Route network requests matching a glob pattern.
  ///
  /// The handler receives a `Route` object with request details and must call
  /// one of `route.fulfill()`, `route.continue()`, or `route.abort()`.
  ///
  /// ```js
  /// await page.route('**/api/*', (route) => {
  ///   if (route.url.includes('block')) {
  ///     route.abort();
  ///   } else {
  ///     route.fulfill({ status: 200, body: '{"ok":true}', contentType: 'application/json' });
  ///   }
  /// });
  /// ```
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean), handler: (route: Route) => void",
    ts_return_type = "Promise<void>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn route(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
    handler: napi::threadsafe_function::ThreadsafeFunction<
      crate::route::Route,
      (),
      crate::route::Route,
      napi::Status,
      false,
      true,
      0,
    >,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    use napi::bindgen_prelude::Either3;
    let nb = napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking;
    // The route-handler TSFN is `weak` (not `Clone`); share it via `Arc`
    // so the predicate path can move it into a per-request task.
    let handler = std::sync::Arc::new(handler);
    let (spec, rust_handler): (MatcherSpec, ferridriver::route::RouteHandler) = match url {
      Either3::A(glob) => (
        MatcherSpec::Glob(glob),
        std::sync::Arc::new(move |route| {
          handler.call(crate::route::Route::wrap(route), nb);
        }),
      ),
      Either3::B(re) => (
        MatcherSpec::Regex {
          source: re.source,
          flags: re.flags,
        },
        std::sync::Arc::new(move |route| {
          handler.call(crate::route::Route::wrap(route), nb);
        }),
      ),
      Either3::C(predicate) => {
        let pred_ref = predicate.create_ref()?;
        let ptsfn = predicate
          .build_threadsafe_function::<JsUrl>()
          .callee_handled::<false>()
          .weak::<false>()
          .max_queue_size::<0>()
          .build()?;
        let ptsfn = std::sync::Arc::new(ptsfn);
        let m = ferridriver::url_matcher::UrlMatcher::predicate(|_| true);
        self
          .predicate_routes
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .push(PredRoute {
            matcher: m.clone(),
            pred_ref,
          });
        (
          MatcherSpec::Ready(m),
          std::sync::Arc::new(move |route| {
            let ptsfn = std::sync::Arc::clone(&ptsfn);
            let handler = std::sync::Arc::clone(&handler);
            let url = JsUrl(route.request().url.clone());
            tokio::spawn(async move {
              let truthy = match ptsfn.call_async(url).await {
                Ok(r) => resolve_pred(r).await,
                Err(_) => false,
              };
              if truthy {
                handler.call(crate::route::Route::wrap(route), nb);
              } else {
                route.continue_route(ferridriver::route::ContinueOverrides::default());
              }
            });
          }),
        )
      },
    };

    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = spec.build()?;
      inner.route(matcher, rust_handler).await.map_err(crate::error::to_napi)
    })
    .build(env)
  }

  /// `page.unroute(string | RegExp | ((url: URL) => boolean))`. A
  /// predicate is matched by `===` against the function passed to
  /// `route`, dropping its always-true core matcher by `Arc` identity.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean)",
    ts_return_type = "Promise<void>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn unroute(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    use napi::bindgen_prelude::Either3;
    let specs: Vec<MatcherSpec> = match url {
      Either3::A(glob) => vec![MatcherSpec::Glob(glob)],
      Either3::B(re) => vec![MatcherSpec::Regex {
        source: re.source,
        flags: re.flags,
      }],
      Either3::C(predicate) => {
        // `Function` is not `Copy` across iterations; round-trip the
        // input through a `Ref` so each comparison borrows a fresh
        // handle.
        let in_ref = predicate.create_ref()?;
        let mut guard = self
          .predicate_routes
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut hit = Vec::new();
        let mut i = 0;
        while i < guard.len() {
          let same = {
            let a = in_ref.borrow_back(env)?;
            let b = guard[i].pred_ref.borrow_back(env)?;
            env.strict_equals(a, b)?
          };
          if same {
            hit.push(MatcherSpec::Ready(guard.remove(i).matcher));
          } else {
            i += 1;
          }
        }
        hit
      },
    };
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      for spec in specs {
        let m = spec.build()?;
        inner.unroute(&m).await.map_err(crate::error::to_napi)?;
      }
      Ok(())
    })
    .build(env)
  }

  // ── Expect assertions (delegates to Rust core, all polling in Rust) ──

  #[napi]
  pub async fn expect_title(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_title(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_url(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_url(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }
}

#[napi(object)]
pub struct MouseClickOptions {
  pub button: Option<String>,
  #[napi(js_name = "clickCount")]
  pub click_count: Option<i32>,
  pub delay: Option<f64>,
}

/// Playwright `{ delay? }` for `keyboard.press` / `keyboard.type`.
#[napi(object)]
pub struct KeyDelayOptions {
  pub delay: Option<f64>,
}

#[napi]
pub struct Keyboard {
  page: Arc<ferridriver::Page>,
}

#[napi]
impl Keyboard {
  #[napi]
  pub async fn down(&self, key: String) -> Result<()> {
    self.page.keyboard().down(&key).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn up(&self, key: String) -> Result<()> {
    self.page.keyboard().up(&key).await.map_err(crate::error::to_napi)
  }

  /// Playwright: `keyboard.press(key, options?: { delay? })`.
  #[napi]
  pub async fn press(&self, key: String, options: Option<KeyDelayOptions>) -> Result<()> {
    let opts = options
      .and_then(|o| o.delay)
      .map(|d| ferridriver::page::KeyboardPressOptions {
        delay: Some(crate::types::f64_to_u64(d)),
      });
    self.page.keyboard().press(&key, opts).await.map_err(crate::error::to_napi)
  }

  /// Playwright: `keyboard.type(text, options?: { delay? })`.
  #[napi(js_name = "type")]
  pub async fn type_text(&self, text: String, options: Option<KeyDelayOptions>) -> Result<()> {
    let opts = options
      .and_then(|o| o.delay)
      .map(|d| ferridriver::page::KeyboardTypeOptions {
        delay: Some(crate::types::f64_to_u64(d)),
      });
    self.page.keyboard().r#type(&text, opts).await.map_err(crate::error::to_napi)
  }

  #[napi(js_name = "insertText")]
  pub async fn insert_text(&self, text: String) -> Result<()> {
    self
      .page
      .keyboard()
      .insert_text(&text)
      .await
      .map_err(crate::error::to_napi)
  }
}

#[napi]
pub struct Mouse {
  page: Arc<ferridriver::Page>,
  position: Arc<Mutex<(f64, f64)>>,
}

#[napi]
impl Mouse {
  #[napi]
  pub async fn click(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options.as_ref().and_then(|o| o.button.clone()),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
      delay: options.as_ref().and_then(|o| o.delay).map(crate::types::f64_to_u64),
    };
    self
      .page
      .mouse()
      .click(x, y, Some(opts))
      .await
      .map_err(crate::error::to_napi)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi(js_name = "move")]
  pub async fn move_to(&self, x: f64, y: f64, steps: Option<i32>) -> Result<()> {
    let step_count = steps
      .map(|s| u32::try_from(s).map_err(|_| napi::Error::from_reason("steps must be non-negative")))
      .transpose()?;
    self
      .page
      .mouse()
      .r#move(x, y, step_count)
      .await
      .map_err(crate::error::to_napi)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn dblclick(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options.as_ref().and_then(|o| o.button.clone()),
      click_count: None,
      delay: options.as_ref().and_then(|o| o.delay).map(crate::types::f64_to_u64),
    };
    self
      .page
      .mouse()
      .dblclick(x, y, Some(opts))
      .await
      .map_err(crate::error::to_napi)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Playwright: `mouse.down(options?: { button?, clickCount? })`.
  #[napi]
  pub async fn down(&self, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseDownOptions {
      button: options.as_ref().and_then(|o| o.button.clone()),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self.page.mouse().down(Some(opts)).await.map_err(crate::error::to_napi)
  }

  /// Playwright: `mouse.up(options?: { button?, clickCount? })`.
  #[napi]
  pub async fn up(&self, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseUpOptions {
      button: options.as_ref().and_then(|o| o.button.clone()),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self.page.mouse().up(Some(opts)).await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .page
      .mouse()
      .wheel(delta_x, delta_y)
      .await
      .map_err(crate::error::to_napi)
  }
}

/// Single screencast frame surfaced through the
/// `page.startScreencast` callback.
#[napi(object)]
pub struct ScreencastFrame {
  pub frame: napi::bindgen_prelude::Buffer,
  pub timestamp: f64,
}

/// Playwright `Touchscreen`. Construct via `page.touchscreen`.
#[napi]
pub struct Touchscreen {
  page: Arc<ferridriver::Page>,
}

#[napi]
impl Touchscreen {
  /// Playwright: `touchscreen.tap(x, y)`. Dispatches a real
  /// `TouchEvent` on platforms supporting it; falls back to a
  /// synthesized `PointerEvent` on platforms without touch (e.g.,
  /// stock `WKWebView` on macOS).
  #[napi]
  pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
    self.page.touchscreen().tap(x, y).await.map_err(crate::error::to_napi)
  }
}

// ── Event conversion helpers ─────────────────────────────────────────────

use ferridriver::events::PageEvent;
use ferridriver::network::{Request as NetRequest, Response as NetResponse};

fn request_snapshot(req: &NetRequest) -> serde_json::Value {
  serde_json::json!({
    "url": req.url(),
    "method": req.method(),
    "resourceType": req.resource_type(),
    "isNavigationRequest": req.is_navigation_request(),
    "headers": req.headers(),
    "postData": req.post_data(),
  })
}

fn response_snapshot(resp: &NetResponse) -> serde_json::Value {
  serde_json::json!({
    "url": resp.url(),
    "status": resp.status(),
    "statusText": resp.status_text(),
    "ok": resp.ok(),
    "fromServiceWorker": resp.is_from_service_worker(),
    "headers": resp.headers(),
  })
}

/// Project a live [`ferridriver::console_message::ConsoleMessage`] into
/// the compact JSON shape `page.on('console', cb)` / the
/// `waitForEvent` fallback path surface. Live-handle access (args as
/// `JSHandle`, `location`, `page`) goes through the dedicated
/// `ConsoleMessage` NAPI class returned from `page.waitForEvent('console')`.
fn console_message_snapshot(msg: &ferridriver::console_message::ConsoleMessage) -> serde_json::Value {
  let loc = msg.location();
  serde_json::json!({
    "type": msg.type_str(),
    "text": msg.text(),
    "location": {
      "url": loc.url,
      "lineNumber": loc.line_number,
      "columnNumber": loc.column_number,
    },
    "timestamp": msg.timestamp(),
    "argsCount": msg.args().len(),
  })
}

/// Convert a named event to a JS value. The `request`/`response` family
/// surfaces a sync snapshot here — full live access to `Request` /
/// `Response` lifecycle methods is exposed via `wait_for_request` and
/// `wait_for_response` which return the dedicated NAPI classes.
fn event_to_js(event_name: &str, event: &PageEvent) -> Option<serde_json::Value> {
  match (event_name, event) {
    ("console", PageEvent::Console(msg)) => Some(console_message_snapshot(msg)),
    ("response", PageEvent::Response(r)) => Some(response_snapshot(r)),
    ("request", PageEvent::Request(r))
    | ("requestfinished", PageEvent::RequestFinished(r))
    | ("requestfailed", PageEvent::RequestFailed(r)) => Some(request_snapshot(r)),
    ("websocket", PageEvent::WebSocket(ws)) => Some(serde_json::json!({"url": ws.url(), "isClosed": ws.is_closed()})),
    ("dialog", PageEvent::Dialog(d)) => Some(serde_json::json!({
      "type": d.dialog_type().as_str(),
      "message": d.message(),
      "defaultValue": d.default_value(),
    })),
    ("filechooser", PageEvent::FileChooser(fc)) => Some(serde_json::json!({
      "isMultiple": fc.is_multiple(),
    })),
    ("frameattached", PageEvent::FrameAttached(f)) | ("framenavigated", PageEvent::FrameNavigated(f)) => {
      serde_json::to_value(f).ok()
    },
    ("framedetached", PageEvent::FrameDetached { frame_id }) => Some(serde_json::json!({"frameId": frame_id})),
    ("download", PageEvent::Download(d)) => Some(serde_json::json!({
      "url": d.url(),
      "suggestedFilename": d.suggested_filename(),
    })),
    ("load", PageEvent::Load) | ("domcontentloaded", PageEvent::DomContentLoaded) | ("close", PageEvent::Close) => {
      Some(serde_json::Value::Object(serde_json::Map::new()))
    },
    ("pageerror", PageEvent::PageError(err)) => {
      let d = err.error();
      Some(serde_json::json!({
        "name": d.name,
        "message": d.message,
        "stack": d.stack,
      }))
    },
    _ => None,
  }
}

/// Project a page event into the NAPI-side [`PageListenerArg`] enum
/// that's sent through the threadsafe function. `'pageerror'` uses
/// the `PageError` variant so the JS callback receives a native JS
/// `Error` (Playwright parity); every other event surface keeps the
/// existing compact-JSON snapshot so consumers see the same shape
/// they did before.
fn event_to_listener_arg(event_name: &str, event: &PageEvent) -> Option<crate::web_error::PageListenerArg> {
  if event_name == "pageerror" {
    if let PageEvent::PageError(err) = event {
      return Some(crate::web_error::PageListenerArg::PageError(
        crate::web_error::JsErrorValue::from_details(err.error()),
      ));
    }
    return None;
  }
  event_to_js(event_name, event).map(crate::web_error::PageListenerArg::Snapshot)
}

/// Convert any `PageEvent` to a JS value (for `waitForEvent`).
fn page_event_to_value(event: &PageEvent) -> serde_json::Value {
  match event {
    PageEvent::Console(msg) => console_message_snapshot(msg),
    PageEvent::Response(r) => response_snapshot(r),
    PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => request_snapshot(r),
    PageEvent::WebSocket(ws) => serde_json::json!({"url": ws.url(), "isClosed": ws.is_closed()}),
    PageEvent::Dialog(d) => serde_json::json!({
      "type": d.dialog_type().as_str(),
      "message": d.message(),
      "defaultValue": d.default_value(),
    }),
    PageEvent::FileChooser(fc) => serde_json::json!({
      "isMultiple": fc.is_multiple(),
    }),
    PageEvent::FrameAttached(f) | PageEvent::FrameNavigated(f) => serde_json::to_value(f).unwrap_or_default(),
    PageEvent::FrameDetached { frame_id } => serde_json::json!({"frameId": frame_id}),
    PageEvent::Download(d) => serde_json::json!({
      "url": d.url(),
      "suggestedFilename": d.suggested_filename(),
    }),
    PageEvent::Load => serde_json::json!({"type": "load"}),
    PageEvent::DomContentLoaded => serde_json::json!({"type": "domcontentloaded"}),
    PageEvent::Close => serde_json::json!({"type": "close"}),
    PageEvent::PageError(err) => {
      let d = err.error();
      serde_json::json!({
        "name": d.name,
        "message": d.message,
        "stack": d.stack,
      })
    },
  }
}
