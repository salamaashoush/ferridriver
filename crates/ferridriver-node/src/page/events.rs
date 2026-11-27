//! Page event registration, wait helpers, and URL/predicate matchers.

use std::sync::Arc;

use napi::Result;
use napi::bindgen_prelude::{JsObjectValue as _, JsValue as _};
use napi_derive::napi;

use crate::error::IntoNapi;

use super::Page;

/// The live object a page event carries to its listener / waiter —
/// Playwright's per-event `PageEventsMap` union materialised as a
/// 10-way `Either`. `frameattached` / `framedetached` / `framenavigated`
/// deliver a live `Frame`; `load` / `domcontentloaded` / `close`
/// deliver the `Page` itself; `pageerror` a native `Error`; everything
/// else its lifecycle class. Shared by `page.on` / `page.once`
/// (threadsafe-function dispatch) and `page.waitForEvent`.
pub(crate) type PageWaitForEventResult = napi::bindgen_prelude::Either11<
  crate::network::Request,
  crate::network::Response,
  crate::network::WebSocket,
  crate::identity::DialogObject,
  crate::file_chooser::FileChooser,
  crate::download::Download,
  crate::console_message::ConsoleMessage,
  crate::web_error::JsErrorValue,
  crate::identity::FrameObject,
  crate::identity::PageObject,
  crate::identity::WorkerObject,
>;

