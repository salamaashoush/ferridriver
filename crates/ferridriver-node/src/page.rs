//! Page class -- NAPI binding for `ferridriver::Page`.

use crate::error::IntoNapi;
use crate::locator::Locator;
use crate::types::{
  DragAndDropOptions, GotoOptions, MetricData, RoleOptions, ScreenshotOptions, SnapshotForAiOptions, TextOptions,
  WaitOptions,
};
use std::sync::{Arc, Mutex};

use ferridriver::protocol::SerializedArgument;
use napi::Result;
use napi::bindgen_prelude::{Buffer, JsObjectValue as _, JsValue as _};
use napi_derive::napi;

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

/// The live object a page event carries to its listener / waiter —
/// Playwright's per-event `PageEventsMap` union materialised as a
/// 10-way `Either`. `frameattached` / `framedetached` / `framenavigated`
/// deliver a live `Frame`; `load` / `domcontentloaded` / `close`
/// deliver the `Page` itself; `pageerror` a native `Error`; everything
/// else its lifecycle class. Shared by `page.on` / `page.once`
/// (threadsafe-function dispatch) and `page.waitForEvent`.
pub type PageWaitForEventResult = napi::bindgen_prelude::Either10<
  crate::network::Request,
  crate::network::Response,
  crate::network::WebSocket,
  crate::dialog::Dialog,
  crate::file_chooser::FileChooser,
  crate::download::Download,
  crate::console_message::ConsoleMessage,
  crate::web_error::JsErrorValue,
  crate::frame::Frame,
  Page,
>;

/// Lift a core [`PageEvent`] into the live-object union listeners
/// receive. Mirrors Playwright's listener argument per event type.
fn live_event_arg(page: &Arc<ferridriver::Page>, ev: ferridriver::events::PageEvent) -> PageWaitForEventResult {
  use ferridriver::events::PageEvent;
  use napi::bindgen_prelude::Either10;
  match ev {
    PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => {
      Either10::A(crate::network::Request::from_core_with_page(r, page.clone()))
    },
    PageEvent::Response(r) => Either10::B(crate::network::Response::from_core_with_page(r, page.clone())),
    PageEvent::WebSocket(ws) => Either10::C(crate::network::WebSocket::from_core(ws)),
    PageEvent::Dialog(d) => Either10::D(crate::dialog::Dialog::from_core(d)),
    PageEvent::FileChooser(fc) => Either10::E(crate::file_chooser::FileChooser::from_core(fc)),
    PageEvent::Download(d) => Either10::F(crate::download::Download::from_core(d)),
    PageEvent::Console(msg) => Either10::G(crate::console_message::ConsoleMessage::from_core(msg)),
    // Playwright's `'pageerror'` listener receives a native JS `Error`;
    // `JsErrorValue::to_napi_value` constructs one on the JS thread.
    PageEvent::PageError(err) => Either10::H(crate::web_error::JsErrorValue::from_details(err.error())),
    PageEvent::FrameAttached(info) | PageEvent::FrameNavigated(info) => {
      Either10::I(crate::frame::Frame::wrap(page.frame_for_id(&info.frame_id)))
    },
    PageEvent::FrameDetached { frame_id } => Either10::I(crate::frame::Frame::wrap(page.frame_for_id(&frame_id))),
    PageEvent::Load | PageEvent::DomContentLoaded | PageEvent::Close => Either10::J(Page::wrap(page.clone())),
  }
}

/// One `page.on`/`page.once` registration: the event name, the core
/// `ListenerId`, and a `FunctionRef` to the original JS listener so
/// Playwright's `off(event, listener)` can match by `===` identity.
struct ListenerReg {
  event: String,
  id: u64,
  fn_ref: napi::bindgen_prelude::FunctionRef<PageWaitForEventResult, ()>,
}

/// High-level page API, mirrors Playwright's Page interface.
/// Predicate return: a `(req|res|url) => boolean | Promise<boolean>`
/// function resolves to either arm.
pub(crate) type PredReturn = napi::Either<bool, napi::bindgen_prelude::Promise<bool>>;

/// Return type of an `addLocatorHandler` handler: `void | Promise<void>`.
/// The registry awaits the promise arm before continuing the original action.
pub(crate) type LocatorHandlerReturn = napi::Either<(), napi::bindgen_prelude::Promise<()>>;

