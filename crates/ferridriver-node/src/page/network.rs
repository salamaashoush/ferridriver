//! Page network interception (`route`, `unroute`, `routeFromHAR`, `routeWebSocket`)
//! and locator-handler / picker helpers.

use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::locator::Locator;

use super::Page;
use super::events::{
  AddLocatorHandlerOptions, JsUrl, LocatorHandlerReturn, MatcherSpec, PredReturn, PredRoute, resolve_pred,
};

/// One `page.route` registration: the identity of the core handler
/// closure, plus a `FunctionRef` to the JS handler that installed it.
pub(super) struct RouteReg {
  pub(super) handler_id: usize,
  pub(super) handler_ref: napi::bindgen_prelude::FunctionRef<crate::route::Route, ()>,
}

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  reason = "napi-derive takes JS strings, functions and option bags only by value"
)]
impl Page {
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
  pub fn route(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
    handler: napi::bindgen_prelude::Function<'_, crate::route::Route, ()>,
    options: Option<crate::types::RouteOptions>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::disposable::Disposable>> {
    use napi::bindgen_prelude::Either3;
    let times = options.and_then(|o| o.times_u32());
    let nb = napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking;
    // Keep a `FunctionRef` to the handler as well as the dispatch TSFN:
    // `unroute(url, handler)` names the registration by the identity of
    // the function the caller passed.
    let handler_ref = handler.create_ref()?;
    // The route-handler TSFN is `weak` (not `Clone`); share it via `Arc`
    // so the predicate path can move it into a per-request task.
    let handler = std::sync::Arc::new(
      handler
        .build_threadsafe_function()
        .callee_handled::<false>()
        .weak::<true>()
        .max_queue_size::<0>()
        .build()?,
    );
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
            let ptsfn = Arc::clone(&ptsfn);
            let handler = Arc::clone(&handler);
            let url = JsUrl::new(route.request().url.clone());
            tokio::spawn(async move {
              let truthy = match ptsfn.call_async(url).await {
                Ok(r) => resolve_pred(r).await,
                Err(_) => false,
              };
              if truthy {
                handler.call(crate::route::Route::wrap(route), nb);
              } else {
                route.reject_as_unmatched();
              }
            });
          }),
        )
      },
    };

    self
      .route_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(RouteReg {
        handler_id: ferridriver::route::route_handler_id(&rust_handler),
        handler_ref,
      });

    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = spec.build()?;
      let disposable = inner
        .route(matcher, rust_handler, times)
        .await
        .map_err(crate::error::to_napi)?;
      Ok(crate::disposable::Disposable::wrap(disposable))
    })
    .build(&env)
  }

  /// Playwright: `page.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`); the
  /// handler receives a live `WebSocketRoute`.
  #[napi(
    ts_args_type = "url: string | RegExp, handler: (ws: WebSocketRoute) => void",
    ts_return_type = "Promise<void>"
  )]
  pub fn route_web_socket(
    &self,
    env: napi::Env,
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
    let rust_handler = crate::web_socket_route::build_ws_handler(&handler)?;
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      inner
        .route_web_socket(matcher, rust_handler)
        .await
        .map_err(crate::error::to_napi)
    })
    .build(&env)
  }

  /// `page.unroute(string | RegExp | ((url: URL) => boolean))`. A
  /// predicate is matched by `===` against the function passed to
  /// `route`, dropping its always-true core matcher by `Arc` identity.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean), handler?: (route: Route) => void",
    ts_return_type = "Promise<void>"
  )]
  pub fn unroute(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
    handler: Option<napi::bindgen_prelude::Function<'_, crate::route::Route, ()>>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    use napi::bindgen_prelude::Either3;
    // Playwright's second argument: remove only the registration made
    // with THIS handler, leaving another handler on the same pattern
    // serving. A handler that never registered anything is a no-op.
    let mut handler_id: Option<usize> = None;
    if let Some(handler) = handler {
      let in_ref = handler.create_ref()?;
      let regs = self
        .route_regs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      for reg in regs.iter() {
        let a = in_ref.borrow_back(&env)?;
        let b = reg.handler_ref.borrow_back(&env)?;
        if env.strict_equals(a, b)? {
          handler_id = Some(reg.handler_id);
          break;
        }
      }
      drop(regs);
      if handler_id.is_none() {
        return napi::bindgen_prelude::AsyncBlockBuilder::new(async move { Ok(()) }).build(&env);
      }
    }
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
            let a = in_ref.borrow_back(&env)?;
            let b = guard[i].pred_ref.borrow_back(&env)?;
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
        inner.unroute(&m, handler_id).await.map_err(crate::error::to_napi)?;
      }
      Ok(())
    })
    .build(&env)
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
      let tsfn = Arc::clone(&tsfn);
      Box::pin(async move {
        let napi_loc = Locator::wrap(loc);
        if let Ok(napi::Either::B(p)) = tsfn.call_async(napi_loc).await {
          let _ = p.await;
        }
        Ok(())
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
/// `BrowserContext`).
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
