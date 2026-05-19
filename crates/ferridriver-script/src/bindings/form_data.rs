//! WHATWG `FormData` (spec subset, no deps; multipart serialization
//! studied from the read-only llrt reference). `append`/`set`/`get`/
//! `getAll`/`has`/`delete`/`keys`/`values`/`entries`/`forEach`; string
//! or `Blob`/`File`-ish values. `entries`/`keys`/`values` return arrays
//! (iterable: `for..of fd.entries()`, spread, `Array.from`, `forEach`);
//! `[Symbol.iterator]` is the entries array. `fetch` with a `FormData`
//! body serializes `multipart/form-data` in-binding (no core change).

use std::sync::atomic::{AtomicU64, Ordering};

use rquickjs::atom::PredefinedAtom;
use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, Function, Value, class::Trace};

use crate::bindings::blob::BlobJs;

#[derive(Clone)]
enum FormEntry {
  Text(String),
  File {
    bytes: Vec<u8>,
    filename: String,
    content_type: String,
  },
}

#[derive(Trace, Default)]
#[rquickjs::class(rename = "FormData")]
pub struct FormDataJs {
  #[qjs(skip_trace)]
  entries: Vec<(String, FormEntry)>,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for FormDataJs {
  type Changed<'to> = FormDataJs;
}

impl FormDataJs {
  fn coerce(value: &Value<'_>, filename: Option<String>) -> FormEntry {
    if let Some((bytes, ct)) = BlobJs::from_js_blob(value) {
      return FormEntry::File {
        bytes,
        filename: filename.unwrap_or_else(|| "blob".to_string()),
        content_type: if ct.is_empty() {
          "application/octet-stream".to_string()
        } else {
          ct
        },
      };
    }
    let s = value
      .as_string()
      .and_then(|s| s.to_string().ok())
      .or_else(|| value.as_number().map(|n| n.to_string()))
      .or_else(|| value.as_bool().map(|b| b.to_string()))
      .unwrap_or_default();
    FormEntry::Text(s)
  }

  fn entry_value<'js>(ctx: &Ctx<'js>, e: &FormEntry) -> rquickjs::Result<Value<'js>> {
    match e {
      FormEntry::Text(s) => Ok(rquickjs::String::from_str(ctx.clone(), s)?.into_value()),
      FormEntry::File {
        bytes, content_type, ..
      } => {
        let blob = Class::instance(ctx.clone(), BlobJs::new_parts(bytes.clone(), content_type.clone()))?;
        Ok(blob.into_value())
      },
    }
  }

  /// `(multipart-body, content-type)` for a `fetch` `FormData` body.
  pub fn to_multipart(&self) -> (Vec<u8>, String) {
    use std::io::Write as _;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos());
    let boundary = format!(
      "----ferridriverFormBoundary{:x}{:x}",
      nanos,
      SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let mut body = Vec::new();
    for (name, value) in &self.entries {
      match value {
        FormEntry::Text(text) => {
          let _ = write!(
            &mut body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{text}\r\n"
          );
        },
        FormEntry::File {
          bytes,
          filename,
          content_type,
        } => {
          let _ = write!(
            &mut body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
          );
          body.extend_from_slice(bytes);
          body.extend_from_slice(b"\r\n");
        },
      }
    }
    let _ = write!(&mut body, "--{boundary}--\r\n");
    (body, format!("multipart/form-data; boundary={boundary}"))
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl FormDataJs {
  #[qjs(constructor)]
  pub fn new() -> Self {
    Self::default()
  }

  #[qjs(rename = "append")]
  pub fn append(&mut self, name: String, value: Value<'_>, filename: Opt<String>) {
    self.entries.push((name, Self::coerce(&value, filename.0)));
  }

  #[qjs(rename = "set")]
  pub fn set(&mut self, name: String, value: Value<'_>, filename: Opt<String>) {
    let entry = Self::coerce(&value, filename.0);
    // Spec: replace the FIRST entry of `name` in place and drop the
    // rest; append if none — order of the first occurrence is kept.
    if let Some(i) = self.entries.iter().position(|(k, _)| k == &name) {
      self.entries[i].1 = entry;
      let mut seen = false;
      self.entries.retain(|(k, _)| {
        if k == &name {
          if seen {
            return false;
          }
          seen = true;
        }
        true
      });
    } else {
      self.entries.push((name, entry));
    }
  }

  #[qjs(rename = "has")]
  pub fn has(&self, name: String) -> bool {
    self.entries.iter().any(|(k, _)| k == &name)
  }

  #[qjs(rename = "delete")]
  pub fn delete(&mut self, name: String) {
    self.entries.retain(|(k, _)| k != &name);
  }

  #[qjs(rename = "get")]
  pub fn get<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    match self.entries.iter().find(|(k, _)| k == &name) {
      Some((_, e)) => Self::entry_value(&ctx, e),
      None => Ok(Value::new_null(ctx)),
    }
  }

  #[qjs(rename = "getAll")]
  pub fn get_all<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Vec<Value<'js>>> {
    self
      .entries
      .iter()
      .filter(|(k, _)| k == &name)
      .map(|(_, e)| Self::entry_value(&ctx, e))
      .collect()
  }

  #[qjs(rename = "keys")]
  pub fn keys(&self) -> Vec<String> {
    self.entries.iter().map(|(k, _)| k.clone()).collect()
  }

  #[qjs(rename = "values")]
  pub fn values<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Vec<Value<'js>>> {
    self.entries.iter().map(|(_, e)| Self::entry_value(&ctx, e)).collect()
  }

  #[qjs(rename = "entries")]
  pub fn entries<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Vec<Vec<Value<'js>>>> {
    self
      .entries
      .iter()
      .map(|(k, e)| {
        Ok(vec![
          rquickjs::String::from_str(ctx.clone(), k)?.into_value(),
          Self::entry_value(&ctx, e)?,
        ])
      })
      .collect()
  }

  #[qjs(rename = PredefinedAtom::SymbolIterator)]
  pub fn js_iter<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Vec<Vec<Value<'js>>>> {
    self.entries(ctx)
  }

  #[qjs(rename = "forEach")]
  pub fn for_each<'js>(&self, ctx: Ctx<'js>, cb: Function<'js>) -> rquickjs::Result<()> {
    for (k, e) in &self.entries {
      let v = Self::entry_value(&ctx, e)?;
      cb.call::<_, ()>((v, k.clone()))?;
    }
    Ok(())
  }
}
