//! Typed error for the fetch engine.
//!
//! The WHATWG binding layer distinguishes an abort (→ `AbortError`) from
//! a generic network failure (→ `TypeError "Failed to fetch"`), so the
//! engine returns a categorized error rather than a flat string. Every
//! variant maps to `FerriError::Backend` for the Rust callers, preserving
//! the message text the existing tests assert on.

use std::fmt;

/// A fetch engine failure.
#[derive(Debug)]
pub enum FetchError {
  /// A transport failure (connection refused, reset past the retry
  /// budget, TLS error, DNS). WHATWG surfaces this as `TypeError`.
  Network(String),
  /// The request's `AbortSignal` fired.
  Abort(String),
  /// The per-request timeout elapsed.
  Timeout(String),
  /// `redirect: follow` exceeded the redirect budget.
  TooManyRedirects(u32),
  /// `redirect: error` saw a 3xx, or a redirect target had no `Location`
  /// that could be resolved.
  RedirectRefused(String),
  /// The sandbox network guard denied the URL or a resolved address.
  Blocked(String),
  /// A URL could not be parsed / resolved against the base URL.
  InvalidUrl(String),
  /// The response body could not be read.
  Body(String),
}

impl fmt::Display for FetchError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Network(m)
      | Self::Abort(m)
      | Self::Timeout(m)
      | Self::RedirectRefused(m)
      | Self::Blocked(m)
      | Self::InvalidUrl(m)
      | Self::Body(m) => f.write_str(m),
      Self::TooManyRedirects(max) => write!(f, "too many redirects (max {max})"),
    }
  }
}

impl std::error::Error for FetchError {}

impl From<FetchError> for crate::error::FerriError {
  fn from(e: FetchError) -> Self {
    crate::error::FerriError::Backend(e.to_string())
  }
}
