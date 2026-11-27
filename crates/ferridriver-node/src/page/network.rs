//! Page network interception (`route`, `unroute`, `routeFromHAR`, `routeWebSocket`)
//! and locator-handler / picker helpers.

use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use crate::locator::Locator;

use super::Page;
use super::events::{AddLocatorHandlerOptions, JsUrl, LocatorHandlerReturn, PredReturn};

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  reason = "napi-derive takes JS strings, functions and option bags only by value"
)]
impl Page {
  // ── Network interception ─────────────────────────────────────────────

  /// Route network requests matching a glob pattern, `RegExp`, or predicate.
  ///
  /// The handler receives a `Route` object with request details and must call
  /// one of `route.fulfill()`, `route.continue()`, or `route.abort()`.
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
    let prepared = self.routes.prepare_route(url, &handler, options)?;
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = prepared.spec.build()?;
      let disposable = inner
        .route(matcher, prepared.handler, prepared.times)
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
    let (matcher, rust_handler) = crate::route_registry::RouteRegistry::prepare_ws_route(url, &handler)?;
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
    let Some(unroute) = self.routes.prepare_unroute(env, url, handler)? else {
      return napi::bindgen_prelude::AsyncBlockBuilder::new(async move { Ok(()) }).build(&env);
    };
    let inner = Arc::clone(&self.inner);
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      for spec in unroute.specs {
        let m = spec.build()?;
        inner
          .unroute(&m, unroute.handler_id)
          .await
          .map_err(crate::error::to_napi)?;
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
      .map_err(crate::error::to_napi)?;
    self.routes.clear();
    Ok(())
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