/// Options for `page.addLocatorHandler(locator, handler, options?)`.
/// Mirrors Playwright `{ times?: number, noWaitAfter?: boolean }`.
#[napi(object)]
pub struct AddLocatorHandlerOptions {
  pub times: Option<u32>,
  pub no_wait_after: Option<bool>,
}

/// A `page.route(predicateFn, handler)` registration. The core matcher
/// is always-true (unique `Arc` identity); `pred_ref` keeps the JS
/// function so `unroute(fn)` can match it by `===`.
pub(crate) struct PredRoute {
  pub(crate) matcher: ferridriver::url_matcher::UrlMatcher,
  pub(crate) pred_ref: napi::bindgen_prelude::FunctionRef<JsUrl, PredReturn>,
}

/// Carries a URL string into JS as a real `URL` instance — the
/// `route(predicate)` predicate receives `(url: URL)`. The conversion
/// runs on the JS thread (same `ToNapiValue`-builds-an-object trick as
/// `web_error::JsErrorValue`), so no borrowed handle escapes a
/// threadsafe-function arg transform.
pub struct JsUrl(String);

impl JsUrl {
  pub(crate) fn new(url: String) -> Self {
    Self(url)
  }
}

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
pub(crate) enum MatcherSpec {
  Glob(String),
  Regex { source: String, flags: Option<String> },
  Ready(ferridriver::url_matcher::UrlMatcher),
}

