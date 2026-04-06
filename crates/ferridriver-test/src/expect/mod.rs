//! Auto-retrying assertions matching Playwright's full `expect()` API.
//!
//! ```ignore
//! use ferridriver_test::expect::expect;
//!
//! // Page assertions (auto-retry with 5s timeout)
//! expect(&page).to_have_title("Example").await?;
//! expect(&page).not().to_have_url("https://wrong.com").await?;
//!
//! // Locator assertions (auto-retry)
//! expect(&page.locator("h1")).to_have_text("Hello").await?;
//! expect(&page.locator("button")).to_be_visible().await?;
//!
//! // Custom timeout
//! expect(&page.locator(".slow"))
//!     .with_timeout(Duration::from_secs(10))
//!     .to_be_visible().await?;
//!
//! // Soft assertions (collect errors, don't fail immediately)
//! expect(&page.locator(".a")).soft().to_be_visible().await; // error collected, not thrown
//!
//! // Poll a generic value
//! expect_poll(|| async { fetch_count().await }, Duration::from_secs(10))
//!     .to_equal(5).await?;
//!
//! // toPass: retry a block until it passes
//! to_pass(Duration::from_secs(10), || async {
//!     let text = page.locator("#status").text_content().await?.unwrap_or_default();
//!     assert!(text.contains("ready"));
//!     Ok(())
//! }).await?;
//! ```

pub mod locator;
pub mod page;

use std::future::Future;
use std::time::Duration;

use crate::model::TestFailure;

/// Default expect timeout (5 seconds, matching Playwright).
pub const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Retry intervals matching Playwright: [0, 100, 250, 500, 1000, 1000, ...]
pub const POLL_INTERVALS: &[u64] = &[100, 250, 500, 1000];

/// A match that supports both string equality and regex.
#[derive(Debug, Clone)]
pub enum StringOrRegex {
  String(String),
  Regex(regex::Regex),
}

impl StringOrRegex {
  pub fn matches(&self, actual: &str) -> bool {
    match self {
      Self::String(expected) => actual == expected,
      Self::Regex(re) => re.is_match(actual),
    }
  }

  pub fn description(&self) -> String {
    match self {
      Self::String(s) => format!("\"{s}\""),
      Self::Regex(re) => format!("/{}/", re.as_str()),
    }
  }
}

impl From<&str> for StringOrRegex {
  fn from(s: &str) -> Self {
    Self::String(s.to_string())
  }
}

impl From<String> for StringOrRegex {
  fn from(s: String) -> Self {
    Self::String(s)
  }
}

impl From<regex::Regex> for StringOrRegex {
  fn from(re: regex::Regex) -> Self {
    Self::Regex(re)
  }
}

// ── expect() ──

/// Wrap a subject for auto-retrying assertions.
pub fn expect<T>(subject: &T) -> Expect<'_, T> {
  Expect {
    subject,
    timeout: DEFAULT_EXPECT_TIMEOUT,
    is_not: false,
    is_soft: false,
    message: None,
  }
}

/// Create a pre-configured expect with custom defaults (Playwright's `expect.configure()`).
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
  pub(crate) subject: &'a T,
  pub(crate) timeout: Duration,
  pub(crate) is_not: bool,
  pub(crate) is_soft: bool,
  pub(crate) message: Option<String>,
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

  /// Custom failure message prefix.
  #[must_use]
  pub fn with_message(mut self, msg: impl Into<String>) -> Self {
    self.message = Some(msg.into());
    self
  }

  /// Mark as soft assertion — error is returned as `Ok(())` but collected.
  /// The caller should check `TestInfo::has_soft_errors()` at test end.
  #[must_use]
  pub fn soft(mut self) -> Self {
    self.is_soft = true;
    self
  }
}

// ── expect.poll() ──

