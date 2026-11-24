//! `Expect<T>` builder shared by every consumer (test runner, `QuickJS`
//! binding, plain Rust callers). Targets are subjects of web-first
//! matchers — `&Locator`, `&Arc<Page>`, `&HttpResponse`. Value matchers
//! live on [`crate::ExpectValue`] instead.

use std::future::Future;
use std::time::Duration;

use crate::AssertionFailure;
use crate::poll::{DEFAULT_EXPECT_TIMEOUT, POLL_INTERVALS};

/// Options for `expect(locator).toBeInViewport()`.
#[derive(Debug, Clone, Default)]
pub struct InViewportOptions {
  /// Required intersection ratio in `[0, 1]`. `None` accepts any
  /// non-zero overlap (Playwright default).
  pub ratio: Option<f64>,
}

/// Options for `expect(locator).toBeChecked()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckedOptions {
  /// `false` asserts unchecked.
  pub checked: Option<bool>,
  /// Asserts the mixed state; cannot be combined with `checked`.
  pub indeterminate: Option<bool>,
}

/// Options for `expect(locator).toHaveCSS()`.
#[derive(Debug, Clone, Default)]
pub struct HaveCssOptions {
  /// Pseudo-element to read computed styles from.
  pub pseudo: Option<CssPseudo>,
}

/// Playwright's `pseudo?: 'before' | 'after'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssPseudo {
  Before,
  After,
}

impl CssPseudo {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Before => "before",
      Self::After => "after",
    }
  }
}

impl std::str::FromStr for CssPseudo {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "before" => Ok(Self::Before),
      "after" => Ok(Self::After),
      other => Err(format!("pseudo: expected \"before\" or \"after\", got \"{other}\"")),
    }
  }
}

/// Wrap a subject for auto-retrying assertions.
#[must_use]
pub fn expect<T>(subject: &T) -> Expect<'_, T> {
  Expect {
    subject,
    timeout: crate::poll::default_expect_timeout(),
    is_not: false,
    is_soft: false,
    message: None,
  }
}

/// Create a pre-configured expect with custom defaults (Playwright's
/// `expect.configure()`).
#[must_use]
pub fn expect_configured<T>(subject: &T, timeout: Duration) -> Expect<'_, T> {
  Expect {
    subject,
    timeout,
    is_not: false,
    is_soft: false,
    message: None,
  }
}

/// Auto-retrying assertion builder.
///
/// # Failures
///
/// A web-first matcher returns an [`AssertionFailure`] when its expectation, inverted by
/// [`Expect::not`], still does not hold at the timeout or when the running test's abort signal
/// fires. It fails at once, without retrying, when the page rejects the selector or the expression,
/// the locator matches several elements under strict mode, or the page closes. A failure from an
/// assertion marked [`Expect::soft`] is recorded by the running test's sink and the matcher returns
/// `Ok`.
pub struct Expect<'a, T> {
  pub subject: &'a T,
  pub timeout: Duration,
  pub is_not: bool,
  pub is_soft: bool,
  pub message: Option<String>,
}

impl<T> Expect<'_, T> {
  /// Invert the assertion (Playwright's `.not`).
  #[must_use]
  #[expect(
    clippy::should_implement_trait,
    reason = "Playwright's `.not`: inherent so callers and generated tests chain it without importing `std::ops::Not`"
  )]
  pub fn not(mut self) -> Self {
    self.is_not = !self.is_not;
    self
  }

  /// Override the timeout for this assertion.
  #[must_use]
  pub fn with_timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// Custom failure-message prefix.
  #[must_use]
  pub fn with_message(mut self, msg: impl Into<String>) -> Self {
    self.message = Some(msg.into());
    self
  }

  /// Mark as soft assertion — error is returned but collected by the
  /// caller rather than thrown.
  #[must_use]
  pub fn soft(mut self) -> Self {
    self.is_soft = true;
    self
  }
}

// ── expect.poll() ──

/// Poll a generic async function until its return value satisfies a
/// matcher. Matches Playwright's `expect.poll(fn, { timeout, intervals,
/// message })`: the timeout defaults to the configured `expect` timeout.
#[must_use = "expect.poll asserts nothing until a matcher is awaited"]
pub struct ExpectPoll<F> {
  generator: F,
  timeout: Duration,
  intervals: Vec<u64>,
  is_not: bool,
  message: Option<String>,
}

/// Create a polling expect (Playwright's `expect.poll(fn)`).
pub fn expect_poll<F, Fut, T>(generator: F) -> ExpectPoll<F>
where
  F: Fn() -> Fut,
  Fut: Future<Output = T>,
{
  ExpectPoll {
    generator,
    timeout: crate::poll::default_expect_timeout(),
    intervals: POLL_INTERVALS.to_vec(),
    is_not: false,
    message: None,
  }
}

