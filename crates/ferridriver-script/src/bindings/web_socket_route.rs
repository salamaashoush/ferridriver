//! `WebSocketRouteJs` / `WebSocketRouteServerJs` — QuickJS bindings for
//! `page.routeWebSocket` / `context.routeWebSocket` (Playwright 1.60).
//!
//! `onMessage` / `onClose` callbacks fire repeatedly over a socket's
//! lifetime, from a backend thread, at arbitrary points relative to the
//! script's own `execute`. That is the loop-shaped case of the VM
//! re-entry discipline (see `page/callbacks.rs`): they MUST hand the
//! event to a `ctx.spawn` pump on the interpreter thread, never drive the
//! VM from the backend thread via `tokio::spawn` + `async_with!`. The JS
//! callbacks are stashed as `Persistent` keyed by id in the page-callbacks
//! userdata and restored inside the pump.

use std::sync::Arc;

use ferridriver::web_socket_route::{WebSocketRoute as CoreRoute, WebSocketRouteServer as CoreServer, WsMessage};
use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, IntoJs, JsLifetime, Value, class::Trace};

use crate::bindings::page::callbacks::{PageCallbacks, RouteOwner, with_page_callbacks};

/// Bound mirroring `PAGE_EVENT_PUMP_CAPACITY`: the session's VM event
/// loop drains the pump even between executes, but a burst can still
/// outrun it; when full the newest message is dropped with a warning
/// rather than growing unbounded.
const WS_PUMP_CAPACITY: usize = 1024;

/// One WS event headed for a JS callback, tagged with the callback id the
/// pump restores it by. Only `Send` data crosses the thread boundary.
enum WsPumpEvent {
  Message(WsMessage),
  Close(Option<u32>, Option<String>),
}

type WsPumpMsg = (u64, WsPumpEvent);

/// Per-context sender feeding the single WS-callback pump. Sibling of
/// `PageEventPumpUd`; same rule-1 rationale (a long-lived loop resolving
/// plain JS callbacks must stay on the interpreter thread).
struct WsEventPumpUd(tokio::sync::mpsc::Sender<WsPumpMsg>);

// SAFETY: holds only a `'static` channel sender, so re-stating the unused
// `'js` lifetime is sound — identical rationale to `PageEventPumpUd`.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for WsEventPumpUd {
  type Changed<'to> = WsEventPumpUd;
}

/// Get (or lazily start) this context's WS-callback pump. The pump future
/// lives on the QuickJS runtime executor, polled only by the session's VM
/// event loop — it cannot interleave with the script's own execute.
fn ensure_ws_pump(ctx: &Ctx<'_>) -> tokio::sync::mpsc::Sender<WsPumpMsg> {
  if let Some(ud) = ctx.userdata::<WsEventPumpUd>() {
    return ud.0.clone();
  }
  let (tx, mut rx) = tokio::sync::mpsc::channel::<WsPumpMsg>(WS_PUMP_CAPACITY);
  let pump_ctx = ctx.clone();
  ctx.spawn(async move {
    while let Some((id, ev)) = rx.recv().await {
      let Ok(Some(saved)) = with_page_callbacks(&pump_ctx, |r| r.get_ws_callback(id)) else {
        continue;
      };
      let Ok(f) = saved.restore(&pump_ctx) else { continue };
      // A throwing callback is swallowed so one bad handler can't kill the
      // pump (same policy as the page-event pump / NAPI tsfn listeners).
      // The handler body is typically `(m) => ws.send(...)`, whose `send`
      // is an async method returning a promise; the pump awaits that
      // promise so sends dispatch in order and never linger as orphaned
      // futures.
      let ret: rquickjs::Result<Value<'_>> = match ev {
        WsPumpEvent::Message(msg) => match ws_message_to_js(&pump_ctx, msg) {
          Ok(arg) => f.call((arg,)),
          Err(_) => continue,
        },
        WsPumpEvent::Close(code, reason) => f.call((code, reason)),
      };
      if let Ok(v) = ret {
        if let Some(promise) = v.as_promise() {
          let _ = promise.clone().into_future::<Value<'_>>().await;
        }
      }
    }
  });
  let _ = ctx.store_userdata(WsEventPumpUd(tx.clone()));
  tx
}

