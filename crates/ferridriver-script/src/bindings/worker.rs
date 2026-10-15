//! `WorkerJs`: a dedicated web worker of a page. Playwright `class Worker`.

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value};

use crate::bindings::convert::FerriResultCtxExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Worker")]
pub struct WorkerJs {
  #[qjs(skip_trace)]
  inner: ferridriver::worker::Worker,
}

impl WorkerJs {
  #[must_use]
  pub(crate) fn new(inner: ferridriver::worker::Worker) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl WorkerJs {
  /// Playwright: `worker.url(): string`.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
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
