//! Page-event plumbing: name matching, subscription draining, event →
//! live JS object conversion, and the predicate wait loops.

use std::sync::Arc;

use ferridriver::Page;

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn match_event_name(name: &str, ev: &ferridriver::events::PageEvent) -> bool {
  ferridriver::events::event_name_matches(name, ev)
}

/// Receive the next subscribed event matching `event_lc`, skipping
/// non-matching events. `None` once the emitter is dropped.
pub(crate) async fn recv_matching(
  rx: &mut ferridriver::EventSubscription<ferridriver::events::PageEvent>,
  event_lc: &str,
) -> Option<ferridriver::events::PageEvent> {
  loop {
    let ev = rx.recv().await?;
    if match_event_name(event_lc, &ev) {
      return Some(ev);
    }
  }
}

/// Lift a core [`ferridriver::events::PageEvent`] into the live class
/// instance Playwright hands to listeners: `ConsoleMessage`, `Request`,
/// `Response`, `WebSocket`, `Dialog`, `FileChooser`, `Download`, a live
/// `Frame` for the frame events, the `Page` itself for `load` /
/// `domcontentloaded` / `close`, and a native JS `Error` for
/// `pageerror`. Mirrors the NAPI binding's `live_event_arg`; shared by
/// the `page.on` event pump and `waitForEvent`.
pub(crate) fn page_event_to_js<'js>(
  ctx: &rquickjs::Ctx<'js>,
  page: &Arc<Page>,
  ev: ferridriver::events::PageEvent,
) -> rquickjs::Result<rquickjs::Value<'js>> {
  use ferridriver::events::PageEvent;
  use rquickjs::IntoJs;
  use rquickjs::class::Class;
  match ev {
    PageEvent::Console(m) => {
      Class::instance(ctx.clone(), crate::bindings::console_message::ConsoleMessageJs::new(m))?.into_js(ctx)
    },
    PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => Class::instance(
      ctx.clone(),
      crate::bindings::network::RequestJs::new_with_page(r, page.clone()),
    )?
    .into_js(ctx),
    PageEvent::Response(r) => Class::instance(
      ctx.clone(),
      crate::bindings::network::ResponseJs::new_with_page(r, page.clone()),
    )?
    .into_js(ctx),
    PageEvent::WebSocket(ws) => {
      Class::instance(ctx.clone(), crate::bindings::network::WebSocketJs::new(ws))?.into_js(ctx)
    },
    PageEvent::Dialog(d) => Class::instance(ctx.clone(), crate::bindings::dialog::DialogJs::new(d))?.into_js(ctx),
    PageEvent::FileChooser(fc) => {
      Class::instance(ctx.clone(), crate::bindings::file_chooser::FileChooserJs::new(fc))?.into_js(ctx)
    },
    PageEvent::Download(d) => Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(d))?.into_js(ctx),
    PageEvent::PageError(err) => crate::bindings::web_error::build_native_error(ctx, err.error()),
    PageEvent::FrameAttached(info) | PageEvent::FrameNavigated(info) => Class::instance(
      ctx.clone(),
      crate::bindings::frame::FrameJs::new(page.frame_for_id(&info.frame_id)),
    )?
    .into_js(ctx),
    PageEvent::FrameDetached { frame_id } => Class::instance(
      ctx.clone(),
      crate::bindings::frame::FrameJs::new(page.frame_for_id(&frame_id)),
    )?
    .into_js(ctx),
    PageEvent::Load | PageEvent::DomContentLoaded | PageEvent::Close => {
      Class::instance(ctx.clone(), pagejs_for_ctx(ctx, page.clone()))?.into_js(ctx)
    },
  }
}