/// Build a core WS route handler that dispatches to the JS handler saved
/// under `handler_id`. Shared by `page.routeWebSocket` and
/// `context.routeWebSocket`.
///
/// The dispatch is a VM-loop job (rule 2 of the re-entry discipline): the
/// loop is always alive, so this works while the VM idles between executes
/// AND while a script is parked on a host await, and the awaited job blocks
/// the returned future until the JS handler (where `connectToServer()` /
/// `onMessage` are wired) has run, so the driver observes that state before
/// `after_handle`.
pub(crate) fn build_ws_route_handler(
  vm: crate::vm::VmHandle,
  handler_id: u64,
  owner: RouteOwner,
) -> ferridriver::web_socket_route::WsHandler {
  Arc::new(move |route| {
    let vm = vm.clone();
    let owner = owner.clone();
    Box::pin(async move {
      let _: Result<rquickjs::Result<()>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
        use rquickjs::class::Class;
        if let Some(saved) = with_page_callbacks(&ctx, |r| r.get_ws_callback(handler_id))? {
          let f = saved.restore(&ctx)?;
          let route_class = Class::instance(ctx.clone(), WebSocketRouteJs::new(route, owner))?;
          let ret: Value<'_> = f.call((route_class,))?;
          // Await an async route handler so `connectToServer()` /
          // `onMessage` set up inside it are observed before `after_handle`
          // (mirrors Playwright awaiting the route handler).
          if let Some(promise) = ret.as_promise() {
            promise.clone().into_future::<Value<'_>>().await?;
          }
        }
        Ok(())
      })
      .await;
    })
  })
}

fn ws_message_from_js(value: &Value<'_>) -> WsMessage {
  if let Some(s) = value.as_string() {
    return WsMessage::Text(s.to_string().unwrap_or_default());
  }
  if let Some(obj) = value.as_object() {
    if let Some(buf) = rquickjs::ArrayBuffer::from_object(obj.clone()) {
      if let Some(bytes) = buf.as_bytes() {
        return WsMessage::Binary(bytes.to_vec());
      }
    }
    if let Ok(ta) = rquickjs::TypedArray::<u8>::from_object(obj.clone()) {
      if let Some(bytes) = ta.as_bytes() {
        return WsMessage::Binary(bytes.to_vec());
      }
    }
  }
  WsMessage::Text(String::new())
}

fn ws_message_to_js<'js>(ctx: &Ctx<'js>, msg: WsMessage) -> rquickjs::Result<Value<'js>> {
  match msg {
    WsMessage::Text(s) => s.into_js(ctx),
    WsMessage::Binary(b) => Ok(rquickjs::TypedArray::<u8>::new(ctx.clone(), b)?.into_value()),
  }
}

fn parse_close(options: &Opt<Value<'_>>) -> (Option<u32>, Option<String>) {
  let Some(obj) = options.0.as_ref().and_then(rquickjs::Value::as_object) else {
    return (None, None);
  };
  let code = obj.get::<_, Option<f64>>("code").ok().flatten().map(|c| c as u32);
  let reason = obj.get::<_, Option<String>>("reason").ok().flatten();
  (code, reason)
}

/// Stash a JS message callback and return a core handler that forwards
/// each message to the interpreter-thread pump (never touches the VM from
/// the backend thread — rule 3 of the re-entry discipline).
fn make_msg_cb<'js>(
  ctx: &Ctx<'js>,
  cb: Function<'js>,
  owner: RouteOwner,
) -> rquickjs::Result<Arc<dyn Fn(WsMessage) + Send + Sync>> {
  let id = with_page_callbacks(ctx, PageCallbacks::next_route_id)?;
  let saved = rquickjs::Persistent::save(ctx, cb);
  with_page_callbacks(ctx, |r| r.insert_ws_callback(id, owner, saved))?;
  let tx = ensure_ws_pump(ctx);
  Ok(Arc::new(move |msg: WsMessage| {
    if tx.try_send((id, WsPumpEvent::Message(msg))).is_err() {
      tracing::warn!(callback_id = id, "WS message pump full (VM idle?); dropping message");
    }
  }))
}

type WsCloseCallback = Arc<dyn Fn(Option<u32>, Option<String>) + Send + Sync>;