impl MatcherSpec {
  pub(crate) fn build(self) -> Result<ferridriver::url_matcher::UrlMatcher> {
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
pub(crate) async fn resolve_pred(r: PredReturn) -> bool {
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
  /// `page.on`/`page.once` registrations, kept so `off(event, listener)`
  /// can resolve the core `ListenerId` from the JS function identity.
  listener_regs: Arc<Mutex<Vec<ListenerReg>>>,
}

impl Page {
  pub(crate) fn wrap(inner: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      mouse_position: Arc::new(Mutex::new((0.0, 0.0))),
      predicate_routes: Arc::new(Mutex::new(Vec::new())),
      listener_regs: Arc::new(Mutex::new(Vec::new())),
    }
  }

  pub(crate) fn inner_ref(&self) -> &ferridriver::Page {
    &self.inner
  }

  pub(crate) fn inner_arc(&self) -> Arc<ferridriver::Page> {
    Arc::clone(&self.inner)
  }

  /// Shared body of `on` / `once`: build the threadsafe dispatch, keep
  /// a `FunctionRef` for `off(event, listener)` identity matching, and
  /// register on the core emitter (which filters by event name).
  fn register_listener(
    &self,
    event: &str,
    listener: &napi::bindgen_prelude::Function<'_, PageWaitForEventResult, ()>,
    once: bool,
  ) -> Result<f64> {
    let fn_ref = listener.create_ref()?;
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let page = self.inner.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      tsfn.call(
        live_event_arg(&page, ev),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    });
    let id = if once {
      self.inner.once(event, callback)
    } else {
      self.inner.on(event, callback)
    };
    self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(ListenerReg {
        event: event.to_string(),
        id: id.0,
        fn_ref,
      });
    // ListenerId is a sequential counter; it will never exceed 2^53 (f64 mantissa precision).
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
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
  /// for same-document navigations (no new main-document request was
  /// issued). Mirrors Playwright's `Promise<Response | null>`.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn goto(&self, url: String, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self
      .inner
      .goto(&url)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Navigate back in history. Returns the main-document `Response`
  /// on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn go_back(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self
      .inner
      .go_back()
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Navigate forward in history. Returns the main-document
  /// `Response` on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn go_forward(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self
      .inner
      .go_forward()
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, self.inner.clone())))
  }

  /// Reload the current page. Returns the main-document `Response`
  /// on the same basis as `goto` (or `null`).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn reload(&self, options: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    let resp = self
      .inner
      .reload()
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)?;
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
  /// context was created with `recordVideo`, or `null` otherwise. If a
  /// backend cannot produce the recording, a handle is still returned
  /// but its `path()` / `saveAs()` / `delete()` reject with a typed
  /// error explaining the reason.
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
    Locator::wrap(match opts {
      Some(f) => self.inner.locator_with(&selector, &f),
      None => self.inner.locator(&selector),
    })
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
    let opts = options.map(ferridriver::options::RoleOptions::from);
    Locator::wrap(self.inner.get_by_role(role.as_str()).maybe_options(opts).into_locator())
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_text(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_label(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_label(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_placeholder(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_placeholder(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_alt_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_alt_text(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_title(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_title(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "testId: string | RegExp")]
  pub fn get_by_test_id(&self, test_id: napi::Either<String, crate::types::JsRegExpLike>) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(crate::types::getby_input_to_rust(test_id)))
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

  /// Register an event listener. Returns a listener ID (ferridriver
  /// extension; also removable Playwright-style via
  /// `off(event, listener)`).
  ///
  /// Listeners receive the same live objects Playwright delivers:
  /// `ConsoleMessage` for `'console'`, `Request` for `'request'` /
  /// `'requestfinished'` / `'requestfailed'`, `Response`, `WebSocket`,
  /// `Dialog`, `FileChooser`, `Download`, a live `Frame` for the frame
  /// events, the `Page` itself for `'load'` / `'domcontentloaded'` /
  /// `'close'`, and a native JS `Error` for `'pageerror'`.
  #[napi(
    ts_args_type = "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => void"
  )]
  pub fn on(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, PageWaitForEventResult, ()>,
  ) -> Result<f64> {
    self.register_listener(&event, &listener, false)
  }

  /// Register a one-time event listener. Auto-removed after first match.
  /// Same live listener argument as [`Page::on`].
  #[napi(
    ts_args_type = "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => void"
  )]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, PageWaitForEventResult, ()>,
  ) -> Result<f64> {
    self.register_listener(&event, &listener, true)
  }

  /// Remove an event listener — Playwright's `off(event, listener)`
  /// (function identity, `===`) or the ferridriver id form
  /// `off(listenerId)` with the number returned from `on()`/`once()`.
  #[napi(
    ts_args_type = "eventOrId: string | number, listener?: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => void"
  )]
  // napi-rs only injects `Env` as `&Env`, hence the pass-by-ref allow
  // (same constraint as `wait_for_event` / `unroute`).
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn off(
    &self,
    env: &napi::Env,
    event_or_id: napi::Either<String, f64>,
    listener: Option<napi::bindgen_prelude::Function<'_, PageWaitForEventResult, ()>>,
  ) -> Result<()> {
    let mut regs = self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    match event_or_id {
      napi::Either::B(listener_id) => {
        let id = crate::types::f64_to_u64(listener_id);
        self.inner.off(ferridriver::events::ListenerId(id));
        regs.retain(|r| r.id != id);
      },
      napi::Either::A(event) => {
        let Some(listener) = listener else {
          // Lenient `off(event)` — drop every listener for that event
          // (Playwright requires the listener; this matches
          // `removeAllListeners(event)` semantics instead of erroring).
          self.inner.remove_listeners_named(&event);
          regs.retain(|r| r.event != event);
          return Ok(());
        };
        let in_ref = listener.create_ref()?;
        let mut i = 0;
        while i < regs.len() {
          let hit = regs[i].event == event && {
            let a = in_ref.borrow_back(env)?;
            let b = regs[i].fn_ref.borrow_back(env)?;
            env.strict_equals(a, b)?
          };
          if hit {
            let reg = regs.remove(i);
            self.inner.off(ferridriver::events::ListenerId(reg.id));
          } else {
            i += 1;
          }
        }
      },
    }
    Ok(())
  }

  /// Remove event listeners — all of them, or only those for `event`
  /// when given. Playwright: `page.removeAllListeners(type?: string)`.
  #[napi]
  pub fn remove_all_listeners(&self, event: Option<String>) {
    let mut regs = self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    match event {
      Some(ev) => {
        self.inner.remove_listeners_named(&ev);
        regs.retain(|r| r.event != ev);
      },
      None => {
        self.inner.remove_all_listeners();
        regs.clear();
      },
    }
  }

  /// Wait for a specific event. Playwright API:
  /// `page.waitForEvent(event, optionsOrPredicate?)` — the second
  /// argument is a predicate function, a `{ predicate?, timeout? }`
  /// bag, or (ferridriver extension) a bare timeout in ms. Resolves to
  /// the same live object the matching `page.on` listener would receive
  /// (`ConsoleMessage` / `Request` / `Response` / `WebSocket` /
  /// `Dialog` / `FileChooser` / `Download` / `Frame` / `Page`, native
  /// `Error` for `'pageerror'`) — matches Playwright's `PageEventsMap`.
  /// The predicate receives that live object and the wait resolves on
  /// the first event for which it returns truthy.
  #[napi(
    ts_args_type = "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', optionsOrPredicate?: number | ((data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => boolean | Promise<boolean>) | { predicate?: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => boolean | Promise<boolean>; timeout?: number }",
    ts_return_type = "Promise<Request | Response | WebSocket | Dialog | FileChooser | Download | ConsoleMessage | Error | Frame | Page>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn wait_for_event(
    &self,
    env: &napi::Env,
    event: String,
    options_or_predicate: Option<
      napi::bindgen_prelude::Either3<
        f64,
        napi::bindgen_prelude::Function<'_, PageWaitForEventResult, PredReturn>,
        napi::bindgen_prelude::Object<'_>,
      >,
    >,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<PageWaitForEventResult>> {
    use napi::bindgen_prelude::Either3;
    let mut timeout_ms: Option<f64> = None;
    let mut predicate = None;
    match options_or_predicate {
      None => {},
      Some(Either3::A(t)) => timeout_ms = Some(t),
      Some(Either3::B(f)) => {
        predicate = Some(
          f.build_threadsafe_function::<PageWaitForEventResult>()
            .callee_handled::<false>()
            .weak::<false>()
            .max_queue_size::<0>()
            .build()?,
        );
      },
      Some(Either3::C(obj)) => {
        timeout_ms = obj.get::<f64>("timeout")?;
        if let Some(f) =
          obj.get::<napi::bindgen_prelude::Function<'_, PageWaitForEventResult, PredReturn>>("predicate")?
        {
          predicate = Some(
            f.build_threadsafe_function::<PageWaitForEventResult>()
              .callee_handled::<false>()
              .weak::<false>()
              .max_queue_size::<0>()
              .build()?,
          );
        }
      },
    }
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let event_lc = event.to_ascii_lowercase();
    let page = self.inner.clone();

    // `dialog` / `filechooser` / `download` bypass the broadcast when
    // there is no predicate — they register a one-shot handler on the
    // per-page manager. With a predicate they go through the broadcast
    // like every other event (the emitter bridge claims the live
    // handles on behalf of broadcast listeners).
    if predicate.is_none() && matches!(event_lc.as_str(), "dialog" | "filechooser" | "download") {
      return napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
        match event_lc.as_str() {
          "dialog" => {
            let d = page.wait_for_dialog(timeout).await.into_napi()?;
            Ok(napi::bindgen_prelude::Either10::D(crate::dialog::Dialog::from_core(d)))
          },
          "filechooser" => {
            let fc = page.wait_for_file_chooser(timeout).await.into_napi()?;
            Ok(napi::bindgen_prelude::Either10::E(
              crate::file_chooser::FileChooser::from_core(fc),
            ))
          },
          _ => {
            let d = page.wait_for_download(timeout).await.into_napi()?;
            Ok(napi::bindgen_prelude::Either10::F(
              crate::download::Download::from_core(d),
            ))
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
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
      loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let ev = ferridriver::events::drain_until(
          &mut rx,
          {
            let event_lc = event_lc.clone();
            move |e| ferridriver::events::event_name_matches(&event_lc, e)
          },
          remaining.as_millis().try_into().unwrap_or(0),
        )
        .await
        .into_napi()?;
        let Some(tsfn) = &predicate else {
          return Ok(live_event_arg(&page, ev));
        };
        let res = tsfn
          .call_async(live_event_arg(&page, ev.clone()))
          .await
          .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if resolve_pred(res).await {
          return Ok(live_event_arg(&page, ev));
        }
      }
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
          Ok(Some(ferridriver::events::PageEvent::Response(r))) => {
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
          Ok(Some(_)) => {},
          Ok(None) => {
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
    self
      .inner
      .click(&selector)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Double-click the first element matching `selector`. Accepts
  /// Playwright's full `PageDblClickOptions` bag.
  #[napi]
  pub async fn dblclick(&self, selector: String, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .dblclick(&selector)
      .maybe_options(opts)
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
      .fill(&selector, &value)
      .maybe_options(opts)
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
      .r#type(&selector, &text)
      .maybe_options(opts)
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
      .press(&selector, &key)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Hover the first element matching `selector`. Accepts Playwright's
  /// full `PageHoverOptions` bag.
  #[napi]
  pub async fn hover(&self, selector: String, options: Option<crate::types::HoverOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .hover(&selector)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
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
      .select_option(&selector, values.0)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Check a checkbox matching `selector`. Accepts Playwright's full
  /// `PageCheckOptions` bag.
  #[napi]
  pub async fn check(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .check(&selector)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Uncheck a checkbox matching `selector`. Accepts Playwright's full
  /// `PageUncheckOptions` bag.
  #[napi]
  pub async fn uncheck(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .uncheck(&selector)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
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
      .set_checked(&selector, checked)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Tap (touch) the element matched by `selector`. Mirrors Playwright's
  /// `page.tap(selector, options?)`. Accepts the full `PageTapOptions` bag.
  #[napi]
  pub async fn tap(&self, selector: String, options: Option<crate::types::TapOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .tap(&selector)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
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
    let opts = options.map(ferridriver::options::WaitOptions::try_from).transpose()?;
    self
      .inner
      .wait_for_selector(&selector)
      .maybe_options(opts)
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
    self.inner.mouse().r#move(x, y).await.map_err(crate::error::to_napi)?;
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
    self
      .inner
      .drag_and_drop(&source, &target)
      .maybe_options(opts)
      .await
      .into_napi()
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
      .dispatch_event(&selector, &event_type, event_init)
      .maybe_options(options.map(Into::into))
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

  /// Playwright: `page.requestGC(): Promise<void>`. Forces a
  /// garbage-collection pass in the page's JS engine.
  #[napi(js_name = "requestGC")]
  pub async fn request_gc(&self) -> Result<()> {
    self.inner.request_gc().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `page.consoleMessages(options?: { filter?: 'all' |
  /// 'since-navigation' }): Promise<ConsoleMessage[]>`. Defaults to
  /// `since-navigation`, i.e. only messages logged after the last
  /// main-frame navigation.
  #[napi]
  #[allow(clippy::unused_async_trait_impl)] // NAPI requires async to surface a JS Promise
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
  #[allow(clippy::unused_async_trait_impl)] // NAPI requires async to surface a JS Promise
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
    crate::web_storage::WebStorage::new(self.inner.clone(), ferridriver::options::WebStorageKind::Local)
  }

  /// Playwright: `page.sessionStorage: WebStorage` — the `sessionStorage`
  /// area for the page's current origin.
  #[napi(getter)]
  pub fn session_storage(&self) -> crate::web_storage::WebStorage {
    crate::web_storage::WebStorage::new(self.inner.clone(), ferridriver::options::WebStorageKind::Session)
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
  #[napi(
    ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any",
    ts_return_type = "Promise<Disposable>"
  )]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
  ) -> Result<crate::disposable::Disposable> {
    let disposable = self
      .inner
      .add_init_script(script.into(), arg.0)
      .await
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

  /// Playwright: `page.exposeFunction(name, callback)`. Registers a
  /// JS function on `window` that proxies into the supplied
  /// `callback`. The callback receives the args from the page-side
  /// call as a single array.
  ///
  /// NAPI convention: the callback receives the page-side call
  /// arguments as a single array (`(args: unknown[]) => void`) and is
  /// fire-and-forget — the page-side call resolves to `null` while the
  /// JS callback runs asynchronously. Return-value delivery + arg
  /// spreading (full Playwright parity) lives on the QuickJS/script
  /// surface, which is the one that runs LLM-generated Playwright code;
  /// this Rust-native binding keeps its established contract.
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
      // ExposedFn is async now (the QuickJS surface awaits the real
      // value); NAPI stays fire-and-forget — resolve immediately.
      Box::pin(async move { serde_json::Value::Null })
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
    let opts = options.map(Into::into);
    let snap = self
      .inner
      .snapshot_for_ai()
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)?;
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
    let opts = options.map(Into::into);
    self
      .inner
      .aria_snapshot()
      .maybe_options(opts)
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
  /// `Page.startScreencast`, BiDi and WebKit via their respective
  /// screencast protocols.
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
    mouse.r#move(x, y).await.map_err(crate::error::to_napi)?;
    let opts = ferridriver::page::MouseDownOptions {
      button: button.as_deref().map(ferridriver::options::MouseButton::from),
      click_count: None,
    };
    mouse.down().options(opts).await.map_err(crate::error::to_napi)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
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
      .set_input_files(&selector, files.0)
      .maybe_options(opts)
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
          Ok(Some(ferridriver::events::PageEvent::Request(r))) => {
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
          Ok(Some(_)) => {},
          Ok(None) => {
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
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean), handler: (route: Route) => void, options?: { times?: number }",
    ts_return_type = "Promise<Disposable>"
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
    options: Option<crate::types::RouteOptions>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::disposable::Disposable>> {
    use napi::bindgen_prelude::Either3;
    let times = options.and_then(|o| o.times_u32());
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
                route.fallback(ferridriver::route::ContinueOverrides::default());
              }
            });
          }),
        )
      },
    };

    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = spec.build()?;
      let disposable = inner
        .route(matcher, rust_handler, times)
        .await
        .map_err(crate::error::to_napi)?;
      Ok(crate::disposable::Disposable::wrap(disposable))
    })
    .build(env)
  }

  /// Playwright: `page.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`); the
  /// handler receives a live `WebSocketRoute`.
  #[napi(
    ts_args_type = "url: string | RegExp, handler: (ws: WebSocketRoute) => void",
    ts_return_type = "Promise<void>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn route_web_socket(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either<String, crate::types::JsRegExpLike>,
    handler: napi::bindgen_prelude::Function<'_, crate::web_socket_route::WebSocketRouteArg, ()>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    use napi::bindgen_prelude::Either;
    let matcher = match url {
      Either::A(glob) => ferridriver::url_matcher::UrlMatcher::glob(glob).map_err(crate::error::to_napi)?,
      Either::B(re) => {
        ferridriver::url_matcher::UrlMatcher::regex_from_source(&re.source, re.flags.as_deref().unwrap_or(""))
          .map_err(crate::error::to_napi)?
      },
    };
    let rust_handler = crate::web_socket_route::build_ws_handler(handler)?;
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      inner
        .route_web_socket(matcher, rust_handler)
        .await
        .map_err(crate::error::to_napi)
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

  /// `page.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  #[napi]
  pub async fn unroute_all(&self, options: Option<UnrouteAllOptions>) -> Result<()> {
    let behavior = options
      .and_then(|o| o.behavior)
      .map(|b| parse_unroute_behavior(&b))
      .transpose()?
      .unwrap_or_default();
    self
      .inner
      .unroute_all(Some(behavior))
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `page.routeFromHAR(har, options?)`. Replays recorded
  /// responses from a `.har` file or `.zip` archive. Recording
  /// (`update: true`) is context-scoped — use
  /// `context.routeFromHAR(har, { update: true })`.
  #[napi(
    js_name = "routeFromHAR",
    ts_args_type = "har: string, options?: { url?: string | RegExp, notFound?: 'abort' | 'fallback', update?: boolean, updateContent?: 'attach' | 'embed', updateMode?: 'minimal' | 'full' }"
  )]
  pub async fn route_from_har(&self, har: String, options: Option<RouteFromHarOptionsJs>) -> Result<()> {
    let opts = crate::page::parse_har_options(options)?;
    self
      .inner
      .route_from_har(std::path::Path::new(&har))
      .options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// `page.addLocatorHandler(locator, handler, options?: { times?, noWaitAfter? })`.
  /// Registers `handler` to run whenever `locator` becomes visible during an
  /// actionability wait, dismissing overlays/modals that block actions.
  /// Mirrors Playwright `client/page.ts:397`.
  #[napi(
    ts_args_type = "locator: Locator, handler: (locator: Locator) => unknown | Promise<unknown>, options?: AddLocatorHandlerOptions"
  )]
  pub fn add_locator_handler(
    &self,
    locator: &Locator,
    handler: napi::bindgen_prelude::Function<'_, Locator, LocatorHandlerReturn>,
    options: Option<AddLocatorHandlerOptions>,
  ) -> Result<()> {
    let times = options.as_ref().and_then(|o| o.times);
    let no_wait_after = options.as_ref().and_then(|o| o.no_wait_after).unwrap_or(false);
    if times == Some(0) {
      return Ok(());
    }
    let tsfn = handler
      .build_threadsafe_function::<Locator>()
      .callee_handled::<false>()
      .weak::<false>()
      .max_queue_size::<0>()
      .build()?;
    let tsfn = std::sync::Arc::new(tsfn);
    let cb: ferridriver::locator_handler::LocatorHandlerFn = std::sync::Arc::new(move |loc| {
      let tsfn = std::sync::Arc::clone(&tsfn);
      Box::pin(async move {
        let napi_loc = Locator::wrap(loc);
        match tsfn.call_async(napi_loc).await {
          Ok(napi::Either::A(())) => Ok(()),
          Ok(napi::Either::B(p)) => {
            let _ = p.await;
            Ok(())
          },
          Err(_) => Ok(()),
        }
      })
    });
    self
      .inner
      .add_locator_handler(locator.core(), cb, times, no_wait_after)
      .map_err(crate::error::to_napi)
  }

  /// `page.removeLocatorHandler(locator)`. Removes handlers registered for
  /// `locator`. Mirrors Playwright `client/page.ts:423`.
  #[napi]
  pub fn remove_locator_handler(&self, locator: &Locator) {
    self.inner.remove_locator_handler(locator.core());
  }

  /// `page.pickLocator(): Promise<Locator>`. Highlights elements under the
  /// cursor and resolves with a Locator for the element the user clicks.
  #[napi]
  pub async fn pick_locator(&self) -> Result<Locator> {
    self
      .inner
      .pick_locator()
      .await
      .map(Locator::wrap)
      .map_err(crate::error::to_napi)
  }

  /// `page.cancelPickLocator(): Promise<void>`.
  #[napi]
  pub async fn cancel_pick_locator(&self) -> Result<()> {
    self.inner.cancel_pick_locator().await.map_err(crate::error::to_napi)
  }

  /// `page.hideHighlight(): Promise<void>`.
  #[napi]
  pub async fn hide_highlight(&self) -> Result<()> {
    self.inner.hide_highlight().await.map_err(crate::error::to_napi)
  }
}

