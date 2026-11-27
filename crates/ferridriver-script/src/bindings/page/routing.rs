//! Shared cross-task routing and exposed-binding helpers for `PageJs` and
//! `BrowserContextJs`.

use std::sync::Arc;

use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Value};

use super::callbacks::{
  PageCallbacks, RouteOwner, SavedCallback, binding_source_object, exposed_outcome, settle_exposed_call,
  with_page_callbacks,
};
use super::events::call_predicate_truthy;
use super::options::{parse_route_times, parse_unroute_behavior, url_value_to_matcher};
use crate::bindings::convert::throw_named;

/// Prepared core arguments for `page.route` / `context.route`.
pub(crate) struct PreparedRoute {
  pub(crate) matcher: ferridriver::url_matcher::UrlMatcher,
  pub(crate) handler: ferridriver::route::RouteHandler,
  pub(crate) times: Option<u32>,
}

/// What `page.unroute` / `context.unroute` should remove on core.
pub(crate) enum UnroutePlan {
  /// A specific `handler` function was passed that was never registered.
  Noop,
  /// One or more predicate routes matched by JS `===` identity.
  Predicates {
    matchers: Vec<ferridriver::url_matcher::UrlMatcher>,
    handler_id: Option<usize>,
  },
  /// A glob string or `RegExp` matcher.
  Single {
    matcher: ferridriver::url_matcher::UrlMatcher,
    handler_id: Option<usize>,
  },
}

/// Snapshot the JS route handler (and optional JS predicate), register them
/// under `owner`, and build the cross-task [`ferridriver::route::RouteHandler`].
///
/// Must run synchronously on the caller's stack before returning the `Promised`
/// future so `SavedCallback::save` captures the registrar's active grants.
pub(crate) fn prepare_route<'js>(
  ctx: &Ctx<'js>,
  vm: ferrijs::VmHandle,
  owner: RouteOwner,
  label: &'static str,
  url: &Value<'js>,
  handler: Function<'js>,
  options: &Opt<Value<'js>>,
) -> rquickjs::Result<PreparedRoute> {
  let times = parse_route_times(options)?;
  let id = with_page_callbacks(ctx, PageCallbacks::next_route_id)?;
  let saved_handler = SavedCallback::save(ctx, handler);

  // A JS predicate is `!Send` and core matches on the backend recv task,
  // so it cannot ride `UrlMatcher::Predicate`. Register an always-true
  // matcher with unique `Arc` identity (lets `unroute(fn)` drop exactly it
  // via `Arc::ptr_eq`); evaluate the predicate in the dispatch bridge and
  // reject as unmatched on falsy.
  let has_predicate = url.as_function().is_some();
  let (matcher, saved_pred, registry_matcher) = if let Some(pred) = url.as_function() {
    let saved_pred = SavedCallback::save(ctx, pred.clone());
    let m = ferridriver::url_matcher::UrlMatcher::predicate(|_| true);
    (m.clone(), Some(saved_pred), Some(m))
  } else {
    (url_value_to_matcher(ctx, url)?, None, None)
  };

  let rust_handler: ferridriver::route::RouteHandler = Arc::new(move |route| {
    let vm = vm.clone();
    tokio::spawn(async move {
      use rquickjs::class::Class;
      let _: Result<rquickjs::Result<()>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
        if has_predicate {
          let saved_pred = with_page_callbacks(&ctx, |r| r.get_route_pred(id))?
            .ok_or_else(|| throw_named(&ctx, "Error", format!("{label}: route predicate gone")))?;
          let pred = saved_pred.restore(&ctx)?;
          let url_ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("URL")?;
          let url_obj: Value<'_> = url_ctor.construct((route.request().url.clone(),))?;
          let truthy = call_predicate_truthy(&pred, url_obj, &ctx).await?;
          if !truthy {
            route.reject_as_unmatched();
            return Ok(());
          }
        }
        let f = with_page_callbacks(&ctx, |r| r.get_route_handler(id))?
          .ok_or_else(|| throw_named(&ctx, "Error", format!("{label}: route handler gone")))?;
        let route_class = Class::instance(ctx.clone(), crate::bindings::network::RouteJs::new(route))?;
        let _: Value<'_> = f.call_bracketed_async(&ctx, (route_class,)).await?;
        Ok(())
      })
      .await;
    });
  });

  // AFTER the core handler exists: its `Arc` identity is what
  // `unroute(url, handler)` names, so the registry remembers it alongside
  // the JS function the caller passed.
  let handler_id = ferridriver::route::route_handler_id(&rust_handler);
  with_page_callbacks(ctx, |r| {
    r.insert_route(id, owner, saved_handler, handler_id, saved_pred, registry_matcher);
  })?;

  Ok(PreparedRoute {
    matcher,
    handler: rust_handler,
    times,
  })
}

