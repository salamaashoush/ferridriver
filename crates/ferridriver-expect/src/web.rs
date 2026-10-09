//! Web-first assertions as Playwright runs them: judge once through the
//! page's engine (`Frame::expect_once`), retry on the frame's backoff
//! until the deadline (`frames.ts::expect`), and print the failure the
//! way the matcher family does (`toBeTruthy.ts`, `toMatchText.ts`,
//! `toEqual.ts`, `matcherHint.ts::formatMatcherMessage`).

use std::time::Duration;

use ferridriver::error::FerriError;
use ferridriver::expect::{ExpectOutcome, ExpectParams, ExpectedText};
use serde_json::Value;

use crate::AssertionFailure;
use crate::value::StringOrRegex;

/// What the assertion runs against.
#[derive(Clone, Copy)]
pub(crate) enum Receiver<'a> {
  Locator(&'a ferridriver::Locator),
  Page(&'a ferridriver::Page),
}

impl Receiver<'_> {
  fn page(&self) -> &ferridriver::Page {
    match self {
      Self::Locator(locator) => locator.page(),
      Self::Page(page) => page,
    }
  }

  fn description(&self) -> Option<String> {
    match self {
      Self::Locator(locator) => Some(format!("locator('{}')", locator.selector())),
      Self::Page(_) => None,
    }
  }

  async fn judge(&self, params: &ExpectParams) -> ferridriver::Result<ExpectOutcome> {
    match self {
      Self::Locator(locator) => locator.expect_once(params).await,
      Self::Page(page) => page.expect_once(params).await,
    }
  }
}

/// How a failure prints, by upstream matcher family.
pub(crate) enum Printed {
  /// `toBeTruthy.ts`: the state's name, and the options the hint shows.
  Truthy { expected: String, arg: &'static str },
  /// `toMatchText.ts`.
  Text { expected: StringOrRegex, substring: bool },
  /// `toEqual.ts`.
  Equal(Shown),
}

/// An expected value for `toEqual.ts`'s printer.
pub(crate) enum Shown {
  Number(f64),
  Texts(Vec<StringOrRegex>),
  Json(Value),
}

pub(crate) struct WebAssert<'a> {
  pub method: &'static str,
  pub receiver: Receiver<'a>,
  pub params: ExpectParams,
  pub printed: Printed,
  pub timeout: Duration,
  pub is_soft: bool,
}

/// What the polling settled on.
struct Verdict {
  matches: bool,
  received: Option<Value>,
  aria_snapshot: Option<String>,
  error_message: Option<String>,
  timed_out: bool,
}

/// `serializeExpectedTextValues` for one value.
pub(crate) fn expected_text(
  value: &StringOrRegex,
  substring: bool,
  normalize: bool,
  ignore_case: Option<bool>,
) -> ExpectedText {
  let mut text = ExpectedText {
    match_substring: substring.then_some(true),
    normalize_white_space: normalize.then_some(true),
    ignore_case,
    ..ExpectedText::default()
  };
  match value {
    StringOrRegex::String(s) => text.string = Some(s.clone()),
    StringOrRegex::Regex(re) => {
      text.regex_source = Some(re.source().to_owned());
      text.regex_flags = Some(re.flags().to_owned());
    },
  }
  text
}

/// `frames.ts::isNonRetriableError` for what `expect_once` can raise:
/// the page refused the expression or the selector, or the page is gone.
fn settled(error: &FerriError) -> bool {
  matches!(
    error,
    FerriError::StrictModeViolation { .. }
      | FerriError::Evaluation(_)
      | FerriError::InvalidSelector { .. }
      | FerriError::TargetClosed { .. }
  )
}

/// `retryWithProgressAndBackoff`: the large steps are dropped for short
/// timeouts so a 100ms assertion still gets several looks.
fn backoff(timeout: Option<Duration>) -> Vec<u64> {
  let mut scale = vec![20, 50, 100, 100, 500];
  if let Some(timeout) = timeout {
    let limit = timeout.as_millis() / 5;
    while scale.last().is_some_and(|last| u128::from(*last) > limit) {
      scale.pop();
    }
  }
  scale.insert(0, 0);
  scale
}

