//! Generic async polling primitives shared by the test runner's
//! `Expect<T>` and the script-layer `ExpectJs` binding.

use std::future::Future;
use std::time::Duration;

use crate::AssertionFailure;

/// Default expect timeout (5 seconds, matching Playwright).
pub const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Process-wide default `expect()` timeout in ms — the runner sets this
/// from the config's `expectTimeout` before tests start (Playwright's
/// `config.expect.timeout`). Per-assertion `with_timeout` still wins.
static DEFAULT_EXPECT_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
  std::sync::atomic::AtomicU64::new(DEFAULT_EXPECT_TIMEOUT.as_millis() as u64);

/// Override the process-wide default `expect()` timeout.
pub fn set_default_expect_timeout(timeout: Duration) {
  DEFAULT_EXPECT_TIMEOUT_MS.store(
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
    std::sync::atomic::Ordering::Relaxed,
  );
}

/// The current process-wide default `expect()` timeout.
#[must_use]
pub fn default_expect_timeout() -> Duration {
  Duration::from_millis(DEFAULT_EXPECT_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Retry intervals matching Playwright: [100, 250, 500, 1000, 1000, ...]
pub const POLL_INTERVALS: &[u64] = &[100, 250, 500, 1000];

/// Internal match error used during polling — captured per attempt and
/// rendered into the final [`AssertionFailure`] when the deadline expires.
#[derive(Debug, Clone)]
pub struct MatchError {
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

/// Context for an expect assertion — used to build Playwright-style
/// error messages.
#[derive(Debug, Clone)]
pub struct ExpectContext {
  /// e.g. `"toHaveText"`, `"toBeVisible"`.
  pub method: &'static str,
  /// e.g. `"locator('h1')"`, `"page"`.
  pub subject: String,
  /// Whether this is a negated assertion (`.not`).
  pub is_not: bool,
}

/// Poll a condition until it passes or timeout. Produces a
/// Playwright-style error message with method name, locator,
/// expected/received, and call log.
pub async fn poll_until<F, Fut>(timeout: Duration, ctx: ExpectContext, mut check: F) -> Result<(), AssertionFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  let deadline = tokio::time::Instant::now() + timeout;
  let mut last_error: Option<MatchError>;
  let mut interval_idx = 0;
  let mut call_log: Vec<String> = Vec::new();
  call_log.push(format!("expect.{} with timeout {}ms", ctx.method, timeout.as_millis()));
  call_log.push(format!("waiting for {}", ctx.subject));

  loop {
    match check().await {
      Ok(()) => return Ok(()),
      Err(e) => {
        call_log.push(format!("  unexpected value {}", e.received));
        last_error = Some(e);
        let interval_ms = POLL_INTERVALS
          .get(interval_idx)
          .copied()
          .unwrap_or_else(|| POLL_INTERVALS.last().copied().unwrap_or(1000));
        interval_idx += 1;

        let sleep_dur = Duration::from_millis(interval_ms);
        if tokio::time::Instant::now() + sleep_dur > deadline {
          break;
        }
        tokio::time::sleep(sleep_dur).await;
      },
    }
  }

  let err = last_error.unwrap_or_else(|| MatchError::new("(unknown)", "(unknown)"));

  let not_str = if ctx.is_not { ".not" } else { "" };
  let timeout_ms = timeout.as_millis();

  let call_log_str = if call_log.is_empty() {
    String::new()
  } else {
    format!(
      "\n\nCall log:\n{}",
      call_log
        .iter()
        .map(|l| format!("  - {l}"))
        .collect::<Vec<_>>()
        .join("\n")
    )
  };

  let message = format!(
    "expect({subject}){not_str}.{method}() failed\n\n\
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

  let diff = format!("Expected: {}\nReceived: {}", err.expected, err.received);

  Err(AssertionFailure::new(message, Some(diff)))
}
