//! Context-level event emitter, callback pump, and `waitForEvent` helpers.

use std::sync::Arc;

use rquickjs::class::Class;
use rquickjs::function::{Opt, This};
use rquickjs::{Ctx, JsLifetime, Value};
use rustc_hash::FxHashMap;

use super::BrowserContextJs;
use crate::bindings::convert::{FerriResultCtxExt, throw_named};

/// One `context.on` / `context.once` registration.
pub(super) struct ContextListenerEntry {
  pub(super) event: String,
  /// Core context name, so listeners registered through one wrapper are
  /// removed through another (`page.context()` mints a fresh wrapper
  /// per call).
  pub(super) context: String,
  pub(super) listener: crate::bindings::page::SavedCallback,
}

#[derive(Default)]
pub(super) struct ContextCallbacks {
  pub(super) listeners: FxHashMap<u64, ContextListenerEntry>,
}

#[derive(JsLifetime)]
struct ContextCallbacksUd(std::cell::RefCell<ContextCallbacks>);

pub(super) fn with_context_callbacks<R>(
  ctx: &Ctx<'_>,
  f: impl FnOnce(&mut ContextCallbacks) -> R,
) -> rquickjs::Result<R> {
  if ctx.userdata::<ContextCallbacksUd>().is_none() {
    let _ = ctx.store_userdata(ContextCallbacksUd(std::cell::RefCell::new(ContextCallbacks::default())));
  }
  let ud = ctx
    .userdata::<ContextCallbacksUd>()
    .ok_or_else(|| throw_named(ctx, "Error", "context callbacks registry missing"))?;
  let mut reg = ud.0.borrow_mut();
  Ok(f(&mut reg))
}

/// Message the context-event pump consumes. Same shape and the same
/// rules as the page pump: a core callback fires on a backend thread and
/// may only `send`; the VM is touched exclusively by the pump future,
/// which the session's event loop polls on the interpreter thread.
pub(super) enum ContextEventMsg {
  Event {
    id: u64,
    remove_after: bool,
    event: Box<ferridriver::events::ContextEvent>,
  },
  Drain(tokio::sync::oneshot::Sender<()>),
}

#[derive(JsLifetime)]
struct ContextEventPumpUd(tokio::sync::mpsc::Sender<ContextEventMsg>);

/// Capacity of the context-event pump channel — the page pump's
/// rationale and bound, applied to the lower-rate context stream.
const CONTEXT_EVENT_PUMP_CAPACITY: usize = 1024;

pub(super) fn ensure_context_event_pump(ctx: &Ctx<'_>) -> tokio::sync::mpsc::Sender<ContextEventMsg> {
  if let Some(ud) = ctx.userdata::<ContextEventPumpUd>() {
    return ud.0.clone();
  }
  let (tx, mut rx) = tokio::sync::mpsc::channel::<ContextEventMsg>(CONTEXT_EVENT_PUMP_CAPACITY);
  let pump_ctx = ctx.clone();
  ctx.spawn(async move {
    while let Some(msg) = rx.recv().await {
      let ContextEventMsg::Event {
        id,
        remove_after,
        event,
      } = msg
      else {
        if let ContextEventMsg::Drain(done) = msg {
          let _ = done.send(());
        }
        continue;
      };
      let Ok(Some(f)) = with_context_callbacks(&pump_ctx, |r| {
        let entry = r.listeners.get(&id).map(|e| e.listener.clone());
        if remove_after {
          r.listeners.remove(&id);
        }
        entry
      }) else {
        continue;
      };
      let Ok(arg) = context_event_to_js(&pump_ctx, *event) else {
        continue;
      };
      let _: rquickjs::Result<Value<'_>> = f.call_bracketed(&pump_ctx, (arg,));
    }
  });
  let _ = ctx.store_userdata(ContextEventPumpUd(tx.clone()));
  tx
}