/// Playwright `page.unrouteAll({ behavior })` option bag.
#[napi(object)]
pub struct UnrouteAllOptions {
  /// `'wait' | 'ignoreErrors' | 'default'`.
  #[napi(ts_type = "'wait' | 'ignoreErrors' | 'default'")]
  pub behavior: Option<String>,
}

pub(crate) fn parse_unroute_behavior(behavior: &str) -> Result<ferridriver::options::UnrouteBehavior> {
  match behavior {
    "default" => Ok(ferridriver::options::UnrouteBehavior::Default),
    "wait" => Ok(ferridriver::options::UnrouteBehavior::Wait),
    "ignoreErrors" => Ok(ferridriver::options::UnrouteBehavior::IgnoreErrors),
    other => Err(napi::Error::from_reason(format!(
      "unrouteAll: invalid behavior {other:?} (expected 'wait', 'ignoreErrors', or 'default')"
    ))),
  }
}

/// Playwright `routeFromHAR` options bag (shared by Page and
/// BrowserContext).
#[napi(object)]
pub struct RouteFromHarOptionsJs {
  /// Only serve/record requests whose URL matches this glob or `RegExp`.
  #[napi(ts_type = "string | RegExp")]
  pub url: Option<napi::bindgen_prelude::Either<String, crate::types::JsRegExpLike>>,
  /// `'abort' | 'fallback'` — action when no recorded entry matches.
  #[napi(ts_type = "'abort' | 'fallback'")]
  pub not_found: Option<String>,
  /// Record network into the HAR instead of replaying (written when the
  /// context closes).
  pub update: Option<bool>,
  /// `'attach' | 'embed'` — body policy for `update` recording.
  #[napi(ts_type = "'attach' | 'embed'")]
  pub update_content: Option<String>,
  /// `'minimal' | 'full'` — detail mode for `update` recording.
  #[napi(ts_type = "'minimal' | 'full'")]
  pub update_mode: Option<String>,
}