/// Lift a core [`PageEvent`] into the live-object union listeners
/// receive. Mirrors Playwright's listener argument per event type.
fn live_event_arg(page: &Arc<ferridriver::Page>, ev: ferridriver::events::PageEvent) -> PageWaitForEventResult {
  use ferridriver::events::PageEvent;
  use napi::bindgen_prelude::Either11;
  match ev {
    PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => {
      Either11::A(crate::network::Request::from_core_with_page(r, Arc::clone(page)))
    },
    PageEvent::Response(r) => Either11::B(crate::network::Response::from_core_with_page(r, Arc::clone(page))),
    PageEvent::WebSocket(ws) => Either11::C(crate::network::WebSocket::from_core(ws)),
    PageEvent::Dialog(d) | PageEvent::DialogClosed(d) => Either11::D(crate::identity::DialogObject(d)),
    PageEvent::FileChooser(fc) => Either11::E(crate::file_chooser::FileChooser::from_core(fc)),
    PageEvent::Download(d) => Either11::F(crate::download::Download::from_core(d)),
    PageEvent::Console(msg) => Either11::G(crate::console_message::ConsoleMessage::from_core(msg)),
    // Playwright's `'pageerror'` listener receives a native JS `Error`;
    // `JsErrorValue::to_napi_value` constructs one on the JS thread.
    PageEvent::PageError(err) => Either11::H(crate::web_error::JsErrorValue::from_details(err.error())),
    PageEvent::FrameAttached(info)
    | PageEvent::FrameNavigated(info)
    | PageEvent::FrameNavigatedWithinDocument(info) => {
      Either11::I(crate::identity::FrameObject(page.frame_for_id(&info.frame_id)))
    },
    PageEvent::FrameDetached { frame_id } => Either11::I(crate::identity::FrameObject(page.frame_for_id(&frame_id))),
    PageEvent::Load | PageEvent::DomContentLoaded | PageEvent::Close | PageEvent::Crash => {
      Either11::J(crate::identity::PageObject(Arc::clone(page)))
    },
    PageEvent::Popup(popup) => Either11::J(crate::identity::PageObject(popup)),
    PageEvent::Worker(worker) => Either11::K(crate::identity::WorkerObject(worker)),
  }
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

#[expect(unsafe_code, reason = "napi-rs declares ToNapiValue::to_napi_value unsafe")]
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

/// `waitForURL`'s `url`: a glob, a `RegExp`, or a `(url: URL) => boolean |
/// Promise<boolean>` that is called back on the JS thread for each URL.
pub(crate) type UrlArg<'a> = napi::bindgen_prelude::Either3<
  String,
  crate::types::JsRegExpLike,
  napi::bindgen_prelude::Function<'a, JsUrl, PredReturn>,
>;

pub(crate) fn url_condition(url: UrlArg<'_>) -> Result<ferridriver::url_matcher::UrlCondition> {
  use napi::bindgen_prelude::Either3;
  let predicate = match url {
    Either3::A(glob) => return Ok(crate::types::string_or_regex_to_rust(napi::Either::A(glob))?.into()),
    Either3::B(regex) => return Ok(crate::types::string_or_regex_to_rust(napi::Either::B(regex))?.into()),
    Either3::C(predicate) => predicate,
  };
  let predicate = Arc::new(
    predicate
      .build_threadsafe_function::<JsUrl>()
      .callee_handled::<false>()
      .weak::<false>()
      .max_queue_size::<0>()
      .build()?,
  );
  Ok(ferridriver::url_matcher::UrlCondition::predicate(move |url| {
    let predicate = Arc::clone(&predicate);
    async move {
      let rejected = |e: napi::Error| ferridriver::FerriError::backend(e.reason);
      match predicate.call_async_catch(JsUrl::new(url)).await.map_err(rejected)? {
        napi::Either::A(matched) => Ok(matched),
        napi::Either::B(promise) => promise.await.map_err(rejected),
      }
    }
  }))
}

/// Resolve a predicate's `boolean | Promise<boolean>` return.
pub(crate) async fn resolve_pred(r: PredReturn) -> bool {
  match r {
    napi::Either::A(b) => b,
    napi::Either::B(p) => p.await.unwrap_or(false),
  }
}

impl Page {
  /// Shared body of `on` / `once`: build the threadsafe dispatch, keep
  /// a `FunctionRef` for `off(event, listener)` identity matching, and
  /// register on the core emitter (which filters by event name).
  pub(super) fn register_listener(
    &self,
    event: &str,
    listener: &napi::bindgen_prelude::Function<'_, PageWaitForEventResult, ()>,
    once: bool,
    front: bool,
  ) -> Result<()> {
    let fn_ref = listener.create_ref()?;
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let page = Arc::clone(&self.inner);
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      tsfn.call(
        live_event_arg(&page, ev),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    });
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    self.listener_regs.record(event, id.0, fn_ref);
    Ok(())
  }
}

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  reason = "napi-derive takes JS strings, functions and option bags only by value"
)]
impl Page {
  // ── Events (waitForEvent / waitForResponse) ─────────────────────────

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
    ts_args_type = "event: 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'websocket' | 'dialog' | 'dialogclosed' | 'filechooser' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'crash' | 'pageerror' | 'popup' | 'worker', optionsOrPredicate?: number | ((data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => boolean | Promise<boolean>) | { predicate?: (data: ConsoleMessage | Request | Response | WebSocket | Dialog | FileChooser | Download | Frame | Page | Error) => boolean | Promise<boolean>; timeout?: number; signal?: AbortSignal }",
    ts_return_type = "Promise<Request | Response | WebSocket | Dialog | FileChooser | Download | ConsoleMessage | Error | Frame | Page | Worker>"
  )]
  pub fn wait_for_event(
    &self,
    env: napi::Env,
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
    let mut signal = None;
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
        signal = crate::abort::take(&mut Some(obj.get::<crate::abort::Signal>("signal")?));
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
    let page = Arc::clone(&self.inner);

    // `dialog` / `filechooser` / `download` bypass the broadcast when
    // there is no predicate — they register a one-shot handler on the
    // per-page manager, at the call so the caller's next line cannot
    // race past it. With a predicate they go through the broadcast
    // like every other event (the emitter bridge claims the live
    // handles on behalf of broadcast listeners).
    if predicate.is_none() {
      let claimed: Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<PageWaitForEventResult>> + Send>>> =
        match event_lc.as_str() {
          "dialog" => {
            let dialog = page.wait_for_dialog(timeout);
            Some(Box::pin(async move {
              let d = dialog.await.into_napi()?;
              Ok(napi::bindgen_prelude::Either11::D(crate::identity::DialogObject(d)))
            }))
          },
          "filechooser" => {
            let chooser = page.wait_for_file_chooser(timeout);
            Some(Box::pin(async move {
              let fc = chooser.await.into_napi()?;
              Ok(napi::bindgen_prelude::Either11::E(
                crate::file_chooser::FileChooser::from_core(fc),
              ))
            }))
          },
          "download" => {
            let download = page.wait_for_download(timeout);
            Some(Box::pin(async move {
              let d = download.await.into_napi()?;
              Ok(napi::bindgen_prelude::Either11::F(
                crate::download::Download::from_core(d),
              ))
            }))
          },
          _ => None,
        };
      if let Some(claimed) = claimed {
        return napi::bindgen_prelude::AsyncBlockBuilder::new(crate::abort::race(signal, claimed)).build(&env);
      }
    }

    // Broadcast-backed events: subscribe synchronously so the JS
    // caller's subsequent triggering line can't race past us. See
    // `wait_for_response` for the same pattern.
    let mut rx = self.inner.events().subscribe();
    napi::bindgen_prelude::AsyncBlockBuilder::new(crate::abort::race(signal, async move {
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
    }))
    .build(&env)
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
  pub fn wait_for_response(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, crate::network::Response, PredReturn>,
    >,
    timeout_ms: Option<f64>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::network::Response>> {
    use napi::bindgen_prelude::Either3;
    let mut rx = self.inner.events().subscribe();
    let timeout = timeout_ms.map_or_else(|| self.inner.default_timeout(), crate::types::f64_to_u64);
    let page = Arc::clone(&self.inner);
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
              let np = crate::network::Response::from_core_with_page(r.clone(), Arc::clone(&page));
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
    .build(&env)
  }
}
