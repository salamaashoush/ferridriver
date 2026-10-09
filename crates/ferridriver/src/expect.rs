//! One judgement of a web-first assertion, as Playwright's
//! `frames.ts::_expectInternal` makes it: resolve the selector in the
//! frame, hand the elements to the injected engine's `expect`, and fill
//! in the verdicts upstream gives when nothing matched. Polling, `.not`
//! and the failure message belong to the caller.

use serde::Serialize;
use serde_json::Value;

use crate::error::{FerriError, Result};
use crate::frame::Frame;

/// One expected text value, as `matchers.ts::serializeExpectedTextValues`
/// sends it to the page.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedText {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub string: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub regex_source: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub regex_flags: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub match_substring: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub ignore_case: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub normalize_white_space: Option<bool>,
}

/// `FrameExpectParams` without the selector.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectParams {
  pub expression: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub expression_arg: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub pseudo: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub expected_text: Option<Vec<ExpectedText>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub expected_number: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub expected_value: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub use_inner_text: Option<bool>,
  pub is_not: bool,
}

impl ExpectParams {
  #[must_use]
  pub fn new(expression: impl Into<String>, is_not: bool) -> Self {
    Self {
      expression: expression.into(),
      is_not,
      ..Self::default()
    }
  }

  fn is_array(&self) -> bool {
    self.expression == "to.have.count" || self.expression.strip_suffix(".array").is_some()
  }
}