/// Lift a context event into the live class instance Playwright hands a
/// listener — the same mapping `waitForEvent` performs.
pub(super) fn context_event_to_js<'js>(
  ctx: &Ctx<'js>,
  event: ferridriver::events::ContextEvent,
) -> rquickjs::Result<Value<'js>> {
  use ferridriver::events::ContextEvent;
  use rquickjs::IntoJs;
  match event {
    ContextEvent::Dialog(d) | ContextEvent::DialogClosed(d) => {
      crate::bindings::identity::dialog_object(ctx, d)?.into_js(ctx)
    },
    ContextEvent::WebError(err) => {
      Class::instance(ctx.clone(), crate::bindings::web_error::WebErrorJs::new(err))?.into_js(ctx)
    },
    ContextEvent::Download(d) => {
      Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(d))?.into_js(ctx)
    },
    ContextEvent::FrameAttached { page, frame_id }
    | ContextEvent::FrameDetached { page, frame_id }
    | ContextEvent::FrameNavigated { page, frame_id } => {
      crate::bindings::identity::frame_object(ctx, page.frame_for_id(&frame_id))?.into_js(ctx)
    },
    ContextEvent::Page(page) | ContextEvent::PageClose(page) | ContextEvent::PageLoad(page) => {
      crate::bindings::identity::page_object(ctx, page)?.into_js(ctx)
    },
    ContextEvent::Console(message) => Class::instance(
      ctx.clone(),
      crate::bindings::console_message::ConsoleMessageJs::new(message),
    )?
    .into_js(ctx),
    ContextEvent::Request { page, request }
    | ContextEvent::RequestFinished { page, request }
    | ContextEvent::RequestFailed { page, request } => Class::instance(
      ctx.clone(),
      crate::bindings::network::RequestJs::new_with_page(request, page),
    )?
    .into_js(ctx),
    ContextEvent::Response { page, response } => Class::instance(
      ctx.clone(),
      crate::bindings::network::ResponseJs::new_with_page(response, page),
    )?
    .into_js(ctx),
    ContextEvent::Close(context) => {
      Class::instance(ctx.clone(), BrowserContextJs::new(Arc::new(context)))?.into_js(ctx)
    },
  }
}

/// What a `context.waitForEvent` listens with, armed at the call.
enum ContextWait<'js> {
  Next(
    std::pin::Pin<Box<dyn std::future::Future<Output = ferridriver::Result<ferridriver::events::ContextEvent>> + Send>>,
  ),
  Matching(
    ferridriver::EventSubscription<ferridriver::events::ContextEvent>,
    rquickjs::Function<'js>,
  ),
}

