//! Web workers a page started. Playwright: `class Worker`
//! (`packages/playwright-core/src/client/worker.ts`).
//!
//! A worker is a realm of its own: no DOM, no injected selector engine,
//! reachable through the backend's own session for it. Evaluation runs
//! Playwright's utility script there, so values cross with the same
//! serialisation a page's do.

use std::sync::Arc;

use crate::error::{FerriError, Result};
use crate::protocol::{SerializedArgument, SerializedValue};

/// Events a [`Worker`] emits. Playwright's `WorkerEventMap`.
#[derive(Clone)]
pub enum WorkerEvent {
  /// The worker terminated, or its page closed or navigated away.
  Close(Worker),
}

impl crate::events::EmitterEvent for WorkerEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    matches!((name, event), ("close", WorkerEvent::Close(_)))
  }
}

/// Emitter for [`WorkerEvent`]s.
pub type WorkerEventEmitter = crate::events::Emitter<WorkerEvent>;

/// The backend session a worker's evaluations go through.
pub(crate) trait WorkerSession: Send + Sync {
  /// Evaluate `expression` in the worker, awaiting a promise it returns,
  /// and answer the resulting JSON value.
  fn evaluate<'a>(&'a self, expression: &'a str) -> futures::future::BoxFuture<'a, Result<serde_json::Value>>;
}

struct WorkerInner {
  id: String,
  url: String,
  realm: Arc<dyn WorkerSession>,
  closed: tokio::sync::watch::Sender<bool>,
  events: WorkerEventEmitter,
}

/// A dedicated web worker. Cheap to clone; clones are the same worker.
#[derive(Clone)]
pub struct Worker {
  inner: Arc<WorkerInner>,
}

impl Worker {
  pub(crate) fn new(id: String, url: String, realm: Arc<dyn WorkerSession>) -> Self {
    Self {
      inner: Arc::new(WorkerInner {
        id,
        url,
        realm,
        closed: tokio::sync::watch::channel(false).0,
        events: WorkerEventEmitter::default(),
      }),
    }
  }

  /// Whether `other` is this same worker.
  #[must_use]
  pub fn is_same(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.inner, &other.inner)
  }

  /// Playwright: `worker.url(): string`.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }

  /// Whether the worker has closed.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    *self.inner.closed.borrow()
  }

  /// The worker's event emitter (`worker.on('close')`).
  #[must_use]
  pub fn events(&self) -> &WorkerEventEmitter {
    &self.inner.events
  }

  /// Mark the worker closed and tell its listeners, once.
  pub(crate) fn did_close(&self) {
    if !self.inner.closed.send_replace(true) {
      self.inner.events.emit(WorkerEvent::Close(self.clone()));
    }
  }

  /// Resolves once the worker has closed.
  pub async fn closed(&self) {
    let mut closed = self.inner.closed.subscribe();
    let _ = closed.wait_for(|closed| *closed).await;
  }

  /// Playwright: `worker.evaluate(pageFunction, arg?): Promise<R>`. Runs
  /// in the worker's global scope; `arg` crosses by value, so it cannot
  /// carry a handle from a page.
  ///
  /// # Errors
  ///
  /// Returns an error if `arg` holds handles or functions, the function
  /// throws, or the worker closes first.
  pub async fn evaluate(
    &self,
    fn_source: &str,
    arg: SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<SerializedValue> {
    if !arg.handles.is_empty() {
      return Err(FerriError::invalid_argument(
        "arg",
        "a handle belongs to its own realm and cannot be passed to a worker",
      ));
    }
    if !arg.functions.is_empty() {
      return Err(FerriError::invalid_argument(
        "arg",
        "functions cannot be passed to a worker",
      ));
    }
    if self.is_closed() {
      return Err(FerriError::target_closed(Some("Worker was closed".into())));
    }
    let empty = matches!(
      arg.value,
      SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    );
    let args: &[SerializedValue] = if empty { &[] } else { std::slice::from_ref(&arg.value) };
    let expression = evaluation_expression(fn_source, is_function, args)?;
    let mut closed = self.inner.closed.subscribe();
    let value = tokio::select! {
      value = self.inner.realm.evaluate(&expression) => value?,
      _ = closed.wait_for(|closed| *closed) => {
        return Err(FerriError::target_closed(Some("Worker was closed".into())));
      },
    };
    decode_evaluation(value)
  }
}