/// Stash a JS close callback `(code?, reason?) => ...` (matches
/// Playwright's two-argument `onClose` handler).
fn make_close_cb<'js>(ctx: &Ctx<'js>, cb: Function<'js>, owner: RouteOwner) -> rquickjs::Result<WsCloseCallback> {
  let id = with_page_callbacks(ctx, PageCallbacks::next_route_id)?;
  let saved = rquickjs::Persistent::save(ctx, cb);
  with_page_callbacks(ctx, |r| r.insert_ws_callback(id, owner, saved))?;
  let tx = ensure_ws_pump(ctx);
  Ok(Arc::new(move |code: Option<u32>, reason: Option<String>| {
    if tx.try_send((id, WsPumpEvent::Close(code, reason))).is_err() {
      tracing::warn!(callback_id = id, "WS close pump full (VM idle?); dropping close");
    }
  }))
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebSocketRoute")]
pub struct WebSocketRouteJs {
  #[qjs(skip_trace)]
  inner: CoreRoute,
  /// Owning page/context of the route registration — `onMessage` /
  /// `onClose` callbacks registered through this route are stored
  /// under it so close-time cleanup releases them.
  #[qjs(skip_trace)]
  owner: RouteOwner,
}

impl WebSocketRouteJs {
  #[must_use]
  pub(crate) fn new(inner: CoreRoute, owner: RouteOwner) -> Self {
    Self { inner, owner }
  }
}

#[rquickjs::methods]
impl WebSocketRouteJs {
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  #[qjs(rename = "protocols")]
  pub fn protocols(&self) -> Vec<String> {
    self.inner.protocols().to_vec()
  }

  #[qjs(rename = "send")]
  pub async fn send<'js>(&self, _ctx: Ctx<'js>, message: Value<'js>) -> rquickjs::Result<()> {
    self.inner.send(ws_message_from_js(&message)).await;
    Ok(())
  }

  #[qjs(rename = "close")]
  pub async fn close<'js>(&self, _ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let (code, reason) = parse_close(&options);
    self.inner.close(code, reason).await;
    Ok(())
  }

  #[qjs(rename = "onMessage")]
  pub fn on_message<'js>(&self, ctx: Ctx<'js>, handler: Function<'js>) -> rquickjs::Result<()> {
    let cb = make_msg_cb(&ctx, handler, self.owner.clone())?;
    self.inner.on_message(cb);
    Ok(())
  }

  #[qjs(rename = "onClose")]
  pub fn on_close<'js>(&self, ctx: Ctx<'js>, handler: Function<'js>) -> rquickjs::Result<()> {
    let cb = make_close_cb(&ctx, handler, self.owner.clone())?;
    self.inner.on_close(cb);
    Ok(())
  }

  #[qjs(rename = "connectToServer")]
  pub fn connect_to_server(&self) -> WebSocketRouteServerJs {
    WebSocketRouteServerJs {
      inner: self.inner.connect_to_server(),
      owner: self.owner.clone(),
    }
  }
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebSocketRouteServer")]
pub struct WebSocketRouteServerJs {
  #[qjs(skip_trace)]
  inner: CoreServer,
  #[qjs(skip_trace)]
  owner: RouteOwner,
}

#[rquickjs::methods]
impl WebSocketRouteServerJs {
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  #[qjs(rename = "send")]
  pub async fn send<'js>(&self, _ctx: Ctx<'js>, message: Value<'js>) -> rquickjs::Result<()> {
    self.inner.send(ws_message_from_js(&message)).await;
    Ok(())
  }

  #[qjs(rename = "close")]
  pub async fn close<'js>(&self, _ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let (code, reason) = parse_close(&options);
    self.inner.close(code, reason).await;
    Ok(())
  }

  #[qjs(rename = "onMessage")]
  pub fn on_message<'js>(&self, ctx: Ctx<'js>, handler: Function<'js>) -> rquickjs::Result<()> {
    let cb = make_msg_cb(&ctx, handler, self.owner.clone())?;
    self.inner.on_message(cb);
    Ok(())
  }

  #[qjs(rename = "onClose")]
  pub fn on_close<'js>(&self, ctx: Ctx<'js>, handler: Function<'js>) -> rquickjs::Result<()> {
    let cb = make_close_cb(&ctx, handler, self.owner.clone())?;
    self.inner.on_close(cb);
    Ok(())
  }
}
