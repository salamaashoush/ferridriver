//! `Expect<T>` builder shared by every consumer (test runner, QuickJS
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

/// Options for `expect(locator).toHaveCSS()`.
#[derive(Debug, Clone, Default)]
pub struct HaveCssOptions {
  /// Pseudo-element selector (e.g. `"::before"`, `"::after"`).
  pub pseudo: Option<String>,
}

/// Wrap a subject for auto-retrying assertions.
#[must_use]
pub fn expect<T>(subject: &T) -> Expect<'_, T> {
  Expect {
    subject,
    timeout: DEFAULT_EXPECT_TIMEOUT,
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
/// matcher. Matches Playwright's `expect.poll()`.
pub struct ExpectPoll<F> {
  generator: F,
  timeout: Duration,
  intervals: Vec<u64>,
}

/// Create a polling expect (Playwright's `expect.poll(fn)`).
pub fn expect_poll<F, Fut, T>(generator: F, timeout: Duration) -> ExpectPoll<F>
where
  F: Fn() -> Fut,
  Fut: Future<Output = T>,
{
  ExpectPoll {
    generator,
    timeout,
    intervals: POLL_INTERVALS.to_vec(),
  }
}

impl<F, Fut, T> ExpectPoll<F>
where
  F: Fn() -> Fut,
  Fut: Future<Output = T>,
  T: PartialEq + std::fmt::Debug,
{
  #[must_use]
  pub fn with_intervals(mut self, intervals: Vec<u64>) -> Self {
    self.intervals = intervals;
    self
  }

  /// Assert the polled value equals the expected value.
  pub async fn to_equal(self, expected: T) -> Result<(), AssertionFailure> {
    let deadline = tokio::time::Instant::now() + self.timeout;
    let mut interval_idx = 0;
    loop {
      let actual = (self.generator)().await;
      if actual == expected {
        return Ok(());
      }
      let interval_ms = self
        .intervals
        .get(interval_idx)
        .copied()
        .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
      interval_idx += 1;
      let sleep_dur = Duration::from_millis(interval_ms);
      if tokio::time::Instant::now() + sleep_dur > deadline {
        return Err(AssertionFailure::new(
          "expect.poll().to_equal() timed out".to_string(),
          Some(format!("Expected: {expected:?}\nReceived: {actual:?}")),
        ));
      }
      tokio::time::sleep(sleep_dur).await;
    }
  }

  /// Assert the polled value satisfies a predicate.
  pub async fn to_satisfy(self, predicate: impl Fn(&T) -> bool, description: &str) -> Result<(), AssertionFailure> {
    let deadline = tokio::time::Instant::now() + self.timeout;
    let mut interval_idx = 0;
    loop {
      let actual = (self.generator)().await;
      if predicate(&actual) {
        return Ok(());
      }
      let interval_ms = self
        .intervals
        .get(interval_idx)
        .copied()
        .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
      interval_idx += 1;
      let sleep_dur = Duration::from_millis(interval_ms);
      if tokio::time::Instant::now() + sleep_dur > deadline {
        return Err(AssertionFailure::new(
          "expect.poll().to_satisfy() timed out".to_string(),
          Some(format!("Expected: {description}\nReceived: {actual:?}")),
        ));
      }
      tokio::time::sleep(sleep_dur).await;
    }
  }
}

// ── toPass() ──

pub struct ToPassOptions {
  pub timeout: Duration,
  pub intervals: Vec<u64>,
  pub message: Option<String>,
}

impl Default for ToPassOptions {
  fn default() -> Self {
    Self {
      timeout: DEFAULT_EXPECT_TIMEOUT,
      intervals: POLL_INTERVALS.to_vec(),
      message: None,
    }
  }
}

/// Retry an async block until it passes or timeout (Playwright's
/// `expect(fn).toPass()`).
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
        let sleep_dur = Duration::from_millis(interval_ms);
        if tokio::time::Instant::now() + sleep_dur > deadline {
          break e;
        }
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