/// Poll a generic async function until its return value satisfies a matcher.
/// Matches Playwright's `expect.poll()`.
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
  /// Override polling intervals.
  #[must_use]
  pub fn with_intervals(mut self, intervals: Vec<u64>) -> Self {
    self.intervals = intervals;
    self
  }

  /// Assert the polled value equals the expected value.
  pub async fn to_equal(self, expected: T) -> Result<(), TestFailure> {
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
        .unwrap_or(*self.intervals.last().unwrap_or(&1000));
      interval_idx += 1;

      let sleep_dur = Duration::from_millis(interval_ms);
      if tokio::time::Instant::now() + sleep_dur > deadline {
        return Err(TestFailure {
          message: format!("expect.poll timed out: expected {expected:?}, last value was {actual:?}"),
          stack: None,
          diff: Some(format!("- expected: {expected:?}\n+ received: {actual:?}")),
          screenshot: None,
        });
      }
      tokio::time::sleep(sleep_dur).await;
    }
  }

  /// Assert the polled value satisfies a predicate.
  pub async fn to_satisfy(self, predicate: impl Fn(&T) -> bool, description: &str) -> Result<(), TestFailure> {
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
        .unwrap_or(*self.intervals.last().unwrap_or(&1000));
      interval_idx += 1;

      let sleep_dur = Duration::from_millis(interval_ms);
      if tokio::time::Instant::now() + sleep_dur > deadline {
        return Err(TestFailure {
          message: format!("expect.poll timed out: {description}, last value was {actual:?}"),
          stack: None,
          diff: None,
          screenshot: None,
        });
      }
      tokio::time::sleep(sleep_dur).await;
    }
  }
}

// ── toPass() ──

/// Retry an async block until it passes or timeout (Playwright's `expect(fn).toPass()`).
pub async fn to_pass<F, Fut>(timeout: Duration, body: F) -> Result<(), TestFailure>
where
  F: Fn() -> Fut,
  Fut: Future<Output = Result<(), TestFailure>>,
{
  let deadline = tokio::time::Instant::now() + timeout;
  let mut interval_idx = 0;
  let mut last_error: Option<TestFailure>;

  loop {
    match body().await {
      Ok(()) => return Ok(()),
      Err(e) => {
        last_error = Some(e);
        let interval_ms = POLL_INTERVALS
          .get(interval_idx)
          .copied()
          .unwrap_or(*POLL_INTERVALS.last().unwrap_or(&1000));
        interval_idx += 1;

        let sleep_dur = Duration::from_millis(interval_ms);
        if tokio::time::Instant::now() + sleep_dur > deadline {
          break;
        }
        tokio::time::sleep(sleep_dur).await;
      }
    }
  }

  Err(
    last_error.unwrap_or_else(|| TestFailure {
      message: "toPass() timed out".into(),
      stack: None,
      diff: None,
      screenshot: None,
    }),
  )
}

// ── Internal polling ──

/// Internal match error used during polling.
#[derive(Debug)]
pub(crate) struct MatchError {
  pub message: String,
  pub diff: Option<String>,
}

impl MatchError {
  pub fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
      diff: None,
    }
  }

  pub fn with_diff(mut self, diff: impl Into<String>) -> Self {
    self.diff = Some(diff.into());
    self
  }
}

/// Poll a condition until it passes or timeout, using Playwright's interval pattern.
pub(crate) async fn poll_until<F, Fut>(timeout: Duration, mut check: F) -> Result<(), TestFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  let deadline = tokio::time::Instant::now() + timeout;
  let mut last_error: Option<MatchError>;
  let mut interval_idx = 0;

  loop {
    match check().await {
      Ok(()) => return Ok(()),
      Err(e) => {
        last_error = Some(e);
        let interval_ms = POLL_INTERVALS
          .get(interval_idx)
          .copied()
          .unwrap_or(*POLL_INTERVALS.last().unwrap_or(&1000));
        interval_idx += 1;

        let sleep_dur = Duration::from_millis(interval_ms);
        if tokio::time::Instant::now() + sleep_dur > deadline {
          break;
        }
        tokio::time::sleep(sleep_dur).await;
      }
    }
  }

  let err = last_error.unwrap_or_else(|| MatchError::new("assertion timed out"));
  Err(TestFailure {
    message: err.message,
    stack: None,
    diff: err.diff,
    screenshot: None,
  })
}
