//! NAPI binding for [`ferridriver::worker::Worker`]. Playwright
//! `class Worker` (`packages/playwright-core/src/client/worker.ts`).

use napi::{Env, Result};
use napi_derive::napi;

use crate::error::IntoNapi;

/// A dedicated web worker a page started.
#[napi]
pub struct Worker {
  pub(crate) inner: ferridriver::worker::Worker,
  /// `on` / `once` registrations, so `off(event, listener)` finds the
  /// core listener from JS function identity.
  listener_regs: std::sync::Arc<std::sync::Mutex<Vec<WorkerListenerReg>>>,
}

struct WorkerListenerReg {
  event: String,
  id: u64,
  fn_ref: napi::bindgen_prelude::FunctionRef<crate::identity::WorkerObject, ()>,
}

impl Worker {
  pub(crate) fn from_core(inner: ferridriver::worker::Worker) -> Self {
    Self {
      inner,
      listener_regs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    }
  }

  fn register_listener(
    &self,
    event: &str,
    listener: napi::bindgen_prelude::Function<'_, crate::identity::WorkerObject, ()>,
    once: bool,
  ) -> Result<()> {
    let fn_ref = listener.create_ref()?;
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let callback: std::sync::Arc<dyn Fn(ferridriver::worker::WorkerEvent) + Send + Sync> =
      std::sync::Arc::new(move |event| {
        let ferridriver::worker::WorkerEvent::Close(worker) = event;
        tsfn.call(
          crate::identity::WorkerObject(worker),
          napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
        );
      });
    let id = if once {
      self.inner.events().once(event, callback)
    } else {
      self.inner.events().on(event, callback)
    };
    self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(WorkerListenerReg {
        event: event.to_string(),
        id: id.0,
        fn_ref,
      });
    Ok(())
  }
}

/// Options for `worker.waitForEvent('close', { timeout? })`.
#[napi(object, object_to_js = false)]
pub struct WorkerWaitOptions {
  /// Maximum wait in milliseconds; 0 waits forever.
  pub timeout: Option<f64>,
}

#[napi]
impl Worker {
  /// Playwright: `worker.url(): string`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Playwright: `worker.on('close', listener)`; returns the worker so
  /// registrations chain.
  #[napi(
    ts_args_type = "event: 'close', listener: (worker: Worker) => void",
    ts_return_type = "this"
  )]
  pub fn on<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::identity::WorkerObject, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.register_listener(&event, listener, false)?;
    Ok(this.object)
  }

  /// Node's `addListener`, an alias of [`Self::on`].
  #[napi(
    ts_args_type = "event: 'close', listener: (worker: Worker) => void",
    ts_return_type = "this"
  )]
  pub fn add_listener<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::identity::WorkerObject, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.on(this, event, listener)
  }

  /// One-shot [`Self::on`].
  #[napi(
    ts_args_type = "event: 'close', listener: (worker: Worker) => void",
    ts_return_type = "this"
  )]
  pub fn once<'env>(
    &self,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::identity::WorkerObject, ()>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.register_listener(&event, listener, true)?;
    Ok(this.object)
  }

  /// Remove `listener` from `event`, or every listener of `event`.
  #[napi(
    ts_args_type = "event: 'close', listener?: (worker: Worker) => void",
    ts_return_type = "this"
  )]
  pub fn off<'env>(
    &self,
    env: Env,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: Option<napi::bindgen_prelude::Function<'_, crate::identity::WorkerObject, ()>>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    let mut regs = self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(listener) = listener else {
      self.inner.events().remove_listeners_named(&event);
      regs.retain(|r| r.event != event);
      return Ok(this.object);
    };
    let wanted = listener.create_ref()?;
    let mut i = 0;
    while i < regs.len() {
      let hit =
        regs[i].event == event && env.strict_equals(wanted.borrow_back(&env)?, regs[i].fn_ref.borrow_back(&env)?)?;
      if hit {
        let reg = regs.remove(i);
        self.inner.events().off(ferridriver::events::ListenerId(reg.id));
      } else {
        i += 1;
      }
    }
    Ok(this.object)
  }

  /// Node's `removeListener`, an alias of [`Self::off`].
  #[napi(
    ts_args_type = "event: 'close', listener?: (worker: Worker) => void",
    ts_return_type = "this"
  )]
  pub fn remove_listener<'env>(
    &self,
    env: Env,
    this: napi::bindgen_prelude::This<'env>,
    event: String,
    listener: Option<napi::bindgen_prelude::Function<'_, crate::identity::WorkerObject, ()>>,
  ) -> Result<napi::bindgen_prelude::Object<'env>> {
    self.off(env, this, event, listener)
  }

  /// Playwright: `worker.evaluate(pageFunction, arg?): Promise<R>`.
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let result = self
      .inner
      .evaluate(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `worker.waitForEvent('close', optionsOrTimeout?)`.
  #[napi(
    ts_args_type = "event: 'close', optionsOrTimeout?: number | { timeout?: number }",
    ts_return_type = "Promise<Worker>"
  )]
  pub async fn wait_for_event(
    &self,
    event: String,
    options: Option<napi::Either<f64, WorkerWaitOptions>>,
  ) -> Result<crate::identity::WorkerObject> {
    if !event.eq_ignore_ascii_case("close") {
      return Err(napi::Error::from_reason(format!(
        "worker.waitForEvent: unknown event {event:?}; a worker emits \"close\""
      )));
    }
    let timeout_ms = match options {
      Some(napi::Either::A(ms)) => Some(ms),
      Some(napi::Either::B(bag)) => bag.timeout,
      None => None,
    }
    .map_or(30_000, crate::types::f64_to_u64);
    if timeout_ms == 0 {
      self.inner.closed().await;
    } else if tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), self.inner.closed())
      .await
      .is_err()
    {
      return Err(crate::error::to_napi(ferridriver::error::FerriError::timeout(
        "waiting for event \"close\"",
        timeout_ms,
      )));
    }
    Ok(crate::identity::WorkerObject(self.inner.clone()))
  }
}
