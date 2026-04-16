//! NAPI `StepHandle` class — wraps the core `StepHandle` for structured test steps.
//!
//! Used by `test.step()` in the Playwright-compatible API.
//! The handle is created via `TestInfo.beginStep()` and must be completed via `end()`.

use napi::Result;
use napi_derive::napi;
use tokio::sync::Mutex;

/// Handle to an in-progress test step. Must be completed via `end()` or `skip()`.
///
/// Wraps the core `StepHandle` in an `Option` because `end()` consumes it.
#[napi]
pub struct StepHandle {
  inner: Mutex<Option<ferridriver_test::model::StepHandle>>,
}

impl StepHandle {
  pub(crate) fn new(handle: ferridriver_test::model::StepHandle) -> Self {
    Self {
      inner: Mutex::new(Some(handle)),
    }
  }
}

#[napi]
impl StepHandle {
  /// Complete this step. Pass an error message string for failure, or nothing for success.
  #[napi]
  pub async fn end(&self, error: Option<String>) -> Result<()> {
    let handle = self
      .inner
      .lock()
      .await
      .take()
      .ok_or_else(|| napi::Error::from_reason("StepHandle already consumed"))?;
    handle.end(error).await;
    Ok(())
  }

  /// Complete this step as skipped.
  #[napi]
  pub async fn skip(&self, reason: Option<String>) -> Result<()> {
    let handle = self
      .inner
      .lock()
      .await
      .take()
      .ok_or_else(|| napi::Error::from_reason("StepHandle already consumed"))?;
    handle.skip(reason).await;
    Ok(())
  }
}
