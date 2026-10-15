//! NAPI binding for [`ferridriver::worker::Worker`]. Playwright
//! `class Worker` (`packages/playwright-core/src/client/worker.ts`).

use napi::Result;
use napi_derive::napi;

use crate::error::IntoNapi;

/// A dedicated web worker a page started.
#[napi]
pub struct Worker {
  pub(crate) inner: ferridriver::worker::Worker,
}

impl Worker {
  pub(crate) fn from_core(inner: ferridriver::worker::Worker) -> Self {
    Self { inner }
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