impl std::fmt::Debug for Worker {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Worker")
      .field("id", &self.inner.id)
      .field("url", &self.inner.url)
      .field("closed", &self.is_closed())
      .finish_non_exhaustive()
  }
}

/// [`crate::backend::cdp::UTILITY_EVAL_WRAPPER`] for a realm with no
/// window and no selector engine: the utility script installs itself on
/// `globalThis` the first time.
const WORKER_EVAL_WRAPPER: &str = "function(isFn, expr, count, serializedArgs) {\
    const fd = globalThis.__fd;\
    const parsed = count > 0 ? JSON.parse(serializedArgs) : [];\
    const us = fd.__us || (fd.__us = fd.newUtilityScript());\
    const encode = r => { const encoded = JSON.stringify(r); return encoded === undefined ? null : encoded; };\
    const result = us.evaluate(isFn, true, expr, count, ...parsed);\
    return result && typeof result.then === 'function' ? result.then(encode) : encode(result);\
  }";

/// The expression that evaluates `fn_source` with `args` in a worker and
/// answers the serialised result as a JSON string.
fn evaluation_expression(fn_source: &str, is_function: Option<bool>, args: &[SerializedValue]) -> Result<String> {
  let is_function = match is_function {
    Some(true) => "true",
    Some(false) => "false",
    None => "null",
  };
  let source = serde_json::to_string(fn_source)?;
  let serialized = serde_json::to_string(&serde_json::to_string(args)?)?;
  let utility = crate::selectors::UTILITY_SCRIPT_JS;
  Ok(format!(
    "(() => {{ if (!globalThis.__fd) {{ {utility} }} return ({WORKER_EVAL_WRAPPER})({is_function}, {source}, {count}, {serialized}); }})()",
    count = args.len()
  ))
}

/// Decode what [`evaluation_expression`] answered.
fn decode_evaluation(value: serde_json::Value) -> Result<SerializedValue> {
  match value {
    serde_json::Value::Null => Ok(SerializedValue::Special(crate::protocol::SpecialValue::Undefined)),
    serde_json::Value::String(encoded) => {
      let wire: serde_json::Value = serde_json::from_str(&encoded)
        .map_err(|error| FerriError::protocol("worker evaluate", format!("result is not JSON: {error}")))?;
      serde_json::from_value(wire)
        .map_err(|error| FerriError::protocol("worker evaluate", format!("result is not a serialised value: {error}")))
    },
    other => Err(FerriError::protocol(
      "worker evaluate",
      format!("expected the serialised result as a string, got {other}"),
    )),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_evaluation_carries_its_arguments_as_literals() {
    let expression = evaluation_expression("x => x + 1", Some(true), &[SerializedValue::Number(41.0)]).unwrap();
    assert!(expression.contains(r#""x => x + 1""#));
    assert!(
      expression.contains("(true, \"x => x + 1\", 1, \"[41.0]\")"),
      "{expression}"
    );
    assert!(expression.starts_with("(() => { if (!globalThis.__fd) {"));
  }

  #[test]
  fn a_result_decodes_from_its_wire_string() {
    assert!(matches!(
      decode_evaluation(serde_json::Value::Null).unwrap(),
      SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    ));
    assert!(matches!(
      decode_evaluation(serde_json::json!("42")).unwrap(),
      SerializedValue::Number(n) if (n - 42.0).abs() < f64::EPSILON
    ));
    assert!(decode_evaluation(serde_json::json!(42)).is_err());
  }
}