/// Parse the `routeFromHAR` options bag into the core options. `notFound`
/// defaults to `abort` (Playwright default).
pub(crate) fn parse_har_options(
  options: Option<RouteFromHarOptionsJs>,
) -> Result<ferridriver::har::RouteFromHarOptions> {
  use napi::bindgen_prelude::Either;
  let mut out = ferridriver::har::RouteFromHarOptions::default();
  let Some(o) = options else { return Ok(out) };
  out.url = match o.url {
    Some(Either::A(glob)) => Some(ferridriver::url_matcher::UrlMatcher::glob(glob).map_err(crate::error::to_napi)?),
    Some(Either::B(re)) => Some(
      ferridriver::url_matcher::UrlMatcher::regex_from_source(&re.source, re.flags.as_deref().unwrap_or(""))
        .map_err(crate::error::to_napi)?,
    ),
    None => None,
  };
  match o.not_found.as_deref() {
    Some("fallback") => out.not_found = ferridriver::har::HarNotFound::Fallback,
    Some("abort") | None => out.not_found = ferridriver::har::HarNotFound::Abort,
    Some(other) => {
      return Err(napi::Error::from_reason(format!(
        "routeFromHAR: invalid notFound {other:?} (expected 'abort' or 'fallback')"
      )));
    },
  }
  out.update = o.update.unwrap_or(false);
  out.update_content = match o.update_content.as_deref() {
    Some("attach") => Some(ferridriver::tracing::HarContentPolicy::Attach),
    Some("embed") => Some(ferridriver::tracing::HarContentPolicy::Embed),
    None => None,
    Some(other) => {
      return Err(napi::Error::from_reason(format!(
        "routeFromHAR: invalid updateContent {other:?} (expected 'attach' or 'embed')"
      )));
    },
  };
  out.update_mode = match o.update_mode.as_deref() {
    Some("minimal") => Some(ferridriver::tracing::HarMode::Minimal),
    Some("full") => Some(ferridriver::tracing::HarMode::Full),
    None => None,
    Some(other) => {
      return Err(napi::Error::from_reason(format!(
        "routeFromHAR: invalid updateMode {other:?} (expected 'minimal' or 'full')"
      )));
    },
  };
  Ok(out)
}