impl<'a> WebAssert<'a> {
  /// Boxed so a matcher's own future stays a pointer wide: the polling
  /// state is several kilobytes and every matcher would otherwise embed it.
  pub(crate) fn run(
    self,
  ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AssertionFailure>> + Send + 'a>> {
    Box::pin(async move {
      let span = self
        .receiver
        .page()
        .begin_expect_trace(self.method, self.trace_params())
        .await;
      let result = self.poll().await;
      if let Some(span) = span {
        self
          .receiver
          .page()
          .finish_expect_trace(span, result.as_ref().err().map(|e| e.message.clone()))
          .await;
      }
      result.or_else(|failure| crate::soft::absorb(failure.with_soft(self.is_soft)))
    })
  }

  fn trace_params(&self) -> Value {
    let mut params = serde_json::to_value(&self.params).unwrap_or(Value::Null);
    if let (Value::Object(map), Receiver::Locator(locator)) = (&mut params, self.receiver) {
      map.insert("selector".into(), Value::from(locator.selector()));
    }
    if let Value::Object(map) = &mut params {
      map.insert(
        "timeout".into(),
        Value::from(u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX)),
      );
    }
    params
  }

  async fn poll(&self) -> Result<(), AssertionFailure> {
    let is_not = self.params.is_not;
    // Playwright's `timeout: 0` is no timeout at all.
    let limit = (!self.timeout.is_zero()).then_some(self.timeout);
    let deadline = limit.map(|limit| tokio::time::Instant::now() + limit);
    let mut log = CallLog::new(self);
    let mut last: Option<ExpectOutcome> = None;

    // The first look always finishes, so an already-true assertion
    // passes even under a 1ms timeout.
    match self.receiver.judge(&self.params).await {
      Ok(outcome) if outcome.matches != is_not => return Ok(()),
      Ok(outcome) => {
        log.attempt(&outcome.log);
        last = Some(outcome);
      },
      Err(error) if settled(&error) => return Err(self.failure(&Verdict::settled(&error), &log)),
      Err(_) => {},
    }

    let steps = backoff(limit);
    for step in 0.. {
      let pause = Duration::from_millis(steps[step.min(steps.len() - 1)]);
      let remaining = deadline.map(|d| d.saturating_duration_since(tokio::time::Instant::now()));
      if remaining.is_some_and(|r| r.is_zero()) {
        break;
      }
      if !pause.is_zero() {
        tokio::time::sleep(remaining.map_or(pause, |r| pause.min(r))).await;
      }
      let remaining = deadline.map(|d| d.saturating_duration_since(tokio::time::Instant::now()));
      if remaining.is_some_and(|r| r.is_zero()) {
        break;
      }
      let attempt = self.receiver.judge(&self.params);
      let judged = match remaining {
        Some(r) => match tokio::time::timeout(r, attempt).await {
          Ok(judged) => judged,
          Err(_) => break,
        },
        None => attempt.await,
      };
      match judged {
        Ok(outcome) if outcome.matches != is_not => return Ok(()),
        Ok(outcome) => {
          log.attempt(&outcome.log);
          last = Some(outcome);
        },
        Err(error) if settled(&error) => return Err(self.failure(&Verdict::settled(&error), &log)),
        Err(_) => {},
      }
    }
    let verdict = match last {
      Some(outcome) => Verdict {
        matches: outcome.matches,
        error_message: outcome.missing.then(|| "Error: element(s) not found".to_owned()),
        received: outcome.received,
        aria_snapshot: outcome.aria_snapshot,
        timed_out: true,
      },
      None => Verdict {
        matches: is_not,
        received: None,
        aria_snapshot: None,
        error_message: None,
        timed_out: true,
      },
    };
    Err(self.failure(&verdict, &log))
  }

  fn failure(&self, verdict: &Verdict, log: &CallLog) -> AssertionFailure {
    let not = if self.params.is_not { ".not" } else { "" };
    let receiver_name = if matches!(self.receiver, Receiver::Locator(_)) {
      "locator"
    } else {
      "page"
    };
    let expectation = match &self.printed {
      Printed::Truthy { arg, .. } => *arg,
      Printed::Text { .. } | Printed::Equal(_) => "expected",
    };
    let header = format!("expect({receiver_name}){not}.{}({expectation}) failed", self.method);
    let (expected, received, diff) = self.printed_lines(verdict);
    let align = verdict.error_message.is_none()
      && expected.as_deref().is_some_and(|e| e.starts_with("Expected:"))
      && received
        .as_deref()
        .is_none_or(|r| r.is_empty() || r.starts_with("Received:"));
    let pad = if align { " " } else { "" };
    let mut lines: Vec<String> = Vec::new();
    if let Some(locator) = self.receiver.description() {
      lines.push(format!("Locator: {pad}{locator}"));
    }
    lines.extend([expected, received].into_iter().flatten().filter(|l| !l.is_empty()));
    if verdict.timed_out && !self.timeout.is_zero() {
      lines.push(format!("Timeout: {pad}{}ms", self.timeout.as_millis()));
    }
    lines.extend(diff);
    lines.extend(
      verdict
        .error_message
        .as_ref()
        .map(|e| e.trim_end_matches('\n').to_owned()),
    );
    lines.push(String::new());
    lines.extend(log.render());
    let mut failure = AssertionFailure::new(header, Some(lines.join("\n")));
    failure.error_context.clone_from(&verdict.aria_snapshot);
    failure
  }

  /// `(printedExpected, printedReceived, printedDiff)` for the family,
  /// with a two-line diff already split the way `formatMatcherMessage`
  /// splits it.
  fn printed_lines(&self, verdict: &Verdict) -> (Option<String>, Option<String>, Option<String>) {
    let passed = verdict.matches;
    let errored = verdict.error_message.is_some();
    match &self.printed {
      Printed::Truthy { expected, .. } => {
        if passed {
          let received = (!errored).then(|| format!("Received: {expected}"));
          (Some(format!("Expected: not {expected}")), received, None)
        } else {
          let received = (!errored).then(|| format!("Received: {}", js_string(verdict.received.as_ref())));
          (Some(format!("Expected: {expected}")), received, None)
        }
      },
      Printed::Text { expected, substring } => {
        let is_string = matches!(expected, StringOrRegex::String(_));
        let expected_suffix = match (is_string, substring) {
          (true, true) => " substring",
          (true, false) => "",
          (false, _) => " pattern",
        };
        let received_suffix = if is_string && !substring { "" } else { " string" };
        let received = verdict.received.as_ref().and_then(Value::as_str).unwrap_or_default();
        let printed_expected = print_expected_text(expected);
        if passed {
          let received = (!errored).then(|| format!("Received{received_suffix}: {}", quote(received)));
          (
            Some(format!("Expected{expected_suffix}: not {printed_expected}")),
            received,
            None,
          )
        } else if errored {
          (
            Some(format!("Expected{expected_suffix}: {printed_expected}")),
            None,
            None,
          )
        } else {
          let (expected_label, received_label) = labels(
            &format!("Expected{expected_suffix}"),
            &format!("Received{received_suffix}"),
          );
          match expected {
            StringOrRegex::String(s) if s.contains('\n') || received.contains('\n') => {
              let expected_lines: Vec<String> = s.lines().map(str::to_owned).collect();
              let received_lines: Vec<String> = received.lines().map(str::to_owned).collect();
              (None, None, Some(jest_diff(&expected_lines, &received_lines)))
            },
            _ => (
              Some(format!("{expected_label}{printed_expected}")),
              Some(format!("{received_label}{}", quote(received))),
              None,
            ),
          }
        }
      },
      Printed::Equal(shown) => {
        let printed_expected = shown.print();
        let received = verdict
          .received
          .as_ref()
          .map_or_else(|| "undefined".to_owned(), print_value);
        if passed {
          let received = (!errored).then(|| format!("Received: {received}"));
          (Some(format!("Expected: not {printed_expected}")), received, None)
        } else if errored {
          (Some(format!("Expected: {printed_expected}")), None, None)
        } else {
          let normalized = shown.normalized_against(verdict.received.as_ref());
          let received_value = verdict.received.clone().unwrap_or(Value::Null);
          let structured = |v: &Value| v.is_array() || v.is_object();
          if structured(&normalized) && structured(&received_value) {
            let diff = jest_diff(&pretty_format(&normalized), &pretty_format(&received_value));
            (None, None, Some(diff))
          } else {
            (
              Some(format!("Expected: {printed_expected}")),
              Some(format!("Received: {received}")),
              None,
            )
          }
        }
      },
    }
  }
}

