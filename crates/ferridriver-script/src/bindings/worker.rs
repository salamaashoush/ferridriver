//! `WorkerJs`: a dedicated web worker of a page. Playwright `class Worker`.

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value};

use crate::bindings::convert::FerriResultCtxExt;

/// Bound on the worker-listener pump, like the page-event pump.
const WORKER_PUMP_CAPACITY: usize = 64;

type WorkerPumpMsg = (u64, ferridriver::worker::Worker);

struct WorkerEventPumpUd(tokio::sync::mpsc::Sender<WorkerPumpMsg>);

// SAFETY: holds only a `'static` channel sender, so re-stating the unused
// `'js` lifetime is sound — identical rationale to `WsEventPumpUd`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for WorkerEventPumpUd {
  type Changed<'to> = WorkerEventPumpUd;
}

/// Get (or lazily start) this context's worker-listener pump: listeners
/// run on the interpreter thread, never on the backend thread.
fn ensure_worker_pump(ctx: &Ctx<'_>) -> tokio::sync::mpsc::Sender<WorkerPumpMsg> {
  if let Some(ud) = ctx.userdata::<WorkerEventPumpUd>() {
    return ud.0.clone();
  }
  let (tx, mut rx) = tokio::sync::mpsc::channel::<WorkerPumpMsg>(WORKER_PUMP_CAPACITY);
  let pump_ctx = ctx.clone();
  ctx.spawn(async move {
    while let Some((id, worker)) = rx.recv().await {
      let Ok(Some(saved)) = crate::bindings::page::with_page_callbacks(&pump_ctx, |r| r.get_object_listener(id)) else {
        continue;
      };
      let Ok(f) = saved.restore(&pump_ctx) else { continue };
      let Ok(arg) = crate::bindings::identity::worker_object(&pump_ctx, worker) else {
        continue;
      };
      let _: rquickjs::Result<Value<'_>> = f.call((arg,));
    }
  });
  let _ = ctx.store_userdata(WorkerEventPumpUd(tx.clone()));
  tx
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Worker")]
pub struct WorkerJs {
  #[qjs(skip_trace)]
  inner: ferridriver::worker::Worker,
  /// `(registry id, event, core listener)` for each listener registered
  /// through this worker, so `off(event, fn)` can release it.
  #[qjs(skip_trace)]
  listeners: std::cell::RefCell<Vec<(u64, String, ferridriver::events::ListenerId)>>,
}

impl WorkerJs {
  #[must_use]
  pub(crate) fn new(inner: ferridriver::worker::Worker) -> Self {
    Self {
      inner,
      listeners: std::cell::RefCell::new(Vec::new()),
    }
  }

  fn register<'js>(
    &self,
    ctx: &Ctx<'js>,
    event: String,
    handler: rquickjs::Function<'js>,
    once: bool,
  ) -> rquickjs::Result<()> {
    let id = crate::bindings::page::with_page_callbacks(ctx, crate::bindings::page::PageCallbacks::next_route_id)?;
    let saved = crate::bindings::page::SavedCallback::save(ctx, handler);
    crate::bindings::page::with_page_callbacks(ctx, |r| r.insert_object_listener(id, saved))?;
    let pump = ensure_worker_pump(ctx);
    let callback: std::sync::Arc<dyn Fn(ferridriver::worker::WorkerEvent) + Send + Sync> =
      std::sync::Arc::new(move |event| {
        let ferridriver::worker::WorkerEvent::Close(worker) = event;
        let _ = pump.try_send((id, worker));
      });
    let core = if once {
      self.inner.events().once(&event, callback)
    } else {
      self.inner.events().on(&event, callback)
    };
    self.listeners.borrow_mut().push((id, event, core));
    Ok(())
  }
}

#[rquickjs::methods]
impl WorkerJs {
  /// Playwright: `worker.url(): string`.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Playwright: `worker.on('close', listener)`.
  #[qjs(rename = "on")]
  pub fn on<'js>(
    this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
    this.0.borrow().register(&ctx, event, handler, false)?;
    Ok(this.0)
  }

  /// Node's `addListener`, an alias of `on`.
  #[qjs(rename = "addListener")]
  pub fn add_listener<'js>(
    this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
    Self::on(this, ctx, event, handler)
  }

  /// One-shot `on`.
  #[qjs(rename = "once")]
  pub fn once<'js>(
    this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
    this.0.borrow().register(&ctx, event, handler, true)?;
    Ok(this.0)
  }

  /// Remove `handler` from `event`, or every listener of `event`
  /// registered through this worker.
  #[qjs(rename = "off")]
  pub fn off<'js>(
    this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    handler: Opt<rquickjs::Function<'js>>,
  ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
    {
      let worker = this.0.borrow();
      let mut listeners = worker.listeners.borrow_mut();
      let mut kept = Vec::with_capacity(listeners.len());
      for (id, name, core) in listeners.drain(..) {
        let hit = name == event
          && match &handler.0 {
            None => true,
            Some(handler) => crate::bindings::page::with_page_callbacks(&ctx, |r| r.get_object_listener(id))?
              .map(|saved| saved.restore(&ctx))
              .transpose()?
              .is_some_and(|stored| stored.as_value() == handler.as_value()),
          };
        if hit {
          crate::bindings::page::with_page_callbacks(&ctx, |r| r.remove_object_listener(id))?;
          worker.inner.events().off(core);
        } else {
          kept.push((id, name, core));
        }
      }
      *listeners = kept;
    }
    Ok(this.0)
  }

  /// Node's `removeListener`, an alias of `off`.
  #[qjs(rename = "removeListener")]
  pub fn remove_listener<'js>(
    this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    handler: Opt<rquickjs::Function<'js>>,
  ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
    Self::off(this, ctx, event, handler)
  }

  /// Playwright: `worker.evaluate(pageFunction, arg?): Promise<R>`.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate<'js>(
    &self,
    ctx: Ctx<'js>,
    page_function: Value<'js>,
    arg: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let (source, is_function) = crate::bindings::convert::extract_page_function(&ctx, page_function)?;
    let serialized = crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .evaluate(&source, serialized, is_function)
      .await
      .into_js_with(&ctx)?;
    crate::bindings::convert::serialized_value_to_quickjs(&ctx, &result)
  }

  /// Playwright: `worker.waitForEvent('close', optionsOrPredicate?)`. The
  /// second argument is a timeout in ms or a `{ timeout? }` bag.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    if !event.eq_ignore_ascii_case("close") {
      return Err(crate::bindings::convert::type_error(
        &ctx,
        format!("worker.waitForEvent: unknown event {event:?}; a worker emits \"close\""),
      ));
    }
    let timeout_ms = match options.0 {
      Some(v) if v.is_number() => v.as_number(),
      Some(v) if v.is_object() => v
        .as_object()
        .and_then(|bag| bag.get::<_, Option<f64>>("timeout").ok().flatten()),
      _ => None,
    };
    let timeout = std::time::Duration::from_millis(timeout_ms.map_or(30_000, crate::bindings::convert::ms_f64_to_u64));
    if timeout.is_zero() {
      self.inner.closed().await;
    } else if tokio::time::timeout(timeout, self.inner.closed()).await.is_err() {
      return Err(crate::bindings::convert::throw_named(
        &ctx,
        "TimeoutError",
        format!(
          "Timeout {}ms exceeded while waiting for event \"close\"",
          timeout.as_millis()
        ),
      ));
    }
    let object = crate::bindings::identity::worker_object(&ctx, self.inner.clone())?;
    rquickjs::IntoJs::into_js(object, &ctx)
  }
}