/// Snapshot the JS WebSocket route handler, register it under `owner`, and
/// build the core matcher + handler pair.
pub(crate) fn prepare_ws_route<'js>(
  ctx: &Ctx<'js>,
  vm: ferrijs::VmHandle,
  owner: RouteOwner,
  url: &Value<'js>,
  handler: Function<'js>,
) -> rquickjs::Result<(
  ferridriver::url_matcher::UrlMatcher,
  ferridriver::web_socket_route::WsHandler,
)> {
  let matcher = url_value_to_matcher(ctx, url)?;
  let handler_id = with_page_callbacks(ctx, PageCallbacks::next_route_id)?;
  let saved = SavedCallback::save(ctx, handler);
  with_page_callbacks(ctx, |r| r.insert_ws_callback(handler_id, owner.clone(), saved))?;
  let rust_handler = crate::bindings::web_socket_route::build_ws_route_handler(vm, handler_id, owner);
  Ok((matcher, rust_handler))
}

/// Resolve `unroute(url, handler?)` arguments against the callback registry
/// for `owner`.
pub(crate) fn prepare_unroute<'js>(
  ctx: &Ctx<'js>,
  owner: &RouteOwner,
  url: &Value<'js>,
  handler: &Opt<Value<'js>>,
) -> rquickjs::Result<UnroutePlan> {
  let handler_fn = handler.0.as_ref().and_then(|v| v.as_function().cloned());
  let mut handler_id: Option<usize> = None;
  if let Some(handler_fn) = handler_fn {
    let target = handler_fn.into_value();
    for (_, saved, core_id) in with_page_callbacks(ctx, |r| r.routes_for_owner(owner))? {
      if saved.restore(ctx)?.into_value() == target {
        handler_id = Some(core_id);
        break;
      }
    }
    if handler_id.is_none() {
      return Ok(UnroutePlan::Noop);
    }
  }
  if let Some(pred) = url.as_function() {
    let saved = with_page_callbacks(ctx, |r| r.predicate_routes_for_owner(owner))?;
    let mut victims: Vec<u64> = Vec::new();
    for (id, sp) in saved {
      let stored = sp.restore(ctx)?;
      if stored.as_value() == pred.as_value() {
        victims.push(id);
      }
    }
    let mut matchers = Vec::with_capacity(victims.len());
    for id in victims {
      if let Some(m) = with_page_callbacks(ctx, |r| r.remove_route(id))? {
        matchers.push(m);
      }
    }
    return Ok(UnroutePlan::Predicates { matchers, handler_id });
  }
  let matcher = url_value_to_matcher(ctx, url)?;
  Ok(UnroutePlan::Single { matcher, handler_id })
}

/// Parse `{ behavior?: 'wait' | 'ignoreErrors' | 'default' }` for `unrouteAll`.
pub(crate) fn parse_unroute_all_behavior<'js>(
  ctx: &Ctx<'js>,
  options: Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::UnrouteBehavior>> {
  match options.0.and_then(Value::into_object) {
    Some(obj) => match obj.get::<_, Option<String>>("behavior")? {
      Some(b) => Ok(Some(parse_unroute_behavior(ctx, &b)?)),
      None => Ok(None),
    },
    None => Ok(None),
  }
}

