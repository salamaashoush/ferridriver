//! Locator retry classification, lazy Playwright call-log formatting, and cold-path trace span setup.

use std::sync::Arc;

/// The call-log line for a retried attempt, in Playwright's wording:
/// `error:notinviewport` is "element is outside of the viewport",
/// `error:not<state>` is "element is not <state>".
pub(crate) fn action_log_line(msg: &str) -> String {
  // `server/dom.ts::_retryAction`'s lines for `selectOption`'s markers.
  if msg.contains("error:optionsnotfound") {
    return "did not find some options".to_string();
  }
  if msg.contains("error:optionnotenabled") {
    return "option being selected is not enabled".to_string();
  }
  let Some((_, after)) = msg.split_once("error:not") else {
    return msg.strip_prefix("backend error: ").unwrap_or(msg).to_string();
  };
  let state: String = after.chars().take_while(char::is_ascii_alphanumeric).collect();
  match state.as_str() {
    "inviewport" => "element is outside of the viewport".to_string(),
    "connected" => "element is not attached to the DOM".to_string(),
    _ => format!("element is not {state}"),
  }
}

/// Cold-path trace span setup for [`retry_resolve!`]. Kept out of the macro's
/// inline state machine so locator action futures stay compact when tracing is
/// inactive.
pub(crate) async fn begin_locator_trace(
  page: &Arc<crate::page::Page>,
  selector: &str,
  op: &str,
  extra: serde_json::Value,
) -> Option<crate::trace::ActionSpan> {
  let composite = page.context().map(crate::context::ContextRef::composite);
  let mut params = serde_json::json!({ "selector": selector });
  if let (Some(dst), Some(src)) = (params.as_object_mut(), extra.as_object()) {
    for (k, v) in src {
      dst.insert(k.clone(), v.clone());
    }
  }
  let span = crate::trace::begin_action(
    composite.as_deref(),
    "Locator",
    op,
    Some(format!("page@{}", page.backend_page_id())),
    params,
  );
  if span.is_some() {
    crate::trace::open_action(page.snapshot_before(span).await).await
  } else {
    None
  }
}

struct CallLogInner {
  lines: Vec<String>,
  last_line: String,
}

/// Lazy Playwright `Call log:` builder. Allocates nothing and skips
/// `as_locator` selector parsing unless the action actually traces, retries,
/// aborts, or times out.
pub(crate) struct LazyCallLog<'a> {
  selector: &'a str,
  op_name: &'a str,
  attempted: bool,
  inner: Option<Box<CallLogInner>>,
}

impl<'a> LazyCallLog<'a> {
  pub(crate) fn new(selector: &'a str, op_name: &'a str) -> Self {
    Self {
      selector,
      op_name,
      attempted: false,
      inner: None,
    }
  }

  fn ensure_inner(&mut self) -> &mut CallLogInner {
    let selector = self.selector;
    let op_name = self.op_name;
    let attempted = self.attempted;
    self.inner.get_or_insert_with(|| {
      let mut lines = Vec::with_capacity(if attempted { 2 } else { 1 });
      lines.push(format!(
        "waiting for {}",
        crate::locator_generator::as_locator(selector)
      ));
      if attempted {
        lines.push(format!("attempting {op_name} action"));
      }
      Box::new(CallLogInner {
        lines,
        last_line: String::new(),
      })
    })
  }

  pub(crate) fn waiting_line(&mut self) -> &str {
    self.ensure_inner().lines.first().map_or("", String::as_str)
  }

  pub(crate) fn mark_attempted(&mut self) {
    if !self.attempted {
      self.attempted = true;
      if let Some(inner) = self.inner.as_deref_mut()
        && inner.lines.len() == 1
      {
        inner.lines.push(format!("attempting {} action", self.op_name));
      }
    }
  }

  pub(crate) fn record_retry(&mut self, line: &str) -> bool {
    if self
      .inner
      .as_deref()
      .is_some_and(|inner| line == inner.last_line.as_str())
    {
      return false;
    }
    self.attempted = true;
    let op_name = self.op_name;
    let inner = self.ensure_inner();
    inner.lines.push(format!("  {line}"));
    inner.lines.push(format!("retrying {op_name} action"));
    inner.last_line.clear();
    inner.last_line.push_str(line);
    true
  }

  pub(crate) fn push_aborted(&mut self, error: &crate::error::FerriError) {
    self
      .ensure_inner()
      .lines
      .push(format!("operation was aborted: {error}"));
  }

  pub(crate) fn into_vec(mut self) -> Vec<String> {
    let _ = self.ensure_inner();
    self.inner.map_or_else(Vec::new, |inner| inner.lines)
  }
}

/// Whether a failed attempt is tried again: never once its input was
/// sent, otherwise as [`is_retryable_action_error`] says.
pub(crate) fn retries(error: &crate::error::FerriError) -> bool {
  !matches!(error, crate::error::FerriError::InputSent { .. }) && is_retryable_action_error(&error.to_string())
}

/// Should a failed action attempt be retried (re-resolve + try again
/// until the action timeout), rather than surfaced as a hard error?
///
/// Two families:
/// 1. Actionability signals from `checkElementStates`
///    (`error:notvisible` / `error:notenabled` / `error:noteditable`,
///    "not connected", "detached") — the element exists but is not yet
///    actionable.
/// 2. Stale-handle / navigated-away backend errors — the resolved node
///    vanished mid-action (React re-mount, SPA route swap, in-flight
///    navigation). Playwright's actionability loop re-resolves and
///    retries these; ferridriver previously surfaced the raw protocol
///    error (e.g. CDP "Object id doesn't reference a Node"), which
///    escaped callers' try/catch and made every real SPA interaction
///    fragile.
pub(crate) fn is_retryable_action_error(msg: &str) -> bool {
  msg.contains("not connected")
    || msg.contains("not found")
    || msg.contains("detached")
    || msg.contains("error:not")
    // `selectOption` before its options render, or while the one asked
    // for is disabled: Playwright waits for both (`server/dom.ts`).
    || msg.contains("error:optionsnotfound")
    || msg.contains("error:optionnotenabled")
    // CDP stale-node / stale-context messages.
    || msg.contains("reference a Node")
    || msg.contains("Could not find node")
    || msg.contains("No node with given")
    || msg.contains("Node with given id")
    || msg.contains("belong to the document")
    || msg.contains("Execution context was destroyed")
    || msg.contains("Cannot find context with specified id")
    // WebKit / BiDi stale-node phrasings.
    || msg.contains("Node was not found")
    || msg.contains("no such node")
    || msg.contains("stale element")
    // A WebKit target destroyed under us — the old side of a
    // cross-process navigation. The page swaps to the committed target,
    // so the very next attempt runs on a live session.
    || msg.contains("transport closed")
    // Hit-target interception: another element covers the action point.
    // Retryable so the Locator loop keeps running locator handlers and
    // re-resolving until its deadline (Playwright retries the pointer
    // action on `hitTargetDescription` the same way).
    || msg.contains("intercepts pointer events")
}
