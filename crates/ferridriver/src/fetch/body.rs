//! Request / response body.
//!
//! A [`Body`] is `Empty`, a buffered `Bytes`, a boxed byte stream, or —
//! for a live network response — the unread reqwest response (kept whole
//! so a buffered read still gets reqwest's per-request timeout). The
//! reqwest handle never leaks: the variants are private and callers go
//! through [`Body::collect`] (buffer) or [`Body::into_stream`] (stream).

use bytes::Bytes;
use futures::{Stream, StreamExt, TryStreamExt};
use std::pin::Pin;

use super::error::FetchError;

/// A boxed byte stream yielding chunks as they arrive.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, FetchError>> + Send>>;

/// A fetch body. Single-use: reading it (buffer or stream) consumes it.
pub struct Body(Inner);

enum Inner {
  Empty,
  Bytes(Bytes),
  Stream(ByteStream),
  /// A live network response, unread. Buffering goes through reqwest's
  /// own `bytes()` so the request-level timeout still covers the body.
  Response(Box<reqwest::Response>),
}

impl Body {
  #[must_use]
  pub fn empty() -> Self {
    Self(Inner::Empty)
  }

  #[must_use]
  pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
    Self(Inner::Bytes(bytes.into()))
  }

  #[must_use]
  pub fn from_stream(stream: ByteStream) -> Self {
    Self(Inner::Stream(stream))
  }

  pub(crate) fn from_response(response: reqwest::Response) -> Self {
    Self(Inner::Response(Box::new(response)))
  }

  /// The buffered request bytes to send (and re-send across redirect
  /// hops). A statically-empty body is `None`; a streamed/network body is
  /// not a valid request payload here and also yields `None`.
  pub(crate) fn into_request_bytes(self) -> Option<Bytes> {
    match self.0 {
      Inner::Bytes(b) => Some(b),
      Inner::Empty | Inner::Stream(_) | Inner::Response(_) => None,
    }
  }

  /// Whether this body is statically empty (no bytes, not a stream).
  #[must_use]
  pub fn is_empty(&self) -> bool {
    matches!(self.0, Inner::Empty)
  }

  /// Buffer the whole body into `Bytes`.
  ///
  /// # Errors
  ///
  /// Returns [`FetchError::Body`] if a stream chunk or the network read
  /// fails.
  pub async fn collect(self) -> Result<Bytes, FetchError> {
    match self.0 {
      Inner::Empty => Ok(Bytes::new()),
      Inner::Bytes(b) => Ok(b),
      Inner::Response(resp) => resp
        .bytes()
        .await
        .map_err(|e| FetchError::Body(format!("read response body: {e}"))),
      Inner::Stream(mut s) => {
        let mut buf = Vec::new();
        while let Some(chunk) = s.next().await {
          buf.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(buf))
      },
    }
  }

  /// Convert the body into a chunk stream (for a WHATWG `Response.body`
  /// `ReadableStream`). An empty body yields an empty stream.
  #[must_use]
  pub fn into_stream(self) -> ByteStream {
    match self.0 {
      Inner::Empty => futures::stream::empty().boxed(),
      Inner::Bytes(b) => futures::stream::once(async move { Ok(b) }).boxed(),
      Inner::Stream(s) => s,
      Inner::Response(resp) => resp
        .bytes_stream()
        .map_err(|e| FetchError::Body(format!("read response body: {e}")))
        .boxed(),
    }
  }
}

impl std::fmt::Debug for Body {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match &self.0 {
      Inner::Empty => f.write_str("Body::Empty"),
      Inner::Bytes(b) => write!(f, "Body::Bytes({} bytes)", b.len()),
      Inner::Stream(_) => f.write_str("Body::Stream"),
      Inner::Response(_) => f.write_str("Body::Response"),
    }
  }
}
