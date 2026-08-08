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

/// Build a request-body stream fed by a channel.
///
/// The producer is whoever owns the source — for the WHATWG `fetch`
/// global that is a pump running on the `QuickJS` thread, which cannot
/// hand out a `Send` stream of its own. Core owns the wire types
/// (`Bytes`, [`FetchError`]) so the binding layer sends plain
/// `Vec<u8>` / `String` and needs no `bytes` dependency.
///
/// An `Err` from the producer fails the body rather than ending it, so a
/// source that breaks mid-send cannot be mistaken for a complete
/// payload.
#[must_use]
pub fn channel_stream(rx: tokio::sync::mpsc::Receiver<Result<Vec<u8>, String>>) -> ByteStream {
  futures::stream::unfold(rx, |mut rx| async move {
    let chunk = rx.recv().await?;
    let item = match chunk {
      Ok(bytes) => Ok(Bytes::from(bytes)),
      Err(message) => Err(FetchError::Body(message)),
    };
    Some((item, rx))
  })
  .boxed()
}

/// What a [`Body`] contributes to an outgoing request.
///
/// The distinction matters to the redirect loop: [`Self::Bytes`] can be
/// re-sent on every hop and re-tried after a connection reset, while
/// [`Self::Stream`] is consumed by the first hop and cannot be replayed.
pub(crate) enum RequestPayload {
  Empty,
  Bytes(Bytes),
  Stream(ByteStream),
  /// A response body was handed to a request — a caller bug.
  Invalid,
}

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

  /// The payload to put on the wire, distinguishing a replayable
  /// buffered body from a single-use stream.
  pub(crate) fn into_request_payload(self) -> RequestPayload {
    match self.0 {
      Inner::Empty => RequestPayload::Empty,
      Inner::Bytes(b) => RequestPayload::Bytes(b),
      Inner::Stream(s) => RequestPayload::Stream(s),
      // A live response is a RESPONSE body; it is never a request
      // payload. Treating it as empty would send a silently bodyless
      // request, so this is a caller bug worth surfacing.
      Inner::Response(_) => RequestPayload::Invalid,
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
