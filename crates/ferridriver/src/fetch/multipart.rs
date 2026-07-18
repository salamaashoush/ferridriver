//! `multipart/form-data` serialization, shared by the request-option
//! lowering and the JS `FormData` body path so both express multipart
//! identically. Mirrors Playwright's `FormField`.

/// One field of a `multipart/form-data` body — a plain text value or an
/// uploaded file part. Mirrors Playwright's `FormField`
/// (`multipartData: { name, value } | { name, file: { name, mimeType, buffer } }`).
#[derive(Debug, Clone)]
pub struct MultipartField {
  pub name: String,
  pub value: MultipartValue,
}

#[derive(Debug, Clone)]
pub enum MultipartValue {
  /// A scalar text field.
  Text(String),
  /// A file part with an explicit filename + content type.
  File {
    filename: String,
    content_type: String,
    bytes: Vec<u8>,
  },
}

impl MultipartField {
  /// Lower Playwright's `multipart` option bag — a map of
  /// `string | number | boolean | { name, mimeType, buffer }` — into
  /// fields.
  ///
  /// `buffer` accepts a byte array or a string (the natural JSON shape of
  /// a `Buffer` / `Uint8Array` crossing either binding boundary). Both
  /// bindings call this so a `multipart` bag means exactly one thing.
  ///
  /// # Errors
  ///
  /// Returns a message naming the offending key when a value is neither
  /// a scalar nor a well-formed file descriptor.
  pub fn from_json_map<I>(fields: I) -> Result<Vec<Self>, String>
  where
    I: IntoIterator<Item = (String, serde_json::Value)>,
  {
    fields
      .into_iter()
      .map(|(name, value)| {
        let value = match value {
          serde_json::Value::String(s) => MultipartValue::Text(s),
          serde_json::Value::Number(n) => MultipartValue::Text(n.to_string()),
          serde_json::Value::Bool(b) => MultipartValue::Text(b.to_string()),
          serde_json::Value::Object(obj) => {
            let filename = obj
              .get("name")
              .and_then(serde_json::Value::as_str)
              .ok_or_else(|| format!("multipart[{name:?}]: a file field needs a string `name`"))?
              .to_string();
            let content_type = obj
              .get("mimeType")
              .and_then(serde_json::Value::as_str)
              .unwrap_or("application/octet-stream")
              .to_string();
            let bytes = match obj.get("buffer") {
              Some(serde_json::Value::String(s)) => s.clone().into_bytes(),
              Some(array @ serde_json::Value::Array(_)) => serde_json::from_value::<Vec<u8>>(array.clone())
                .map_err(|_| format!("multipart[{name:?}]: `buffer` must be bytes or a string"))?,
              _ => return Err(format!("multipart[{name:?}]: a file field needs a `buffer`")),
            };
            MultipartValue::File {
              filename,
              content_type,
              bytes,
            }
          },
          other => {
            return Err(format!(
              "multipart[{name:?}] must be a string, number, boolean, or {{ name, mimeType, buffer }} (got {other})"
            ));
          },
        };
        Ok(Self { name, value })
      })
      .collect()
  }
}

/// Serialize `multipart/form-data` fields into a body + the matching
/// `content-type` header value (with the boundary). Field names /
/// filenames are written into the part headers verbatim (the caller
/// controls them).
#[must_use]
pub fn serialize_multipart(fields: &[MultipartField], boundary: &str) -> (Vec<u8>, String) {
  let mut body = Vec::new();
  for field in fields {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    match &field.value {
      MultipartValue::Text(text) => {
        body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", field.name).as_bytes());
        body.extend_from_slice(text.as_bytes());
      },
      MultipartValue::File {
        filename,
        content_type,
        bytes,
      } => {
        body.extend_from_slice(
          format!(
            "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n",
            field.name
          )
          .as_bytes(),
        );
        body.extend_from_slice(bytes);
      },
    }
    body.extend_from_slice(b"\r\n");
  }
  body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
  (body, format!("multipart/form-data; boundary={boundary}"))
}

/// A process-unique multipart boundary. Deterministic construction (no
/// RNG dependency): a fixed prefix + a monotonic counter.
#[must_use]
pub(crate) fn multipart_boundary() -> String {
  use std::sync::atomic::{AtomicU64, Ordering};
  static SEQ: AtomicU64 = AtomicU64::new(0);
  let n = SEQ.fetch_add(1, Ordering::Relaxed);
  format!("----ferridriverBoundary{n:016x}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn multipart_serialization_shape() {
    let fields = vec![
      MultipartField {
        name: "text".into(),
        value: MultipartValue::Text("val".into()),
      },
      MultipartField {
        name: "file".into(),
        value: MultipartValue::File {
          filename: "f.bin".into(),
          content_type: "application/octet-stream".into(),
          bytes: vec![1, 2, 3],
        },
      },
    ];
    let (body, content_type) = serialize_multipart(&fields, "BOUND");
    assert_eq!(content_type, "multipart/form-data; boundary=BOUND");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("--BOUND\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\nval\r\n"));
    assert!(text.contains("name=\"file\"; filename=\"f.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"));
    assert!(text.ends_with("--BOUND--\r\n"));
  }

  #[test]
  fn boundaries_are_unique() {
    assert_ne!(multipart_boundary(), multipart_boundary());
  }
}