/// ECMAScript `ToBoolean` for a predicate's return value.
pub(crate) fn js_truthy(v: &rquickjs::Value<'_>) -> bool {
  if v.is_undefined() || v.is_null() {
    return false;
  }
  if let Some(b) = v.as_bool() {
    return b;
  }
  if let Some(i) = v.as_int() {
    return i != 0;
  }
  if let Some(f) = v.as_float() {
    return f != 0.0 && !f.is_nan();
  }
  if let Some(s) = v.as_string() {
    return !s.to_string().unwrap_or_default().is_empty();
  }
  true
}

/// Call a JS predicate and resolve `boolean | Promise<boolean>`.
pub(crate) async fn call_predicate_truthy<'js>(
  pred: &rquickjs::Function<'js>,
  arg: impl rquickjs::IntoJs<'js>,
  ctx: &rquickjs::Ctx<'js>,
) -> rquickjs::Result<bool> {
  let arg = arg.into_js(ctx)?;
  let mp: rquickjs::promise::MaybePromise<'js> = pred.call((arg,))?;
  let v: rquickjs::Value<'js> = mp.into_future().await?;
  Ok(js_truthy(&v))
}

/// Binding-side wait loop for a `(Request) => boolean` predicate: the
/// predicate needs a live `RequestJs`, so it runs in the JS runtime
/// while the loop drains the page event broadcast.
pub(crate) async fn wait_request_predicate<'js>(
  ctx: rquickjs::Ctx<'js>,
  page: Arc<Page>,
  pred: rquickjs::Function<'js>,
  timeout_ms: u64,
) -> rquickjs::Result<crate::bindings::network::RequestJs> {
  use ferridriver::events::PageEvent;
  use rquickjs::class::Class;
  let mut rx = page.events().subscribe();
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  loop {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
      return Err(crate::bindings::convert::throw_named(
        &ctx,
        "TimeoutError",
        format!("Timeout {timeout_ms}ms exceeded while waiting for request"),
      ));
    }
    match tokio::time::timeout(remaining, rx.recv()).await {
      Ok(Some(PageEvent::Request(req))) => {
        let probe = crate::bindings::network::RequestJs::new_with_page(req.clone(), page.clone());
        let inst = Class::instance(ctx.clone(), probe)?;
        if call_predicate_truthy(&pred, inst, &ctx).await? {
          return Ok(crate::bindings::network::RequestJs::new_with_page(req, page.clone()));
        }
      },
      Ok(Some(_)) => {},
      Ok(None) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "Error",
          "page closed while waiting for request",
        ));
      },
      Err(_) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "TimeoutError",
          format!("Timeout {timeout_ms}ms exceeded while waiting for request"),
        ));
      },
    }
  }
}

/// Response-side twin of [`wait_request_predicate`].
pub(crate) async fn wait_response_predicate<'js>(
  ctx: rquickjs::Ctx<'js>,
  page: Arc<Page>,
  pred: rquickjs::Function<'js>,
  timeout_ms: u64,
) -> rquickjs::Result<crate::bindings::network::ResponseJs> {
  use ferridriver::events::PageEvent;
  use rquickjs::class::Class;
  let mut rx = page.events().subscribe();
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  loop {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
      return Err(crate::bindings::convert::throw_named(
        &ctx,
        "TimeoutError",
        format!("Timeout {timeout_ms}ms exceeded while waiting for response"),
      ));
    }
    match tokio::time::timeout(remaining, rx.recv()).await {
      Ok(Some(PageEvent::Response(resp))) => {
        let probe = crate::bindings::network::ResponseJs::new_with_page(resp.clone(), page.clone());
        let inst = Class::instance(ctx.clone(), probe)?;
        if call_predicate_truthy(&pred, inst, &ctx).await? {
          return Ok(crate::bindings::network::ResponseJs::new_with_page(resp, page.clone()));
        }
      },
      Ok(Some(_)) => {},
      Ok(None) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "Error",
          "page closed while waiting for response",
        ));
      },
      Err(_) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "TimeoutError",
          format!("Timeout {timeout_ms}ms exceeded while waiting for response"),
        ));
      },
    }
  }
}