#[napi(object)]
pub struct MouseClickOptions {
  pub button: Option<String>,
  #[napi(js_name = "clickCount")]
  pub click_count: Option<i32>,
  pub delay: Option<f64>,
}

/// Playwright `{ delay? }` for `keyboard.press`.
#[napi(object)]
pub struct KeyDelayOptions {
  pub delay: Option<f64>,
}

/// Playwright `{ delay?, namedKeys? }` for `keyboard.type`.
#[napi(object)]
pub struct KeyTypeOptions {
  pub delay: Option<f64>,
  pub named_keys: Option<bool>,
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
    self
      .page
      .keyboard()
      .press(&key)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `keyboard.type(text, options?: { delay?, namedKeys? })`.
  #[napi(js_name = "type")]
  pub async fn type_text(&self, text: String, options: Option<KeyTypeOptions>) -> Result<()> {
    let opts = options.map(|o| ferridriver::page::KeyboardTypeOptions {
      delay: o.delay.map(crate::types::f64_to_u64),
      named_keys: o.named_keys,
    });
    self
      .page
      .keyboard()
      .r#type(&text)
      .maybe_options(opts)
      .await
      .map_err(crate::error::to_napi)
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
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
      delay: options.as_ref().and_then(|o| o.delay).map(crate::types::f64_to_u64),
    };
    self
      .page
      .mouse()
      .click(x, y)
      .options(opts)
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
    let mut action = self.page.mouse().r#move(x, y);
    if let Some(steps) = step_count {
      action = action.steps(steps);
    }
    action.await.map_err(crate::error::to_napi)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn dblclick(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: None,
      delay: options.as_ref().and_then(|o| o.delay).map(crate::types::f64_to_u64),
    };
    self
      .page
      .mouse()
      .dblclick(x, y)
      .options(opts)
      .await
      .map_err(crate::error::to_napi)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Playwright: `mouse.down(options?: { button?, clickCount? })`.
  #[napi]
  pub async fn down(&self, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseDownOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self
      .page
      .mouse()
      .down()
      .options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `mouse.up(options?: { button?, clickCount? })`.
  #[napi]
  pub async fn up(&self, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseUpOptions {
      button: options
        .as_ref()
        .and_then(|o| o.button.as_deref())
        .map(ferridriver::options::MouseButton::from),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self
      .page
      .mouse()
      .up()
      .options(opts)
      .await
      .map_err(crate::error::to_napi)
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
  /// `TouchEvent` where the `Touch` constructor is usable; falls back
  /// to a synthesized `PointerEvent` + click where it is not (WebKit on
  /// Linux and macOS throws on `new Touch(...)`).
  #[napi]
  pub async fn tap(&self, x: f64, y: f64) -> Result<()> {
    self.page.touchscreen().tap(x, y).await.map_err(crate::error::to_napi)
  }
}
