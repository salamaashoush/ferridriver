//! The WHATWG "extract a body" step (Fetch §7.4), in one place.
//!
//! `new Request(input, { body })`, `new Response(body, init)` and
//! `fetch(url, { body })` all take the same `BodyInit` union, so they all
//! call [`extract_body`]. They used to each recognise their own subset,
//! which is how a `Uint8Array` body reached the wire as
//! `{"0":104,"1":105}` under `content-type: application/json`.
//!
//! One deliberate deviation from WebIDL: a plain object body is
//! serialized as JSON with `application/json`, where the spec would
//! stringify it to `"[object Object]"`. That ergonomic is part of
//! ferridriver's `fetch` surface and is asserted by
//! `tests/fetch_api.rs`. It is the LAST branch, so it can never shadow a
//! real `BodyInit` type.

use ferridriver_jsstd::stream_web::ReadableStream;
use ferridriver_jsstd::utils::bytes::ObjectBytes;
use rquickjs::{Class, Coerced, Ctx, Value};

use crate::bindings::blob_bytes::blob_parts;
use crate::bindings::form_data::FormDataJs;
use ferridriver_jsstd::url::url_search_params::URLSearchParams;

/// Where the extracted body's bytes live. A `ReadableStream` body is
/// NOT drained at extraction time — the spec keeps it as the body
/// stream, and only a consumer (a body reader, or `fetch` putting it on
/// the wire) pulls it.
pub(crate) enum BodySource<'js> {
  Bytes(Vec<u8>),
  Stream(Class<'js, ReadableStream<'js>>),
}

/// An extracted body plus the `content-type` its type implies.
pub(crate) struct ExtractedBody<'js> {
  pub source: BodySource<'js>,
  /// The type the body implies, which the caller applies only when no
  /// `content-type` was given — unless [`Self::forced`].
  pub content_type: Option<String>,
  /// `FormData` picks the multipart boundary, so its content type must
  /// REPLACE any caller-supplied one; sending a caller's bare
  /// `multipart/form-data` would leave the server unable to split the
  /// parts.
  pub forced: bool,
}

impl ExtractedBody<'_> {
  fn bytes(bytes: Vec<u8>, content_type: Option<&str>) -> Self {
    Self {
      source: BodySource::Bytes(bytes),
      content_type: content_type.map(ToString::to_string),
      forced: false,
    }
  }
}

/// WHATWG "extract a body". `None` for a null/undefined/absent body.
///
/// Recognised, in spec order: `ReadableStream`, `Blob`/`File`,
/// `FormData`, `URLSearchParams`, `ArrayBuffer` / any typed array /
/// `DataView`, string — then ferridriver's JSON-object extension, then
/// the WebIDL stringification fallback for remaining primitives.
pub(crate) fn extract_body<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> rquickjs::Result<Option<ExtractedBody<'js>>> {
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
    return Ok(Some(ExtractedBody::bytes(
      s.into_bytes(),
      Some("text/plain;charset=UTF-8"),
    )));
  }
  if let Ok(stream) = Class::<ReadableStream<'js>>::from_value(v) {
    return Ok(Some(ExtractedBody {
      source: BodySource::Stream(stream),
      content_type: None,
      forced: false,
    }));
  }
  if let Ok(fd) = Class::<FormDataJs>::from_value(v) {
    let (bytes, content_type) = fd.borrow().to_multipart();
    return Ok(Some(ExtractedBody {
      source: BodySource::Bytes(bytes),
      content_type: Some(content_type),
      forced: true,
    }));
  }
  if let Some((bytes, mime)) = blob_parts(v) {
    let mime = (!mime.is_empty()).then_some(mime);
    return Ok(Some(ExtractedBody::bytes(bytes, mime.as_deref())));
  }
  if let Ok(params) = Class::<URLSearchParams>::from_value(v) {
    return Ok(Some(ExtractedBody::bytes(
      params.borrow().to_string().into_bytes(),
      Some("application/x-www-form-urlencoded;charset=UTF-8"),
    )));
  }
  // Every binary view — `ArrayBuffer`, any typed array (not just
  // `Uint8Array`), `DataView` — through the one reader that understands
  // byte offsets and lengths.
  if let Some(obj) = v.as_object()
    && let Some(bytes) = ObjectBytes::from_array_buffer(obj)?
  {
    return Ok(Some(ExtractedBody::bytes(bytes.into_bytes(ctx)?, None)));
  }
  if v.is_object()
    && let Ok(json) = crate::bindings::convert::serde_from_js::<serde_json::Value>(ctx, v.clone())
  {
    return Ok(Some(ExtractedBody::bytes(
      json.to_string().into_bytes(),
      Some("application/json"),
    )));
  }
  // WebIDL USVString fallback: numbers, booleans, symbols-with-toString.
  let text: Coerced<String> = rquickjs::FromJs::from_js(ctx, v.clone())?;
  Ok(Some(ExtractedBody::bytes(
    text.0.into_bytes(),
    Some("text/plain;charset=UTF-8"),
  )))
}