impl<F, Fut, T> ExpectPoll<F>
where
  F: Fn() -> Fut,
  Fut: Future<Output = T>,
  T: std::fmt::Debug,
{
  /// Playwright's `timeout` option.
  pub fn with_timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// Playwright's `intervals` option, in milliseconds; the last one
  /// repeats.
  pub fn with_intervals(mut self, intervals: Vec<u64>) -> Self {
    self.intervals = intervals;
    self
  }

  /// Playwright's `message` option: prefixes the failure.
  pub fn with_message(mut self, message: impl Into<String>) -> Self {
    self.message = Some(message.into());
    self
  }

  /// Invert the matcher (`expect.poll(fn).not`).
  #[expect(
    clippy::should_implement_trait,
    reason = "Playwright's `.not`: inherent so callers and generated tests chain it without importing `std::ops::Not`"
  )]
  pub fn not(mut self) -> Self {
    self.is_not = !self.is_not;
    self
  }

  /// Poll until `pass(actual)` (or its negation under `.not()`) holds, or
  /// fail at the deadline with the last value received.
  async fn poll(&self, method: &str, expected: String, pass: impl Fn(&T) -> bool) -> Result<(), AssertionFailure> {
    let deadline = tokio::time::Instant::now() + self.timeout;
    let mut interval_idx = 0;
    loop {
      let actual = (self.generator)().await;
      if pass(&actual) != self.is_not {
        return Ok(());
      }
      let now = tokio::time::Instant::now();
      if now >= deadline {
        let not = if self.is_not { ".not" } else { "" };
        let prefix = self.message.as_deref().map(|m| format!("{m}\n\n")).unwrap_or_default();
        return Err(AssertionFailure::new(
          format!("{prefix}expect.poll(){not}.{method}() failed"),
          Some(format!(
            "Expected: {not_word}{expected}\nReceived: {actual:?}\nTimeout:  {}ms",
            self.timeout.as_millis(),
            not_word = if self.is_not { "not " } else { "" },
          )),
        ));
      }
      let interval_ms = self
        .intervals
        .get(interval_idx)
        .copied()
        .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
      interval_idx += 1;
      // Clamp the interval to the remaining budget so the final
      // attempt lands AT the deadline instead of bailing early.
      tokio::time::sleep(Duration::from_millis(interval_ms).min(deadline - now)).await;
    }
  }

  /// Assert the polled value equals `expected`.
  ///
  /// # Errors
  ///
  /// Returns an [`AssertionFailure`] when the polled value still does not equal `expected` at the
  /// timeout (the reverse under `.not()`).
  pub async fn to_equal(&self, expected: T) -> Result<(), AssertionFailure>
  where
    T: PartialEq,
  {
    let description = format!("{expected:?}");
    self.poll("toEqual", description, |actual| *actual == expected).await
  }

  /// Assert the polled value satisfies `predicate`, described in the
  /// failure as `description`.
  ///
  /// # Errors
  ///
  /// Returns an [`AssertionFailure`] when the polled value still does not satisfy `predicate` at
  /// the timeout (the reverse under `.not()`).
  pub async fn to_satisfy(&self, predicate: impl Fn(&T) -> bool, description: &str) -> Result<(), AssertionFailure> {
    self.poll("toSatisfy", description.to_string(), predicate).await
  }

  /// Assert the polled value is `> expected`.
  ///
  /// # Errors
  ///
  /// Returns an [`AssertionFailure`] when the polled value is still not greater than `expected` at
  /// the timeout (the reverse under `.not()`).
  pub async fn to_be_greater_than(&self, expected: T) -> Result<(), AssertionFailure>
  where
    T: PartialOrd,
  {
    let description = format!("> {expected:?}");
    self
      .poll("toBeGreaterThan", description, |actual| *actual > expected)
      .await
  }

  /// Assert the polled value is `>= expected`.
  ///
  /// # Errors
  ///
  /// Returns an [`AssertionFailure`] when the polled value is still less than `expected` at the
  /// timeout (the reverse under `.not()`).
  pub async fn to_be_greater_than_or_equal(&self, expected: T) -> Result<(), AssertionFailure>
  where
    T: PartialOrd,
  {
    let description = format!(">= {expected:?}");
    self
      .poll("toBeGreaterThanOrEqual", description, |actual| *actual >= expected)
      .await
  }

  /// Assert the polled value is `< expected`.
  ///
  /// # Errors
  ///
  /// Returns an [`AssertionFailure`] when the polled value is still not less than `expected` at the
  /// timeout (the reverse under `.not()`).
  pub async fn to_be_less_than(&self, expected: T) -> Result<(), AssertionFailure>
  where
    T: PartialOrd,
  {
    let description = format!("< {expected:?}");
    self
      .poll("toBeLessThan", description, |actual| *actual < expected)
      .await
  }

  /// Assert the polled value is `<= expected`.
  ///
  /// # Errors
  ///
  /// Returns an [`AssertionFailure`] when the polled value is still greater than `expected` at the
  /// timeout (the reverse under `.not()`).
  pub async fn to_be_less_than_or_equal(&self, expected: T) -> Result<(), AssertionFailure>
  where
    T: PartialOrd,
  {
    let description = format!("<= {expected:?}");
    self
      .poll("toBeLessThanOrEqual", description, |actual| *actual <= expected)
      .await
  }
}

// ── toPass() ──

pub struct ToPassOptions {
  pub timeout: Duration,
  pub intervals: Vec<u64>,
  pub message: Option<String>,
}

