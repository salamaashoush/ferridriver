//! Exposed page-binding plumbing shared by every backend.

use std::sync::Arc;

use super::PageBackref;

/// An exposed callback as a backend runs it: its outcome is the JS
/// expression the page-side call resolves with, evaluated in the calling
/// context, or the message it rejects with.
pub(crate) type PageBinding =
  Arc<dyn Fn(crate::events::BindingSource, Vec<serde_json::Value>) -> PageBindingFuture + Send + Sync>;

pub(crate) type PageBindingFuture =
  std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send>>;

/// Parks a value in the calling frame and answers with the token that
/// takes it out again. A result with handles in it has to reach the page
/// as an evaluate argument, while the call it settles is resolved by an
/// expression: the argument is parked here, the expression takes it.
const HOLD_BINDING_RESULT_JS: &str = "value => {
  let held = globalThis.__fd_held;
  if (!held) {
    const values = new Map();
    let next = 0;
    held = {
      hold(value) { values.set(++next, value); return next; },
      take(token) { const value = values.get(token); values.delete(token); return value; },
    };
    Object.defineProperty(globalThis, '__fd_held', { value: held, configurable: true });
  }
  return held.hold(value);
}";

/// The expression a binding result reaches its caller as.
pub(super) async fn binding_result_expression(
  page: &PageBackref,
  frame: &str,
  returned: crate::events::BindingReturn,
) -> std::result::Result<String, String> {
  let argument = match returned {
    crate::events::BindingReturn::Json(value) => return serde_json::to_string(&value).map_err(|e| e.to_string()),
    crate::events::BindingReturn::Argument(argument) => argument,
  };
  if argument.handles.is_empty() {
    return match argument.value.to_json_like() {
      Some(value) => serde_json::to_string(&value).map_err(|e| e.to_string()),
      None => argument.value.to_js_source().map_err(|e| e.to_string()),
    };
  }
  let page = page
    .upgrade()
    .ok_or_else(|| String::from("the page of this binding is closed"))?;
  let held = page
    .inner
    .call_utility_evaluate(
      HOLD_BINDING_RESULT_JS,
      &[argument.value],
      &argument.handles,
      Some(frame),
      Some(true),
      true,
    )
    .await
    .map_err(|e| e.to_string())?;
  let token = match held {
    crate::js_handle::EvaluateResult::Value(value) => value.to_json_like(),
    crate::js_handle::EvaluateResult::Handle(..) => None,
  }
  .and_then(|token| token.as_u64())
  .ok_or_else(|| String::from("the binding result could not be handed to the page"))?;
  Ok(format!("globalThis.__fd_held.take({token})"))
}

/// The page-side statement settling call `seq` on the `__fd_bc` binding
/// controller with an exposed callback's outcome: the expression of its
/// value, or a rejection carrying the message the callback threw.
pub(crate) fn binding_settle_js(seq: u64, outcome: &std::result::Result<String, String>) -> String {
  match outcome {
    Ok(expression) => format!("globalThis.__fd_bc.resolve({seq}, {expression})"),
    Err(message) => format!(
      "globalThis.__fd_bc.reject({seq}, {})",
      serde_json::to_string(message).unwrap_or_else(|_| "''".into())
    ),
  }
}

#[cfg(test)]
mod binding_result_tests {
  use super::{PageBackref, binding_result_expression, binding_settle_js};
  use crate::events::BindingReturn;
  use crate::protocol::{HandleId, PropertyEntry, SerializedArgument, SerializedValue};

  fn argument(value: SerializedValue, handles: Vec<HandleId>) -> BindingReturn {
    BindingReturn::Argument(SerializedArgument {
      value,
      handles,
      functions: Vec::new(),
    })
  }

  #[tokio::test]
  async fn json_and_plain_arguments_settle_as_json() {
    let page = PageBackref::new();
    let json = binding_result_expression(&page, "frame", serde_json::json!({ "a": [1, "two"] }).into()).await;
    assert_eq!(json.as_deref(), Ok(r#"{"a":[1,"two"]}"#));
    let plain = SerializedValue::object(
      1,
      vec![PropertyEntry {
        k: "n".into(),
        v: SerializedValue::number(3.0),
      }],
    );
    let plain = binding_result_expression(&page, "frame", argument(plain, Vec::new())).await;
    assert_eq!(plain.as_deref(), Ok(r#"{"n":3}"#));
  }

  #[tokio::test]
  async fn rich_arguments_settle_as_source() {
    let page = PageBackref::new();
    let undefined = binding_result_expression(&page, "frame", argument(SerializedValue::undefined(), Vec::new())).await;
    assert_eq!(undefined.as_deref(), Ok("(r => undefined)({})"));
    let date = SerializedValue::date("1970-01-02T00:00:00.000Z");
    let date = binding_result_expression(&page, "frame", argument(date, Vec::new())).await;
    assert_eq!(
      binding_settle_js(7, &date),
      r#"globalThis.__fd_bc.resolve(7, (r => new Date("1970-01-02T00:00:00.000Z"))({}))"#
    );
  }

  #[tokio::test]
  async fn a_handle_needs_the_page_it_belongs_to() {
    let handle = HandleId::WebKit("object-1".into());
    let outcome = binding_result_expression(
      &PageBackref::new(),
      "frame",
      argument(SerializedValue::handle(0), vec![handle]),
    )
    .await;
    assert_eq!(outcome, Err(String::from("the page of this binding is closed")));
    assert_eq!(
      binding_settle_js(7, &outcome),
      r#"globalThis.__fd_bc.reject(7, "the page of this binding is closed")"#
    );
  }
}