/// What one judgement saw.
#[derive(Debug, Clone, Default)]
pub struct ExpectOutcome {
  /// The page's verdict before `.not` is applied.
  pub matches: bool,
  /// The value the page judged, for the failure message.
  pub received: Option<Value>,
  /// The accessibility tree around the failure, for the error context.
  pub aria_snapshot: Option<String>,
  /// Nothing matched and the expression has no verdict for that, so the
  /// failure reads "element(s) not found" rather than a value.
  pub missing: bool,
  /// Progress lines for the call log.
  pub log: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Envelope {
  strict: Option<usize>,
  missing: Option<bool>,
  error: Option<String>,
  matches: Option<bool>,
  received: Option<Received>,
  log: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Received {
  value: Option<Value>,
  aria_snapshot: Option<String>,
}

impl Frame {
  /// Judge `params` once against what `selector` resolves to in this
  /// frame. `None` judges the document itself (`:root`, or `body` for
  /// an aria match), which is how `toHaveTitle` and `toHaveURL` run.
  ///
  /// # Errors
  ///
  /// [`FerriError::StrictModeViolation`] when a strict single-element
  /// expression matched several elements, [`FerriError::Evaluation`] when
  /// the engine refused the expression (not an input, an invalid
  /// selector), and the backend's error when the page could not be
  /// reached. The first two are settled; retrying cannot change them.
  pub async fn expect_once(
    &self,
    selector: Option<&str>,
    strict: bool,
    params: &ExpectParams,
  ) -> Result<ExpectOutcome> {
    let frame_id = (!self.is_main_frame()).then(|| self.id().to_string());
    judge(self.page(), frame_id.as_deref(), selector, strict, params).await
  }
}

async fn judge(
  page: &crate::page::Page,
  frame_id: Option<&str>,
  selector: Option<&str>,
  strict: bool,
  params: &ExpectParams,
) -> Result<ExpectOutcome> {
  let effective = selector.unwrap_or(if params.expression == "to.match.aria" {
    "body"
  } else {
    ":root"
  });
  let parts = crate::selectors::build_parts_json(&crate::selectors::parse(effective)?);
  let params_json = serde_json::to_string(params).map_err(|e| FerriError::Json(e.to_string()))?;
  let raw = evaluate_engine(
    page,
    frame_id,
    &format!("window.__fd.expect({parts}, {params_json}, {strict})"),
  )
  .await?;
  let text = raw
    .as_ref()
    .and_then(Value::as_str)
    .ok_or_else(|| FerriError::protocol("expect", "engine returned no verdict"))?;
  let envelope: Envelope = serde_json::from_str(text).map_err(|e| FerriError::Json(e.to_string()))?;
  if let Some(count) = envelope.strict {
    return Err(FerriError::strict(effective, count));
  }
  if let Some(message) = envelope.error {
    return Err(FerriError::Evaluation(message));
  }
  let mut outcome = if envelope.missing == Some(true) {
    expect_nothing_matched(page, frame_id, params).await?
  } else {
    let received = envelope.received;
    ExpectOutcome {
      matches: envelope.matches.unwrap_or(false),
      aria_snapshot: received.as_ref().and_then(|r| r.aria_snapshot.clone()),
      received: received.and_then(|r| r.value),
      missing: false,
      log: envelope.log.into_iter().collect(),
    }
  };
  if outcome.matches == params.is_not && !outcome.missing && !outcome.received.as_ref().is_some_and(Value::is_array) {
    outcome.log.push(format!(
      "  unexpected value \"{}\"",
      render_unexpected(params, outcome.received.as_ref())
    ));
  }
  Ok(outcome)
}

/// The verdicts `_expectInternal` gives when the selector matched
/// nothing: a count of zero, an empty list, and the states absence
/// satisfies or refutes.
async fn expect_nothing_matched(
  page: &crate::page::Page,
  frame_id: Option<&str>,
  params: &ExpectParams,
) -> Result<ExpectOutcome> {
  let is_not = params.is_not;
  let expression = params.expression.as_str();
  let mut outcome = ExpectOutcome::default();
  if expression == "to.have.count" {
    outcome.log.push("  locator resolved to 0 elements".into());
    outcome.received = Some(Value::from(0));
    outcome.matches = params.expected_number == Some(0.0);
  } else if params.is_array() {
    outcome.log.push("  locator resolved to 0 elements".into());
    outcome.received = Some(Value::Array(Vec::new()));
    outcome.matches = params.expected_text.as_ref().is_none_or(Vec::is_empty);
  } else if !is_not && matches!(expression, "to.be.hidden" | "to.be.detached") {
    outcome.matches = true;
  } else if is_not && matches!(expression, "to.be.visible" | "to.be.attached" | "to.be.in.viewport") {
    outcome.matches = false;
  } else {
    outcome.matches = is_not;
    outcome.missing = true;
  }
  if outcome.matches == is_not && !params.is_array() {
    outcome.aria_snapshot = evaluate_engine(page, frame_id, "window.__fd.ariaSnapshotForExpectFailure()")
      .await?
      .and_then(|v| v.as_str().map(str::to_owned));
  }
  Ok(outcome)
}

async fn evaluate_engine(page: &crate::page::Page, frame_id: Option<&str>, expression: &str) -> Result<Option<Value>> {
  let inner = page.inner();
  inner.ensure_engine_injected().await?;
  match frame_id {
    None => inner.evaluate(expression).await,
    Some(id) => inner.evaluate_in_frame(expression, id).await,
  }
}

impl crate::locator::Locator {
  /// [`Frame::expect_once`] on this locator, through any frame hops and
  /// under its strictness.
  ///
  /// # Errors
  ///
  /// As [`Frame::expect_once`], plus a frame hop that cannot be entered.
  pub async fn expect_once(&self, params: &ExpectParams) -> Result<ExpectOutcome> {
    let (frame, selector) = self.resolved().await?;
    frame.expect_once(Some(&selector), self.is_strict(), params).await
  }
}

impl crate::page::Page {
  /// [`Frame::expect_once`] on the main frame's document.
  ///
  /// # Errors
  ///
  /// As [`Frame::expect_once`].
  pub async fn expect_once(&self, params: &ExpectParams) -> Result<ExpectOutcome> {
    judge(self, None, None, true, params).await
  }
}

/// `frames.ts::renderUnexpectedValue`, which prints a value the way
/// JavaScript's template literal would.
fn render_unexpected(params: &ExpectParams, received: Option<&Value>) -> String {
  match received {
    None => "undefined".into(),
    Some(Value::String(s)) => s.clone(),
    Some(Value::Object(o)) if params.expression == "to.match.aria" => {
      o.get("raw").and_then(Value::as_str).unwrap_or_default().to_owned()
    },
    Some(Value::Object(_)) => "[object Object]".into(),
    Some(other) => other.to_string(),
  }
}