impl BrowserContextJs {
  /// Shared core for the context emitter: persist the JS listener, then
  /// register a core callback that only forwards `(id, event)` to this
  /// context's pump. The backend thread never touches the VM.
  pub(super) fn register_listener<'js>(
    &self,
    ctx: &Ctx<'js>,
    event: &str,
    listener: rquickjs::Function<'js>,
    once: bool,
    front: bool,
  ) -> rquickjs::Result<()> {
    let saved = crate::bindings::page::SavedCallback::save(ctx, listener);
    let tx = ensure_context_event_pump(ctx);
    let id_slot = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let id_slot_cb = Arc::clone(&id_slot);
    let callback: ferridriver::events::ContextEventCallback = Arc::new(move |ev: ferridriver::events::ContextEvent| {
      let id = id_slot_cb.load(std::sync::atomic::Ordering::Relaxed);
      let msg = ContextEventMsg::Event {
        id,
        remove_after: once,
        event: Box::new(ev),
      };
      if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(msg) {
        tracing::warn!(
          listener_id = id,
          capacity = CONTEXT_EVENT_PUMP_CAPACITY,
          "context event pump full (VM idle between scripts?); dropping event"
        );
      }
    });
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    id_slot.store(id.0, std::sync::atomic::Ordering::Relaxed);
    with_context_callbacks(ctx, |r| {
      r.listeners.insert(
        id.0,
        ContextListenerEntry {
          event: event.to_string(),
          context: self.inner.name().to_string(),
          listener: saved,
        },
      );
    })?;
    Ok(())
  }

  pub(super) fn off_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    event: &str,
    listener: Option<&Value<'js>>,
  ) -> rquickjs::Result<()> {
    let name = self.inner.name().to_string();
    let Some(listener_fn) = listener.and_then(|v| v.as_function().cloned()) else {
      let ids = with_context_callbacks(ctx, |r| {
        let ids: Vec<u64> = r
          .listeners
          .iter()
          .filter(|(_, e)| e.context == name && e.event == event)
          .map(|(id, _)| *id)
          .collect();
        for id in &ids {
          r.listeners.remove(id);
        }
        ids
      })?;
      for id in ids {
        self.inner.off(ferridriver::events::ListenerId(id));
      }
      return Ok(());
    };
    let target: Value<'js> = listener_fn.into_value();
    let saved = with_context_callbacks(ctx, |r| {
      r.listeners
        .iter()
        .filter(|(_, e)| e.context == name && e.event == event)
        .map(|(id, e)| (*id, e.listener.clone()))
        .collect::<Vec<_>>()
    })?;
    for (id, cb) in saved {
      if cb.restore(ctx)?.into_value() == target {
        self.inner.off(ferridriver::events::ListenerId(id));
        with_context_callbacks(ctx, |r| r.listeners.remove(&id))?;
      }
    }
    Ok(())
  }

  pub(super) fn remove_all_listeners_impl<'js>(
    this: This<Class<'js, Self>>,
    ctx: &Ctx<'js>,
    event: Option<&str>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    {
      let context = this.0.borrow();
      let name = context.inner.name().to_string();
      let removed = with_context_callbacks(ctx, |r| {
        let ids: Vec<u64> = r
          .listeners
          .iter()
          .filter(|(_, e)| e.context == name && event.is_none_or(|ev| e.event == ev))
          .map(|(id, _)| *id)
          .collect();
        for id in &ids {
          r.listeners.remove(id);
        }
        ids
      })?;
      for id in removed {
        context.inner.off(ferridriver::events::ListenerId(id));
      }
    }
    let Some(options) = options.0.filter(|v| !v.is_undefined() && !v.is_null()) else {
      return Ok(this.0.into_value());
    };
    let wait = options
      .as_object()
      .and_then(|o| o.get::<_, Option<String>>("behavior").ok().flatten())
      .is_some_and(|behavior| behavior == "wait");
    let tx = ensure_context_event_pump(ctx);
    let promise = rquickjs::promise::Promise::wrap_future(ctx, async move {
      if wait {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if tx.send(ContextEventMsg::Drain(done_tx)).await.is_ok() {
          let _ = done_rx.await;
        }
      }
      Ok::<(), rquickjs::Error>(())
    })?;
    Ok(promise.into_value())
  }

  pub(super) fn listeners_impl<'js>(&self, ctx: &Ctx<'js>, event: &str) -> rquickjs::Result<Vec<Value<'js>>> {
    let name = self.inner.name().to_string();
    let saved = with_context_callbacks(ctx, |r| {
      r.listeners
        .iter()
        .filter(|(_, e)| e.context == name && e.event == event)
        .map(|(id, e)| (*id, e.listener.clone()))
        .collect::<Vec<_>>()
    })?;
    let mut out = Vec::with_capacity(saved.len());
    for id in self.inner.listener_ids_named(event) {
      if let Some((_, cb)) = saved.iter().find(|(saved_id, _)| *saved_id == id.0) {
        out.push(cb.restore(ctx)?.into_value());
      }
    }
    Ok(out)
  }

  pub(super) fn wait_for_event_impl<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js> {
    use ferridriver::events::EmitterEvent;
    let armed = crate::bindings::convert::arm_now(&ctx, || {
      let mut timeout_ms: Option<f64> = None;
      let mut predicate: Option<rquickjs::Function<'js>> = None;
      if let Some(value) = options.0 {
        if let Some(n) = value.as_number() {
          timeout_ms = Some(n);
        } else if let Some(f) = value.as_function() {
          predicate = Some(f.clone());
        } else if let Some(bag) = value.as_object() {
          let timeout: Value<'js> = bag.get("timeout")?;
          timeout_ms = timeout.as_number();
          let found: Value<'js> = bag.get("predicate")?;
          predicate = found.as_function().cloned();
        }
      }
      let timeout = timeout_ms.map_or_else(|| self.inner.default_timeout(), crate::bindings::convert::ms_f64_to_u64);
      let armed = match predicate {
        None => ContextWait::Next(Box::pin(self.inner.wait_for_event(&event, timeout))),
        Some(predicate) => ContextWait::Matching(self.inner.events().subscribe(), predicate),
      };
      Ok((timeout, armed))
    });
    rquickjs::promise::Promised::from(async move {
      let (timeout, armed) = armed.map_err(|deferred| deferred.raise(&ctx))?;
      let (mut rx, predicate) = match armed {
        ContextWait::Next(next) => {
          let ev = next.await.into_js_with(&ctx)?;
          return context_event_to_js(&ctx, ev);
        },
        ContextWait::Matching(rx, predicate) => (rx, predicate),
      };
      let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout));
      loop {
        let next = match deadline {
          Some(deadline) => tokio::time::timeout_at(deadline, rx.recv()).await.ok(),
          None => Some(rx.recv().await),
        };
        let Some(next) = next else {
          return Err(throw_named(
            &ctx,
            "TimeoutError",
            format!("Timeout {timeout}ms exceeded while waiting for event \"{event}\""),
          ));
        };
        let Some(ev) = next else {
          return Err(throw_named(&ctx, "Error", "context closed while waiting for event"));
        };
        if !ferridriver::events::ContextEvent::matches_name(&event, &ev) {
          continue;
        }
        let arg = context_event_to_js(&ctx, ev.clone())?;
        if crate::bindings::page::call_predicate_truthy(&predicate, arg, &ctx).await? {
          return context_event_to_js(&ctx, ev);
        }
      }
    })
  }
}
