//! Buffered [`HttpResponse`] and streaming [`HttpStreamResponse`] types.

use std::time::Duration;

use ferrijs_fetch as fetch;
use futures::StreamExt as _;

use super::{RemoteAddr, ResponseType};

/// A buffered HTTP response (the Playwright `APIResponse` view over a
/// [`fetch::Response`]): sync `status()`/`ok()`/`headers()`, async body
/// already collected.
#[derive(Debug, Clone)]
pub struct HttpResponse {
  pub(super) status_code: u16,
  pub(super) status_text: String,
  pub(super) response_url: String,
  pub(super) response_headers: fetch::Headers,
  pub(super) body_bytes: bytes::Bytes,
  pub(super) server_addr: Option<RemoteAddr>,
  pub(super) redirected: bool,
  pub(super) unfollowed_redirect: bool,
  pub(super) response_type: ResponseType,
  pub(super) timing: crate::network::RequestTiming,
  pub(super) peer_certificate: Option<Vec<u8>>,
  pub(super) disposed: bool,
}

/// Playwright's `ResourceTiming` for the hop that produced a response
/// (`server/fetch.ts::notifyRequestFinished`): milliseconds from
/// `start_time`, `-1` for a phase that did not happen. The transport
/// reports when a connection was ready and not when its TLS handshake
/// began, so `secure_connection_start` is always `-1`.
pub(super) fn resource_timing(timing: &fetch::Timing, ended: std::time::SystemTime) -> crate::network::RequestTiming {
  let ms = |offset: Duration| offset.as_secs_f64() * 1000.0;
  let since_epoch = |time: std::time::SystemTime| {
    time
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
  };
  let start_time = since_epoch(timing.start_time);
  crate::network::RequestTiming {
    // A wall-clock reading in whole milliseconds, as `Date.now()` gives
    // Playwright's; the offsets below keep their fractions.
    start_time: start_time.floor(),
    domain_lookup_start: timing.dns_end.map_or(-1.0, |_| 0.0),
    domain_lookup_end: timing.dns_end.map_or(-1.0, ms),
    connect_start: timing.connect_end.map_or(-1.0, |_| timing.dns_end.map_or(0.0, ms)),
    secure_connection_start: -1.0,
    connect_end: timing.connect_end.map_or(-1.0, ms),
    // A connection the pool already had is written to at once.
    request_start: timing.connect_end.map_or(0.0, ms),
    response_start: ms(timing.response_start),
    response_end: (since_epoch(ended) - start_time).max(ms(timing.response_start)),
  }
}

/// Playwright's `SecurityDetails` from the server's leaf certificate
/// (`server/fetch.ts::captureSecurityDetails`): the first common name of
/// the subject and of the issuer, and the validity window in seconds.
/// The transport does not report the negotiated protocol version.
fn security_details(certificate: &[u8]) -> Option<crate::network::SecurityDetails> {
  let (_, certificate) = x509_parser::parse_x509_certificate(certificate).ok()?;
  let common_name = |name: &x509_parser::x509::X509Name<'_>| {
    name
      .iter_common_name()
      .next()
      .and_then(|cn| cn.as_str().ok())
      .map(ToString::to_string)
  };
  #[expect(
    clippy::cast_precision_loss,
    reason = "X.509 times are whole seconds before year 10000, far inside the 2^53 an f64 holds exactly"
  )]
  let seconds = |time: x509_parser::time::ASN1Time| time.timestamp() as f64;
  Some(crate::network::SecurityDetails {
    protocol: None,
    subject_name: common_name(certificate.subject()),
    issuer: common_name(certificate.issuer()),
    valid_from: Some(seconds(certificate.validity().not_before)),
    valid_to: Some(seconds(certificate.validity().not_after)),
  })
}

impl HttpResponse {
  /// Buffer a [`fetch::Response`] into an `HttpResponse`.
  pub(super) async fn buffer(response: fetch::Response) -> crate::error::Result<Self> {
    let fetch::Response {
      status,
      status_text,
      url,
      headers,
      body,
      redirected,
      unfollowed_redirect,
      server_addr,
      type_,
      timing,
      peer_certificate,
    } = response;
    let body_bytes = body
      .collect()
      .await
      .map_err(|e| crate::error::FerriError::backend(e.to_string()))?;
    Ok(Self {
      status_code: status,
      status_text,
      response_url: url,
      response_headers: headers,
      body_bytes,
      server_addr,
      redirected,
      unfollowed_redirect,
      response_type: type_,
      timing: resource_timing(&timing, std::time::SystemTime::now()),
      peer_certificate,
      disposed: false,
    })
  }

