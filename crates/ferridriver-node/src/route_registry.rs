//! Shared NAPI route registration table for `Page` and `BrowserContext`.
//!
//! Consolidates the `route`, `routeWebSocket`, `unroute`, and `unrouteAll`
//! matcher/predicate/handler-identity bookkeeping so both bindings behave
//! identically and release `FunctionRef` handles on `unrouteAll`.

use std::sync::{Arc, Mutex};

use napi::Result;
use napi::bindgen_prelude::{Either, Either3, Function, FunctionRef};

use crate::page::{JsUrl, MatcherSpec, PredReturn, PredRoute, resolve_pred};

/// One `route` registration: the identity of the core handler closure, plus a
/// `FunctionRef` to the JS handler that installed it for `unroute(url, fn)`.
struct RouteReg {
  handler_id: usize,
  handler_ref: FunctionRef<crate::route::Route, ()>,
}

/// Prepared arguments for core `route(...)`.
pub(crate) struct PreparedNapiRoute {
  pub(crate) spec: MatcherSpec,
  pub(crate) handler: ferridriver::route::RouteHandler,
  pub(crate) times: Option<u32>,
}

/// What `unroute` should remove on core (`None` = unknown handler no-op).
pub(crate) struct PreparedNapiUnroute {
  pub(crate) specs: Vec<MatcherSpec>,
  pub(crate) handler_id: Option<usize>,
}

/// Per-owner (`Page` or `BrowserContext`) registry of JS predicate matchers
/// and route handler refs.
pub(crate) struct RouteRegistry {
  predicate_routes: Mutex<Vec<PredRoute>>,
  route_regs: Mutex<Vec<RouteReg>>,
}

impl RouteRegistry {
  pub(crate) fn new() -> Self {
    Self {
      predicate_routes: Mutex::new(Vec::new()),
      route_regs: Mutex::new(Vec::new()),
    }
  }

  /// Build the matcher spec and threadsafe route handler, recording handler
  /// and predicate JS references for later `unroute`.
  pub(crate) fn prepare_route(
    &self,
    url: Either3<String, crate::types::JsRegExpLike, Function<'_, JsUrl, PredReturn>>,
    handler: &Function<'_, crate::route::Route, ()>,
    options: Option<crate::types::RouteOptions>,
  ) -> Result<PreparedNapiRoute> {
    let times = options.and_then(|o| o.times_u32());
    let nb = napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking;
    let handler_ref = handler.create_ref()?;
    let handler = Arc::new(
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
        Arc::new(move |route| {
          handler.call(crate::route::Route::wrap(route), nb);
        }),
      ),
      Either3::B(re) => (
        MatcherSpec::Regex {
          source: re.source,
          flags: re.flags,
        },
        Arc::new(move |route| {
          handler.call(crate::route::Route::wrap(route), nb);
        }),
      ),
      Either3::C(predicate) => {
        let pred_ref = predicate.create_ref()?;
        let ptsfn = Arc::new(
          predicate
            .build_threadsafe_function::<JsUrl>()
            .callee_handled::<false>()
            .weak::<false>()
            .max_queue_size::<0>()
            .build()?,
        );
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
          Arc::new(move |route| {
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

    Ok(PreparedNapiRoute {
      spec,
      handler: rust_handler,
      times,
    })
  }

  /// Build a core WebSocket route matcher and threadsafe handler.
  pub(crate) fn prepare_ws_route(
    url: Either<String, crate::types::JsRegExpLike>,
    handler: &Function<'_, crate::web_socket_route::WebSocketRouteArg, ()>,
  ) -> Result<(
    ferridriver::url_matcher::UrlMatcher,
    ferridriver::web_socket_route::WsHandler,
  )> {
    let matcher = match url {
      Either::A(glob) => ferridriver::url_matcher::UrlMatcher::glob(glob).map_err(crate::error::to_napi)?,
      Either::B(re) => {
        ferridriver::url_matcher::UrlMatcher::regex_from_source(&re.source, re.flags.as_deref().unwrap_or(""))
          .map_err(crate::error::to_napi)?
      },
    };
    let rust_handler = crate::web_socket_route::build_ws_handler(handler)?;
    Ok((matcher, rust_handler))
  }

  /// Resolve `unroute(urlOrPredicate, handler?)` against recorded JS refs.
  /// Returns `Ok(None)` when a specific `handler` was passed that was never
  /// registered (Playwright treats this as a no-op).
  pub(crate) fn prepare_unroute(
    &self,
    env: napi::Env,
    url: Either3<String, crate::types::JsRegExpLike, Function<'_, JsUrl, PredReturn>>,
    handler: Option<Function<'_, crate::route::Route, ()>>,
  ) -> Result<Option<PreparedNapiUnroute>> {
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
        return Ok(None);
      }
    }
    let specs: Vec<MatcherSpec> = match url {
      Either3::A(glob) => vec![MatcherSpec::Glob(glob)],
      Either3::B(re) => vec![MatcherSpec::Regex {
        source: re.source,
        flags: re.flags,
      }],
      Either3::C(predicate) => {
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
    Ok(Some(PreparedNapiUnroute { specs, handler_id }))
  }

  /// Drop all tracked handler and predicate `FunctionRef`s (for `unrouteAll`).
  pub(crate) fn clear(&self) {
    self
      .predicate_routes
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    self
      .route_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
  }
}