impl Default for ToPassOptions {
  /// `expect.toPass` from the running test's config, else Playwright's
  /// own defaults (`matchers/matchers.ts:504-505`). A caller that names
  /// a field still wins — this only fills in what nothing set.
  fn default() -> Self {
    let cfg = crate::poll::current_expect_config();
    let to_pass = cfg.as_ref().map(|c| &c.to_pass);
    Self {
      timeout: to_pass
        .and_then(|t| t.timeout)
        .map_or(DEFAULT_EXPECT_TIMEOUT, Duration::from_millis),
      intervals: to_pass
        .and_then(|t| t.intervals.clone())
        .unwrap_or_else(|| POLL_INTERVALS.to_vec()),
      message: None,
    }
  }
}

/// Retry an async block until it passes or timeout (Playwright's
/// `expect(fn).toPass()`).
///
/// # Errors
///
/// Returns the last attempt's [`AssertionFailure`], its message prefixed with the attempt count,
/// when `body` has not passed by `timeout`.
pub async fn to_pass<F, Fut>(timeout: Duration, body: F) -> Result<(), AssertionFailure>
where
  F: Fn() -> Fut,
  Fut: Future<Output = Result<(), AssertionFailure>>,
{
  to_pass_with_options(
    body,
    ToPassOptions {
      timeout,
      ..Default::default()
    },
  )
  .await
}

/// # Errors
///
/// Returns the last attempt's [`AssertionFailure`], its message prefixed with `options.message` (or
/// `toPass()`) and the attempt count, when `body` has not passed by `options.timeout`.
pub async fn to_pass_with_options<F, Fut>(body: F, options: ToPassOptions) -> Result<(), AssertionFailure>
where
  F: Fn() -> Fut,
  Fut: Future<Output = Result<(), AssertionFailure>>,
{
  let deadline = tokio::time::Instant::now() + options.timeout;
  let mut interval_idx = 0;
  let mut attempts = 0u32;

  // Loop produces the last failure on timeout exit; the initial `None`
  // never reads back because the body runs at least once.
  let final_err: AssertionFailure = loop {
    attempts += 1;
    match body().await {
      Ok(()) => return Ok(()),
      Err(e) => {
        let interval_ms = options
          .intervals
          .get(interval_idx)
          .copied()
          .unwrap_or_else(|| options.intervals.last().copied().unwrap_or(1000));
        interval_idx += 1;
        // Clamp the interval to the remaining budget so the final
        // attempt lands AT the deadline instead of bailing early.
        let now = tokio::time::Instant::now();
        if now >= deadline {
          break e;
        }
        let sleep_dur = Duration::from_millis(interval_ms).min(deadline - now);
        tokio::time::sleep(sleep_dur).await;
      },
    }
  };

  let mut err = final_err;
  let prefix = options.message.as_deref().unwrap_or("toPass()");
  err.message = format!(
    "{prefix} failed after {attempts} attempt(s) ({:?}): {}",
    options.timeout, err.message
  );
  Err(err)
}

#[cfg(test)]
mod poll_tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};
  use std::time::Duration;

  use super::expect_poll;

  fn counter() -> (Arc<AtomicU32>, impl Fn() -> std::future::Ready<u32>) {
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&calls);
    (calls, move || {
      std::future::ready(seen.fetch_add(1, Ordering::SeqCst) + 1)
    })
  }

  #[tokio::test(start_paused = true)]
  async fn polls_until_the_value_arrives() {
    let (calls, next) = counter();
    expect_poll(next).to_be_greater_than_or_equal(3).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
  }

  #[tokio::test(start_paused = true)]
  async fn not_inverts_and_the_failure_reports_the_last_value() {
    let failure = expect_poll(|| std::future::ready(7))
      .not()
      .with_timeout(Duration::from_millis(300))
      .with_message("cart total")
      .to_equal(7)
      .await
      .unwrap_err();
    assert_eq!(failure.message, "cart total\n\nexpect.poll().not.toEqual() failed");
    let diff = failure.diff.unwrap_or_default();
    assert!(
      diff.contains("Expected: not 7") && diff.contains("Received: 7") && diff.contains("300ms"),
      "{diff}"
    );
    expect_poll(|| std::future::ready(1)).not().to_equal(2).await.unwrap();
  }

  #[tokio::test(start_paused = true)]
  async fn the_default_deadline_is_the_expect_timeout() {
    let started = tokio::time::Instant::now();
    expect_poll(|| std::future::ready(0))
      .to_be_greater_than(0)
      .await
      .unwrap_err();
    assert_eq!(started.elapsed(), crate::poll::default_expect_timeout());
  }

  #[tokio::test(start_paused = true)]
  async fn ordering_matchers_compare_the_polled_value() {
    expect_poll(|| std::future::ready(2)).to_be_less_than(3).await.unwrap();
    expect_poll(|| std::future::ready(3))
      .to_be_less_than_or_equal(3)
      .await
      .unwrap();
    expect_poll(|| std::future::ready("b"))
      .to_satisfy(|v| v.len() == 1, "one character")
      .await
      .unwrap();
  }
}