/// Stash an `exposeBinding` / `exposeFunction` callback in `PageCallbacks` and
/// build a cross-task [`ferridriver::events::ExposedBinding`].
pub(crate) fn prepare_exposed_binding<'js>(
  ctx: &Ctx<'js>,
  vm: ferrijs::VmHandle,
  page_owner: Option<usize>,
  label: &'static str,
  name: &str,
  callback: Function<'js>,
  with_source: bool,
) -> rquickjs::Result<ferridriver::events::ExposedBinding> {
  let saved = SavedCallback::save(ctx, callback);
  with_page_callbacks(ctx, |r| {
    r.exposed.insert(name.to_string(), saved);
    if let Some(page_key) = page_owner {
      r.track_exposed_owner(page_key, name.to_string());
    }
  })?;

  let name = name.to_string();
  Ok(Arc::new(move |source, args: Vec<serde_json::Value>| {
    let vm = vm.clone();
    let name = name.clone();
    Box::pin(async move {
      let out: Result<rquickjs::Result<Result<ferridriver::BindingReturn, String>>, crate::error::ScriptError> =
        crate::vm_with!(vm => |ctx| {
          let saved = with_page_callbacks(&ctx, |r| r.exposed.get(&name).cloned())?
            .ok_or_else(|| throw_named(&ctx, "Error", format!("{label}: exposed callback gone")))?;
          let f = saved.restore(&ctx)?;
          let mut call_args = rquickjs::function::Args::new_unsized(ctx.clone());
          if with_source {
            call_args.push_arg(binding_source_object(&ctx, &source)?)?;
          }
          for v in &args {
            // `json_to_js` (NOT serde): a transitive dep force-enables
            // `serde_json/arbitrary_precision`, under which the serde path
            // turns numbers into wrapper objects.
            call_args.push_arg(crate::bindings::convert::json_to_js(&ctx, v)?)?;
          }
          settle_exposed_call(&ctx, &f, call_args).await
        })
        .await;
      exposed_outcome(out)
    })
  }))
}

/// Stash a `page.exposeFunction` callback in `PageCallbacks` and build a
/// cross-task [`ferridriver::events::ExposedFn`].
pub(crate) fn prepare_exposed_fn<'js>(
  ctx: &Ctx<'js>,
  vm: ferrijs::VmHandle,
  page_owner: usize,
  label: &'static str,
  name: &str,
  callback: Function<'js>,
) -> rquickjs::Result<ferridriver::events::ExposedFn> {
  let saved = SavedCallback::save(ctx, callback);
  with_page_callbacks(ctx, |r| {
    r.exposed.insert(name.to_string(), saved);
    r.track_exposed_owner(page_owner, name.to_string());
  })?;

  let name = name.to_string();
  Ok(Arc::new(move |args: Vec<serde_json::Value>| {
    let vm = vm.clone();
    let name = name.clone();
    Box::pin(async move {
      let out: Result<rquickjs::Result<Result<ferridriver::BindingReturn, String>>, crate::error::ScriptError> =
        crate::vm_with!(vm => |ctx| {
          let saved = with_page_callbacks(&ctx, |r| r.exposed.get(&name).cloned())?
            .ok_or_else(|| throw_named(&ctx, "Error", format!("{label}: exposed callback gone")))?;
          let f = saved.restore(&ctx)?;
          let mut call_args = rquickjs::function::Args::new_unsized(ctx.clone());
          for v in &args {
            call_args.push_arg(crate::bindings::convert::json_to_js(&ctx, v)?)?;
          }
          settle_exposed_call(&ctx, &f, call_args).await
        })
        .await;
      exposed_outcome(out)
    })
  }))
}