impl Verdict {
  fn settled(error: &FerriError) -> Self {
    let message = match error {
      FerriError::Evaluation(message) => message.strip_prefix("Error: ").unwrap_or(message).to_owned(),
      other => other.to_string(),
    };
    Self {
      matches: false,
      received: None,
      aria_snapshot: None,
      error_message: Some(format!("Error: {message}")),
      timed_out: false,
    }
  }
}

impl Shown {
  fn print(&self) -> String {
    match self {
      Self::Number(n) => print_value(&Value::from(*n)),
      Self::Texts(items) => format!(
        "[{}]",
        items.iter().map(print_expected_text).collect::<Vec<_>>().join(", ")
      ),
      Self::Json(value) => print_value(value),
    }
  }

  /// `toEqual.ts`: a pattern that matches its received entry is shown as
  /// that entry, so the diff only marks the entries that differ.
  fn normalized_against(&self, received: Option<&Value>) -> Value {
    match self {
      Self::Number(n) => Value::from(*n),
      Self::Json(value) => value.clone(),
      Self::Texts(items) => Value::Array(
        items
          .iter()
          .enumerate()
          .map(|(index, item)| {
            let entry = received.and_then(|r| r.get(index)).and_then(Value::as_str);
            match item {
              StringOrRegex::String(s) => Value::from(s.as_str()),
              StringOrRegex::Regex(re) => match entry {
                Some(entry) if re.is_match(entry) => Value::from(entry),
                _ => Value::from(re.to_string()),
              },
            }
          })
          .collect(),
      ),
    }
  }
}

