//! Bridge from core [`FerriError`] to `napi::Error`.
//!
//! napi-rs's `Status` enum is closed (no `Custom` variant), so we cannot set
//! an arbitrary `error.code` on the JS side. Instead we follow the same
//! convention Playwright uses for its TypeScript errors: prefix the message
//! with `"<ErrorName>: "` for the distinguishable variants. A small TS
//! wrapper in `packages/ferridriver-test` then parses the prefix and
//! rethrows a real class instance (`class TimeoutError extends Error`),
//! giving JS consumers both `err instanceof TimeoutError` and
//! `err.name === 'TimeoutError'` — parity with Playwright.
//!
//! Unnamed variants (`Backend`, `Io`, etc.) pass through unprefixed; they
//! surface as plain `Error` on the JS side, which matches Playwright's
//! behaviour for non-class errors.
//!
//! The orphan rule prevents `impl From<FerriError> for napi::Error` in this
//! crate, so we expose [`to_napi`] plus the [`IntoNapi`] extension trait.

use ferridriver::FerriError;

const NAMED_PREFIX_VARIANTS: &[&str] = &["TimeoutError", "TargetClosedError"];

#[must_use]
pub fn to_napi(err: FerriError) -> napi::Error {
  let name = err.name();
  let msg = if NAMED_PREFIX_VARIANTS.contains(&name) {
    format!("{name}: {err}")
  } else {
    err.to_string()
  };
  napi::Error::from_reason(msg)
}

/// Extension trait to convert `Result<T, FerriError>` into `napi::Result<T>`
/// with one call at NAPI boundaries.
pub trait IntoNapi<T> {
  fn into_napi(self) -> napi::Result<T>;
}

impl<T> IntoNapi<T> for Result<T, FerriError> {
  fn into_napi(self) -> napi::Result<T> {
    self.map_err(to_napi)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn timeout_message_carries_typed_prefix() {
    let core = FerriError::timeout("navigating", 30_000);
    let napi = to_napi(core);
    assert_eq!(napi.reason, "TimeoutError: Timeout 30000ms exceeded while navigating");
  }

  #[test]
  fn target_closed_message_carries_typed_prefix() {
    let core = FerriError::target_closed(Some("browser crashed".into()));
    let napi = to_napi(core);
    assert_eq!(
      napi.reason,
      "TargetClosedError: Target page, context or browser has been closed: browser crashed"
    );
  }

  #[test]
  fn strict_mode_violation_reported_without_prefix() {
    let core = FerriError::strict("button.primary", 3);
    let napi = to_napi(core);
    assert_eq!(
      napi.reason,
      r#"strict mode violation: selector "button.primary" resolved to 3 elements"#
    );
  }

  #[test]
  fn unnamed_variants_have_no_prefix() {
    let core = FerriError::Backend("launch failed".into());
    let napi = to_napi(core);
    assert_eq!(napi.reason, "backend error: launch failed");
  }

  #[test]
  fn into_napi_extension_trait_maps_err() {
    let r: Result<(), FerriError> = Err(FerriError::timeout_plain(1_000));
    let err = r.into_napi().unwrap_err();
    assert_eq!(err.reason, "TimeoutError: Timeout 1000ms exceeded");
  }

  #[test]
  fn into_napi_ok_passes_through() {
    let r: Result<u32, FerriError> = Ok(42);
    assert_eq!(r.into_napi().unwrap(), 42);
  }
}
