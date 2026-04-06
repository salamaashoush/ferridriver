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

/// Options for `to_pass` / `expect(fn).toPass()`.
pub struct ToPassOptions {
  /// Maximum time to retry (default: 5s).
  pub timeout: Duration,
  /// Retry intervals in ms (cycles last value). Default: [100, 250, 500, 1000].
  pub intervals: Vec<u64>,
  /// Custom error message prefix on final failure.
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

/// Retry an async block until it passes or timeout (Playwright's `expect(fn).toPass()`).
///
/// Simple form with just timeout:
/// ```ignore
/// to_pass(Duration::from_secs(10), || async { ... }).await?;
/// ```
pub async fn to_pass<F, Fut>(timeout: Duration, body: F) -> Result<(), TestFailure>
where
  F: Fn() -> Fut,
  Fut: Future<Output = Result<(), TestFailure>>,
{
  to_pass_with_options(body, ToPassOptions { timeout, ..Default::default() }).await
}

/// Retry an async block with full options.
pub async fn to_pass_with_options<F, Fut>(body: F, options: ToPassOptions) -> Result<(), TestFailure>
where
  F: Fn() -> Fut,
  Fut: Future<Output = Result<(), TestFailure>>,
{
  let deadline = tokio::time::Instant::now() + options.timeout;
  let mut interval_idx = 0;
  let mut last_error: Option<TestFailure>;
  let mut attempts = 0u32;

  loop {
    attempts += 1;
    match body().await {
      Ok(()) => return Ok(()),
      Err(e) => {
        last_error = Some(e);
        let interval_ms = options
          .intervals
          .get(interval_idx)
          .copied()
          .unwrap_or(*options.intervals.last().unwrap_or(&1000));
        interval_idx += 1;

        let sleep_dur = Duration::from_millis(interval_ms);
        if tokio::time::Instant::now() + sleep_dur > deadline {
          break;
        }
        tokio::time::sleep(sleep_dur).await;
      }
    }
  }

  let mut err = last_error.unwrap_or_else(|| TestFailure {
    message: "toPass() timed out".into(),
    stack: None,
    diff: None,
    screenshot: None,
  });

  // Wrap with context.
  let prefix = options.message.as_deref().unwrap_or("toPass()");
  err.message = format!("{prefix} failed after {attempts} attempt(s) ({:?}): {}", options.timeout, err.message);
  Err(err)
}

// ── Internal polling ──

/// Internal match error used during polling.
#[derive(Debug)]
pub(crate) struct MatchError {
  pub expected: String,
  pub received: String,
}

impl MatchError {
  pub fn new(expected: impl Into<String>, received: impl Into<String>) -> Self {
    Self {
      expected: expected.into(),
      received: received.into(),
    }
  }

}

/// Context for an expect assertion — used to build Playwright-style error messages.
pub(crate) struct ExpectContext {
  /// e.g. "toHaveText", "toBeVisible"
  pub method: &'static str,
  /// e.g. "locator('h1')", "page"
  pub subject: String,
  /// Whether this is a negated assertion (.not)
  pub is_not: bool,
}

/// Poll a condition until it passes or timeout, using Playwright's interval pattern.
/// Produces Playwright-style error messages with method name, locator, expected/received, and call log.
pub(crate) async fn poll_until<F, Fut>(
  timeout: Duration,
  ctx: ExpectContext,
  mut check: F,
) -> Result<(), TestFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  let deadline = tokio::time::Instant::now() + timeout;
  let mut last_error: Option<MatchError>;
  let mut interval_idx = 0;
  let mut call_log: Vec<String> = Vec::new();
  let mut retries = 0u32;

  call_log.push(format!(
    "expect.{} with timeout {}ms",
    ctx.method,
    timeout.as_millis()
  ));
  call_log.push(format!("waiting for {}", ctx.subject));

  loop {
    match check().await {
      Ok(()) => return Ok(()),
      Err(e) => {
        retries += 1;
        call_log.push(format!("  unexpected value {}", e.received));
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

  let err = last_error.unwrap_or_else(|| MatchError::new("(unknown)", "(unknown)"));

  // Playwright-style error format:
  //   expect(locator).toHaveText(expected) failed
  //
  //   Locator:  locator('h1')
  //   Expected: "Wrong Text"
  //   Received: "Example Domain"
  //   Timeout:  5000ms
  //
  //   Call log:
  //     ...
  let not_str = if ctx.is_not { ".not" } else { "" };
  let timeout_ms = timeout.as_millis();

  let call_log_str = if call_log.is_empty() {
    String::new()
  } else {
    format!(
      "\n\nCall log:\n{}",
      call_log.iter().map(|l| format!("  - {l}")).collect::<Vec<_>>().join("\n")
    )
  };

  let message = format!(
    "\
expect({subject}){not_str}.{method}() failed\n\
\n\
Locator:  {locator}\n\
Expected: {expected}\n\
Received: {received}\n\
Timeout:  {timeout_ms}ms\
{call_log_str}",
    subject = ctx.subject,
    method = ctx.method,
    locator = ctx.subject,
    expected = err.expected,
    received = err.received,
  );

  let diff = format!(
    "Expected: {}\nReceived: {}",
    err.expected, err.received,
  );

  Err(TestFailure {
    message,
    stack: None,
    diff: Some(diff),
    screenshot: None,
  })
}
