//! Reading bytes back out of a `Blob` or `File`.
//!
//! The classes are the vendored ones (`crate::buffer`); this is
//! the synchronous accessor the request-body and form-data paths need,
//! which the JS surface only exposes as promises.

use crate::buffer::{Blob, File};
use rquickjs::{Class, Value};

/// Bytes + MIME type of a value that is a `Blob` — or a `File`, which is
/// one.
pub fn blob_parts(value: &Value<'_>) -> Option<(Vec<u8>, String)> {
  if let Ok(blob) = Class::<Blob<'_>>::from_value(value) {
    let blob = blob.borrow();
    return Some((blob.get_bytes(), blob.mime_type()));
  }
  file_parts(value).map(|(bytes, mime, _)| (bytes, mime))
}

/// Bytes + MIME type + filename of a value that is a `File`.
pub fn file_parts(value: &Value<'_>) -> Option<(Vec<u8>, String, String)> {
  let file = Class::<File<'_>>::from_value(value).ok()?;
  let file = file.borrow();
  let blob = file.get_blob();
  Some((blob.get_bytes(), file.mime_type(), file.name()))
}
