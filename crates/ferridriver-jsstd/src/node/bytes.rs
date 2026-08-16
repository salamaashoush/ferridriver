//! Byte extraction: the one place a JS value becomes `Vec<u8>`.
//!
//! `BufferSource` (an `ArrayBuffer` or any view over one), Node's
//! `Buffer`, an array of byte values, or a string in one of the encodings
//! `Buffer` understands. Every consumer — `crypto`, the compression
//! streams, `Buffer.from`, `setInputFiles` — reads through here rather
//! than repeating the walk.

use base64::Engine as _;
use rquickjs::{ArrayBuffer, Ctx, Value};

use super::buffer::BufferJs;
use super::throw_named;

/// A `BufferSource`: an `ArrayBuffer`, or a view over one.
///
/// # Errors
///
/// A `TypeError` when the value is neither, or when the buffer is
/// detached or the view is out of bounds.
pub fn buffer_source_bytes(ctx: &Ctx<'_>, value: &Value<'_>) -> rquickjs::Result<Vec<u8>> {
  if let Some(ab) = ArrayBuffer::from_value(value.clone()) {
    return ab
      .as_bytes()
      .map(<[u8]>::to_vec)
      .ok_or_else(|| throw_named(ctx, "TypeError", "detached ArrayBuffer"));
  }
  if let Some(obj) = value.as_object() {
    let buffer: rquickjs::Result<ArrayBuffer<'_>> = obj.get("buffer");
    if let Ok(ab) = buffer {
      let offset: usize = obj.get("byteOffset")?;
      let len: usize = obj.get("byteLength")?;
      let bytes = ab
        .as_bytes()
        .ok_or_else(|| throw_named(ctx, "TypeError", "detached ArrayBuffer"))?;
      return bytes
        .get(offset..offset + len)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| throw_named(ctx, "TypeError", "view out of bounds"));
    }
  }
  Err(throw_named(
    ctx,
    "TypeError",
    "expected an ArrayBuffer or ArrayBuffer view",
  ))
}

/// Decode a string under one of the encodings `Buffer` supports.
fn decode(ctx: &Ctx<'_>, s: &str, encoding: &str) -> rquickjs::Result<Vec<u8>> {
  match encoding {
    "utf8" | "utf-8" => Ok(s.as_bytes().to_vec()),
    "base64" => base64::engine::general_purpose::STANDARD
      .decode(s)
      .map_err(|e| throw_named(ctx, "TypeError", format!("invalid base64: {e}"))),
    "hex" => (0..s.len() / 2)
      .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
      .collect::<Result<Vec<u8>, _>>()
      .map_err(|e| throw_named(ctx, "TypeError", format!("invalid hex: {e}"))),
    other => Err(throw_named(
      ctx,
      "TypeError",
      format!("unsupported Buffer encoding {other:?} (utf8 | base64 | hex)"),
    )),
  }
}

/// Node's `Buffer.from` lowering: a string in `encoding`, an array of
/// byte values, another `Buffer`, or any `BufferSource`.
///
/// # Errors
///
/// A `TypeError` for an unsupported encoding or a value that is none of
/// those.
pub fn value_to_bytes<'js>(
  ctx: &Ctx<'js>,
  value: &Value<'js>,
  encoding: Option<&str>,
) -> rquickjs::Result<Vec<u8>> {
  if let Some(s) = value.as_string() {
    return decode(ctx, &s.to_string()?, encoding.unwrap_or("utf8"));
  }
  if let Some(obj) = value.as_object() {
    if let Some(buf) = obj.as_class::<BufferJs>() {
      return Ok(buf.borrow().bytes().to_vec());
    }
    if let Some(arr) = obj.as_array() {
      let mut out = Vec::with_capacity(arr.len());
      for i in 0..arr.len() {
        out.push(arr.get::<u8>(i)?);
      }
      return Ok(out);
    }
  }
  buffer_source_bytes(ctx, value)
}