/// Collapses identical consecutive attempts the way upstream's call log
/// does (`N × locator resolved to ...`).
struct CallLog {
  header: Vec<String>,
  attempts: Vec<(usize, Vec<String>)>,
}

impl CallLog {
  fn new(assertion: &WebAssert<'_>) -> Self {
    let not = if assertion.params.is_not { "not " } else { "" };
    let subject = assertion.receiver.description();
    let target = subject.as_ref().map(|s| format!(" {s}")).unwrap_or_default();
    let mut header = vec![format!(
      "Expect \"{not}{}\"{target} with timeout {}ms",
      assertion.method,
      assertion.timeout.as_millis()
    )];
    if let Some(subject) = subject {
      header.push(format!("waiting for {subject}"));
    }
    Self {
      header,
      attempts: Vec::new(),
    }
  }

  fn attempt(&mut self, lines: &[String]) {
    match self.attempts.last_mut() {
      Some((count, last)) if last.as_slice() == lines => *count += 1,
      _ => self.attempts.push((1, lines.to_vec())),
    }
  }

  fn render(&self) -> Vec<String> {
    let mut out = vec!["Call log:".to_owned()];
    out.extend(self.header.iter().map(|line| format!("  - {line}")));
    for (count, lines) in &self.attempts {
      for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let indent = " ".repeat(line.len() - trimmed.len());
        let times = if index == 0 && *count > 1 {
          format!("{count} × ")
        } else {
          String::new()
        };
        out.push(format!("  {indent}- {times}{trimmed}"));
      }
    }
    out
  }
}

/// A received value as JavaScript's template literal prints it.
fn js_string(value: Option<&Value>) -> String {
  match value {
    None => "undefined".into(),
    Some(Value::String(s)) => s.clone(),
    Some(other) => other.to_string(),
  }
}

