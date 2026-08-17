//! The bridge between the JS `FormData` class and the core multipart
//! wire types.
//!
//! `FormData` itself is a plain entry list in `ferridriver-jsstd`; the
//! conversion to `ferridriver::http_client::MultipartField` lives here so
//! the standard-library crate keeps no dependency on the automation
//! stack. A `FormData` body is written by the CORE serializer — the same
//! one the Playwright `multipart` option bag uses, so both produce
//! identical bodies.

use ferridriver::http_client::{MultipartField, MultipartValue, multipart_boundary, serialize_multipart};
use ferridriver_jsstd::web::form_data::{FormDataJs, FormEntry};

/// A parsed `multipart/form-data` body as `FormData`. A part with a
/// filename reads back as a `File`, matching how `append(name, file)`
/// stored it.
pub fn form_data_from_fields(fields: &[MultipartField]) -> FormDataJs {
  FormDataJs::from_entries(
    fields
      .iter()
      .map(|field| {
        let entry = match &field.value {
          MultipartValue::Text(text) => FormEntry::Text(text.clone()),
          MultipartValue::File {
            filename,
            content_type,
            bytes,
          } => FormEntry::File {
            bytes: bytes.clone(),
            filename: filename.clone(),
            content_type: content_type.clone(),
          },
        };
        (field.name.clone(), entry)
      })
      .collect(),
  )
}

/// The entries as core multipart fields.
pub fn form_data_to_fields(form: &FormDataJs) -> Vec<MultipartField> {
  form
    .entries_slice()
    .iter()
    .map(|(name, entry)| MultipartField {
      name: name.clone(),
      value: match entry {
        FormEntry::Text(text) => MultipartValue::Text(text.clone()),
        FormEntry::File {
          bytes,
          filename,
          content_type,
        } => MultipartValue::File {
          filename: filename.clone(),
          content_type: content_type.clone(),
          bytes: bytes.clone(),
        },
      },
    })
    .collect()
}

/// `(multipart-body, content-type)` for a `fetch` `FormData` body.
pub fn form_data_to_multipart(form: &FormDataJs) -> (Vec<u8>, String) {
  serialize_multipart(&form_data_to_fields(form), &multipart_boundary())
}