  /// Playwright: `apiResponse.timing(): ResourceTiming`, the phases of
  /// the request that produced this response.
  #[must_use]
  pub fn timing(&self) -> crate::network::RequestTiming {
    self.timing
  }

  /// Playwright: `apiResponse.securityDetails()`. `None` for a response
  /// that did not arrive over TLS.
  #[must_use]
  pub fn security_details(&self) -> Option<crate::network::SecurityDetails> {
    self.peer_certificate.as_deref().and_then(security_details)
  }

  /// HTTP status code.
  #[must_use]
  pub fn status(&self) -> u16 {
    self.status_code
  }

  /// HTTP status text (e.g., "OK", "Not Found").
  #[must_use]
  pub fn status_text(&self) -> &str {
    &self.status_text
  }

  /// Final URL after redirects.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.response_url
  }

  /// Whether the response status is 200-299.
  #[must_use]
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status_code)
  }

  /// Response headers as (name, value) pairs, verbatim: duplicates and
  /// original casing preserved (Playwright's `headersArray()`).
  #[must_use]
  pub fn headers(&self) -> &[(String, String)] {
    self.response_headers.entries()
  }

  /// Flattened header object: lowercased names, combined values
  /// (Playwright's `headers()`).
  #[must_use]
  pub fn headers_object(&self) -> Vec<(String, String)> {
    self.response_headers.to_object()
  }

  /// Combined value of a header (case-insensitive), duplicates joined
  /// with `, ` — `\n` for `set-cookie`. Playwright's `RawHeaders.get`.
  #[must_use]
  pub fn header(&self, name: &str) -> Option<String> {
    self.response_headers.get(name)
  }

  /// Response body as UTF-8 string.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid UTF-8, or if the response
  /// was disposed.
  pub fn text(&self) -> crate::error::Result<String> {
    String::from_utf8(self.body()?.to_vec())
      .map_err(|e| crate::error::FerriError::evaluation(format!("response body is not UTF-8: {e}")))
  }

  /// Parse response body as JSON.
  ///
  /// # Errors
  ///
  /// Returns an error if the body cannot be deserialized, or if the
  /// response was disposed.
  pub fn json<T: serde::de::DeserializeOwned>(&self) -> crate::error::Result<T> {
    serde_json::from_slice(self.body()?).map_err(Into::into)
  }

  /// Response body as a JSON value.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid JSON, or if the response
  /// was disposed.
  pub fn json_value(&self) -> crate::error::Result<serde_json::Value> {
    self.json()
  }

  /// Raw response body bytes.
  ///
  /// # Errors
  ///
  /// Returns an error if the response was disposed.
  pub fn body(&self) -> crate::error::Result<&[u8]> {
    if self.disposed {
      return Err(crate::error::FerriError::Disposed("Response"));
    }
    Ok(&self.body_bytes)
  }

  /// The body as a refcounted [`bytes::Bytes`] — a cheap clone that
  /// shares the buffer rather than copying it.
  ///
  /// Lets a binding hand the body to its runtime without a copy (the
  /// `QuickJS` layer builds an `ArrayBuffer` directly over this
  /// allocation). Prefer [`Self::body`] when a plain slice will do.
  ///
  /// # Errors
  ///
  /// Returns an error if the response was disposed.
  pub fn body_shared(&self) -> crate::error::Result<bytes::Bytes> {
    if self.disposed {
      return Err(crate::error::FerriError::Disposed("Response"));
    }
    Ok(self.body_bytes.clone())
  }

  /// Resolved peer address (`{ ipAddress, port }`), or `None` when the
  /// transport didn't surface one. Playwright:
  /// `apiResponse.serverAddr(): Promise<RemoteAddr | null>`.
  #[must_use]
  pub fn server_addr(&self) -> Option<&RemoteAddr> {
    self.server_addr.as_ref()
  }

  /// Whether at least one redirect hop was followed (WHATWG
  /// `Response.redirected`).
  #[must_use]
  pub fn redirected(&self) -> bool {
    self.redirected
  }

  /// Whether this 3xx was returned without following because
  /// `redirect: manual` was requested — the JS layer maps it to an
  /// opaque-redirect `Response`.
  #[must_use]
  pub fn unfollowed_redirect(&self) -> bool {
    self.unfollowed_redirect
  }

  /// WHATWG `Response.type` (basic / opaqueredirect / …).
  #[must_use]
  pub fn response_type(&self) -> ResponseType {
    self.response_type
  }

  /// Playwright: `apiResponse.dispose()` — release the buffered body.
  /// Status, headers and URL stay readable; `text()`/`json()`/`body()`
  /// then fail with [`FerriError::Disposed`](crate::error::FerriError).
  pub fn dispose(&mut self) {
    self.body_bytes = bytes::Bytes::new();
    self.disposed = true;
  }
}

