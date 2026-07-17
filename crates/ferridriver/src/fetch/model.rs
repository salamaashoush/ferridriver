//! The WHATWG request/response model the engine sends and returns.
//!
//! [`Request`] is fully resolved before it reaches the engine (absolute
//! URL, params appended, headers assembled, body materialized): the
//! engine is format-agnostic. [`Response`] carries the metadata plus a
//! single-use [`Body`].

use std::time::Duration;

use super::body::Body;
use super::headers::Headers;
use super::net_guard::NetGuard;

/// How a redirect response is handled, mirroring WHATWG Fetch's
/// `RequestInit.redirect` (`follow` | `manual` | `error`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RedirectMode {
  /// Follow up to the redirect cap ([`Request::max_redirects`]), then
  /// error (the default).
  #[default]
  Follow,
  /// Do not follow: the 3xx response is returned as-is (the JS layer
  /// turns this into an opaque-redirect `Response`).
  Manual,
  /// Treat any redirect as a network error.
  Error,
}

/// WHATWG `RequestCredentials` — whether stored cookies ride the request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Credentials {
  /// Never send credentials (cookies).
  Omit,
  /// Send credentials (the default for this engine, and what Playwright's
  /// `request` always does — the browser context is the jar).
  #[default]
  SameOrigin,
  /// Send credentials on cross-origin requests too. Same behaviour as
  /// `SameOrigin` here since the engine has a single origin scope.
  Include,
}

/// WHATWG `Response.type`: how the response was filtered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResponseType {
  /// A same-origin (unfiltered) response.
  #[default]
  Basic,
  /// A CORS-filtered response.
  Cors,
  /// An opaque cross-origin `no-cors` response.
  Opaque,
  /// An opaque `redirect: manual` 3xx.
  OpaqueRedirect,
  /// A network-error response (`Response.error()`).
  Error,
  /// A constructed response with no filtering applied.
  Default,
}

impl ResponseType {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Basic => "basic",
      Self::Cors => "cors",
      Self::Opaque => "opaque",
      Self::OpaqueRedirect => "opaqueredirect",
      Self::Error => "error",
      Self::Default => "default",
    }
  }
}

/// Resolved peer address of a response. Mirrors Playwright's
/// `RemoteAddr` (`{ ipAddress, port }`) returned by
/// `apiResponse.serverAddr()` / `response.serverAddr()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteAddr {
  #[serde(rename = "ipAddress")]
  pub ip_address: String,
  pub port: u16,
}

/// A fully-resolved request the engine can send as-is.
#[derive(Debug)]
pub struct Request {
  pub method: reqwest::Method,
  /// Absolute URL with query params already appended.
  pub url: reqwest::Url,
  /// Assembled request headers (content-type already set for bodies).
  pub headers: Headers,
  pub body: Body,
  pub redirect: RedirectMode,
  pub credentials: Credentials,
  /// Redirect cap for `redirect: follow`: `Some(0)` = don't follow,
  /// `Some(n)` = follow up to `n`, `None` = the engine default (20).
  pub max_redirects: Option<u32>,
  /// Retry the request on a connection reset up to this many times.
  pub max_retries: u32,
  pub timeout: Duration,
  /// Resolved TLS posture for this request.
  pub ignore_https_errors: bool,
  /// Sandbox network policy, enforced on the initial URL, every redirect
  /// hop, and every resolved address when active.
  pub net_guard: Option<NetGuard>,
}

/// The engine's response: metadata plus a single-use body.
#[derive(Debug)]
pub struct Response {
  pub status: u16,
  pub status_text: String,
  /// Final URL after any followed redirects.
  pub url: String,
  pub headers: Headers,
  pub body: Body,
  /// Whether at least one redirect hop was followed.
  pub redirected: bool,
  /// Whether a 3xx was returned unfollowed because `redirect: manual`.
  pub unfollowed_redirect: bool,
  pub server_addr: Option<RemoteAddr>,
  pub type_: ResponseType,
}

impl Response {
  #[must_use]
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status)
  }
}