fn quote(s: &str) -> String {
  serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

fn print_expected_text(value: &StringOrRegex) -> String {
  match value {
    StringOrRegex::String(s) => quote(s),
    StringOrRegex::Regex(re) => re.to_string(),
  }
}

/// Jest's `printExpected` / `printReceived` for JSON values.
fn print_value(value: &Value) -> String {
  match value {
    Value::String(s) => quote(s),
    Value::Array(items) => format!("[{}]", items.iter().map(print_value).collect::<Vec<_>>().join(", ")),
    Value::Object(map) => format!(
      "{{{}}}",
      map
        .iter()
        .map(|(k, v)| format!("{}: {}", quote(k), print_value(v)))
        .collect::<Vec<_>>()
        .join(", ")
    ),
    Value::Number(n) => n
      .as_f64()
      .filter(|f| f.fract() == 0.0 && f.abs() < 1e15)
      .map_or_else(|| n.to_string(), |f| format!("{f:.0}")),
    other => other.to_string(),
  }
}

/// Jest's `getLabelPrinter`: both labels padded to one width.
fn labels(expected: &str, received: &str) -> (String, String) {
  let width = expected.len().max(received.len());
  (
    format!("{expected}: {}", " ".repeat(width - expected.len())),
    format!("{received}: {}", " ".repeat(width - received.len())),
  )
}

/// Jest's `pretty-format` for JSON values, one line per entry.
fn pretty_format(value: &Value) -> Vec<String> {
  fn walk(value: &Value, indent: usize, prefix: &str, out: &mut Vec<String>, trailing: &str) {
    let pad = "  ".repeat(indent);
    match value {
      Value::Array(items) if !items.is_empty() => {
        out.push(format!("{pad}{prefix}Array ["));
        for item in items {
          walk(item, indent + 1, "", out, ",");
        }
        out.push(format!("{pad}]{trailing}"));
      },
      Value::Object(map) if !map.is_empty() => {
        out.push(format!("{pad}{prefix}Object {{"));
        for (key, item) in map {
          walk(item, indent + 1, &format!("{}: ", quote(key)), out, ",");
        }
        out.push(format!("{pad}}}{trailing}"));
      },
      Value::Array(_) => out.push(format!("{pad}{prefix}Array []{trailing}")),
      Value::Object(_) => out.push(format!("{pad}{prefix}Object {{}}{trailing}")),
      other => out.push(format!("{pad}{prefix}{}{trailing}", print_value(other))),
    }
  }
  let mut out = Vec::new();
  walk(value, 0, "", &mut out, "");
  out
}

/// Jest's line diff: a count header, then every line marked `- `, `+ `
/// or left as context.
fn jest_diff(expected: &[String], received: &[String]) -> String {
  let expected_text = expected.join("\n");
  let received_text = received.join("\n");
  let diff = similar::TextDiff::from_lines(&expected_text, &received_text);
  let (mut deleted, mut inserted) = (0, 0);
  let mut body = Vec::new();
  for change in diff.iter_all_changes() {
    let line = change.value().trim_end_matches('\n');
    let mark = match change.tag() {
      similar::ChangeTag::Delete => {
        deleted += 1;
        "-"
      },
      similar::ChangeTag::Insert => {
        inserted += 1;
        "+"
      },
      similar::ChangeTag::Equal => " ",
    };
    body.push(format!("{mark} {line}"));
  }
  format!(
    "- Expected  - {deleted}\n+ Received  + {inserted}\n\n{}",
    body.join("\n")
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn short_timeouts_drop_the_long_backoff_steps() {
    assert_eq!(backoff(Some(Duration::from_millis(1000))), vec![0, 20, 50, 100, 100]);
    assert_eq!(backoff(Some(Duration::from_secs(5))), vec![0, 20, 50, 100, 100, 500]);
    assert_eq!(backoff(Some(Duration::from_millis(50))), vec![0]);
    assert_eq!(backoff(None), vec![0, 20, 50, 100, 100, 500]);
  }

  #[test]
  fn repeated_attempts_collapse_in_the_call_log() {
    let mut log = CallLog {
      header: vec!["waiting for locator('input')".into()],
      attempts: Vec::new(),
    };
    let attempt = vec![
      "  locator resolved to <input/>".to_owned(),
      "  unexpected value \"unchecked\"".to_owned(),
    ];
    log.attempt(&attempt);
    log.attempt(&attempt);
    assert_eq!(
      log.render().join("\n"),
      "Call log:\n  - waiting for locator('input')\n    - 2 × locator resolved to <input/>\n    - unexpected value \"unchecked\""
    );
  }

  #[test]
  fn arrays_diff_like_jest() {
    let expected = serde_json::json!(["Text 1", "Text 3", "Extra"]);
    let received = serde_json::json!(["Text 1", "Text 3"]);
    assert_eq!(
      jest_diff(&pretty_format(&expected), &pretty_format(&received)),
      "- Expected  - 1\n+ Received  + 0\n\n  Array [\n    \"Text 1\",\n    \"Text 3\",\n-   \"Extra\",\n  ]"
    );
  }

  #[test]
  fn labels_pad_to_one_width() {
    assert_eq!(
      labels("Expected pattern", "Received string"),
      ("Expected pattern: ".to_owned(), "Received string:  ".to_owned())
    );
  }

  #[test]
  fn values_print_like_jest() {
    assert_eq!(print_value(&serde_json::json!(3)), "3");
    assert_eq!(print_value(&serde_json::json!(["a", 1])), "[\"a\", 1]");
    assert_eq!(js_string(None), "undefined");
  }
}