/// A response whose body has NOT been buffered: status/headers are
/// available immediately, body bytes are pulled incrementally with
/// [`Self::chunk`]. Produced by [`super::HttpClient::fetch_stream`]; backs a
/// WHATWG `Response.body` `ReadableStream`.
pub struct HttpStreamResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: fetch::Headers,
  server_addr: Option<RemoteAddr>,
  redirected: bool,
  unfollowed_redirect: bool,
  response_type: ResponseType,
  stream: fetch::ByteStream,
}

impl std::fmt::Debug for HttpStreamResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HttpStreamResponse")
      .field("status_code", &self.status_code)
      .field("response_url", &self.response_url)
      .field("redirected", &self.redirected)
      .finish_non_exhaustive()
  }
}

impl HttpStreamResponse {
  pub(super) fn from_response(response: fetch::Response) -> Self {
    let fetch::Response {
      status,
      status_text,
      url,
      headers,
      body,
      redirected,
      unfollowed_redirect,
      server_addr,
      type_,
      ..
    } = response;
    Self {
      status_code: status,
      status_text,
      response_url: url,
      response_headers: headers,
      server_addr,
      redirected,
      unfollowed_redirect,
      response_type: type_,
      stream: body.into_stream(),
    }
  }

  #[must_use]
  pub fn status(&self) -> u16 {
    self.status_code
  }

  #[must_use]
  pub fn status_text(&self) -> &str {
    &self.status_text
  }

  #[must_use]
  pub fn url(&self) -> &str {
    &self.response_url
  }

  #[must_use]
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status_code)
  }

  /// Response headers verbatim: duplicates and casing preserved.
  #[must_use]
  pub fn headers(&self) -> &[(String, String)] {
    self.response_headers.entries()
  }

  /// Flattened header object: lowercased names, combined values.
  #[must_use]
  pub fn headers_object(&self) -> Vec<(String, String)> {
    self.response_headers.to_object()
  }

  /// Combined value of a header (case-insensitive), duplicates joined
  /// with `, ` — `\n` for `set-cookie`.
  #[must_use]
  pub fn header(&self, name: &str) -> Option<String> {
    self.response_headers.get(name)
  }

  /// Resolved peer address, or `None` when the transport didn't surface
  /// one (so `apiResponse.serverAddr()` works through the streamed path).
  #[must_use]
  pub fn server_addr(&self) -> Option<&RemoteAddr> {
    self.server_addr.as_ref()
  }

  /// Whether at least one redirect hop was followed (`Response.redirected`).
  #[must_use]
  pub fn redirected(&self) -> bool {
    self.redirected
  }

  /// Whether this 3xx was returned unfollowed because `redirect: manual`.
  #[must_use]
  pub fn unfollowed_redirect(&self) -> bool {
    self.unfollowed_redirect
  }

  /// WHATWG `Response.type` (basic / opaqueredirect / …).
  #[must_use]
  pub fn response_type(&self) -> ResponseType {
    self.response_type
  }

  /// Next body chunk, or `None` at end of stream.
  ///
  /// # Errors
  ///
  /// Returns an error if reading the body fails (connection reset, etc).
  pub async fn chunk(&mut self) -> crate::error::Result<Option<bytes::Bytes>> {
    self
      .stream
      .next()
      .await
      .transpose()
      .map_err(|e| crate::error::FerriError::backend(e.to_string()))
  }
}
