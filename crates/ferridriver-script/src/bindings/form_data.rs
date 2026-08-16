//! WHATWG `FormData` (spec subset, no deps; multipart serialization
//! studied from the read-only llrt reference). `append`/`set`/`get`/
//! `getAll`/`has`/`delete`/`keys`/`values`/`entries`/`forEach`; string,
//! `Blob` or `File` values. `entries`/`keys`/`values`/`[Symbol.iterator]`
//! return real live iterators (see [`super::js_iterator`]). A file entry
//! reads back as a `File` carrying the filename it was stored under, and
//! appending a `File` supplies that filename without repeating it.
//! A `FormData` used as a `fetch` body lowers to the core
//! [`MultipartField`](ferridriver::http_client::MultipartField) list and
//! is written by the core serializer — the same one the Playwright
//! `multipart` option bag uses, so both produce identical bodies.

use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Opt, This};
use rquickjs::{Class, Ctx, Function, Object, Value, class::Trace};

use crate::bindings::blob_bytes::{blob_parts, file_parts};
use crate::bindings::js_iterator::live_iterator;

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
    // A `File` carries its own name, so `fd.append('f', file)` needs no
    // explicit filename; an explicit one still wins, per spec.
    if let Some((bytes, ct, name)) = file_parts(value) {
      return FormEntry::File {
        bytes,
        filename: filename.unwrap_or(name),
        content_type: if ct.is_empty() {
          "application/octet-stream".to_string()
        } else {
          ct
        },
      };
    }
    if let Some((bytes, ct)) = blob_parts(value) {
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

  /// Spec: a file entry reads back as a `File` (carrying the filename it
  /// was stored under), a text entry as a string.
  fn entry_value<'js>(ctx: &Ctx<'js>, e: &FormEntry) -> rquickjs::Result<Value<'js>> {
    match e {
      FormEntry::Text(s) => Ok(rquickjs::String::from_str(ctx.clone(), s)?.into_value()),
      FormEntry::File {
        bytes,
        content_type,
        filename,
      } => {
        let file = Class::instance(
          ctx.clone(),
          ferridriver_jsstd::buffer::File::from_bytes(
            ctx,
            bytes.clone(),
            filename.clone(),
            Some(content_type.clone()),
          )?,
        )?;
        Ok(file.into_value())
      },
    }
  }

  fn project_entry<'js>(
    ctx: &Ctx<'js>,
    parent: &Class<'js, Self>,
    index: usize,
  ) -> rquickjs::Result<Option<Value<'js>>> {
    let Some((name, entry)) = parent.borrow().entries.get(index).cloned() else {
      return Ok(None);
    };
    let pair = rquickjs::Array::new(ctx.clone())?;
    pair.set(0, rquickjs::String::from_str(ctx.clone(), &name)?)?;
    pair.set(1, Self::entry_value(ctx, &entry)?)?;
    Ok(Some(pair.into_value()))
  }

  fn project_key<'js>(ctx: &Ctx<'js>, parent: &Class<'js, Self>, index: usize) -> rquickjs::Result<Option<Value<'js>>> {
    let Some((name, _)) = parent.borrow().entries.get(index).cloned() else {
      return Ok(None);
    };
    Ok(Some(rquickjs::String::from_str(ctx.clone(), &name)?.into_value()))
  }

  fn project_value<'js>(
    ctx: &Ctx<'js>,
    parent: &Class<'js, Self>,
    index: usize,
  ) -> rquickjs::Result<Option<Value<'js>>> {
    let Some((_, entry)) = parent.borrow().entries.get(index).cloned() else {
      return Ok(None);
    };
    Self::entry_value(ctx, &entry).map(Some)
  }

  /// Build from parsed `multipart/form-data` fields — the read side of
  /// the `formData()` body mixin. A part with a filename reads back as a
  /// `File`, matching how `append(name, file)` stored it.
  pub fn from_multipart_fields(fields: &[ferridriver::http_client::MultipartField]) -> Self {
    use ferridriver::http_client::MultipartValue;
    Self {
      entries: fields
        .iter()
        .map(|f| {
          let entry = match &f.value {
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
          (f.name.clone(), entry)
        })
        .collect(),
    }
  }

  /// Build from an `application/x-www-form-urlencoded` body: `+` decodes
  /// to a space and every entry is text (the format cannot carry files).
  pub fn from_urlencoded(body: &str) -> Self {
    Self {
      entries: url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), FormEntry::Text(v.into_owned())))
        .collect(),
    }
  }

  /// The entries as core multipart fields — the same representation the
  /// Playwright `multipart` option bag lowers into, so both reach the
  /// wire through one serializer.
  pub fn to_fields(&self) -> Vec<ferridriver::http_client::MultipartField> {
    use ferridriver::http_client::{MultipartField, MultipartValue};
    self
      .entries
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
  pub fn to_multipart(&self) -> (Vec<u8>, String) {
    ferridriver::http_client::serialize_multipart(&self.to_fields(), &ferridriver::http_client::multipart_boundary())
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl FormDataJs {
  /// Spec: every platform object carries `Symbol.toStringTag`, so
  /// `Object.prototype.toString.call(x)` reads `[object FormData]`.
  #[qjs(prop, rename = PredefinedAtom::SymbolToStringTag, configurable)]
  pub fn to_string_tag() -> &'static str {
    "FormData"
  }

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
  pub fn keys<'js>(ctx: Ctx<'js>, this: This<Class<'js, Self>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, Self::project_key)
  }

  #[qjs(rename = "values")]
  pub fn values<'js>(ctx: Ctx<'js>, this: This<Class<'js, Self>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, Self::project_value)
  }

  #[qjs(rename = "entries")]
  pub fn entries<'js>(ctx: Ctx<'js>, this: This<Class<'js, Self>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, Self::project_entry)
  }

  #[qjs(rename = PredefinedAtom::SymbolIterator)]
  pub fn js_iter<'js>(ctx: Ctx<'js>, this: This<Class<'js, Self>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, Self::project_entry)
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
