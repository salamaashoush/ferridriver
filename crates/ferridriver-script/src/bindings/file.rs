//! WHATWG `File` — a `Blob` with a `name` and a `lastModified`.
//!
//! `new File(parts, name, { type?, lastModified? })`. It carries the full
//! `Blob` surface (`size`/`type`/`text`/`arrayBuffer`/`bytes`/`slice`/
//! `stream`) and its prototype chains to `Blob.prototype`, so
//! `file instanceof Blob` holds — rquickjs classes have no inheritance of
//! their own, so [`install_file_prototype`] wires the chain once at
//! class-definition time.
//!
//! A `File` used as a `FormData` value supplies the multipart `filename`
//! without the caller repeating it (`fd.append('f', file)`).

use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, Object, TypedArray, Value, class::Trace};

use crate::bindings::blob::{BlobJs, blob_parts, normalize_type};

#[derive(Trace)]
#[rquickjs::class(rename = "File")]
pub struct FileJs {
  #[qjs(skip_trace)]
  data: Vec<u8>,
  #[qjs(skip_trace)]
  type_: String,
  #[qjs(skip_trace)]
  name: String,
  #[qjs(skip_trace)]
  last_modified: i64,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for FileJs {
  type Changed<'to> = FileJs;
}

impl FileJs {
  /// Build a `File` from Rust-side parts (a `FormData` file entry read
  /// back out).
  pub fn new_parts(data: Vec<u8>, type_: String, name: String) -> Self {
    Self {
      data,
      type_,
      name,
      last_modified: 0,
    }
  }

  /// Bytes, mime and filename of a JS value if it is a `File`.
  pub fn from_js_file(v: &Value<'_>) -> Option<(Vec<u8>, String, String)> {
    Class::<FileJs>::from_value(v).ok().map(|f| {
      let f = f.borrow();
      (f.data.clone(), f.type_.clone(), f.name.clone())
    })
  }
}

/// Chain `File.prototype` onto `Blob.prototype` so `instanceof Blob` is
/// true for a `File`, matching the spec's interface inheritance.
pub fn install_file_prototype(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let (Some(file_proto), Some(blob_proto)) = (Class::<FileJs>::prototype(ctx)?, Class::<BlobJs>::prototype(ctx)?)
  else {
    return Ok(());
  };
  file_proto.set_prototype(Some(&blob_proto))
}

#[rquickjs::methods(rename_all = "camelCase")]
impl FileJs {
  /// `new File(fileBits, fileName, options?)` — `lastModified` defaults
  /// to now, per spec.
  #[qjs(constructor)]
  pub fn new<'js>(parts: Opt<Value<'js>>, name: Opt<String>, options: Opt<Object<'js>>) -> Self {
    let last_modified = options
      .0
      .as_ref()
      .and_then(|o| o.get::<_, i64>("lastModified").ok())
      .unwrap_or_else(|| {
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
      });
    Self {
      data: blob_parts(parts.0.as_ref()),
      type_: normalize_type(options.0.as_ref()),
      name: name.0.unwrap_or_default(),
      last_modified,
    }
  }

  #[qjs(get, rename = "name")]
  pub fn name(&self) -> String {
    self.name.clone()
  }

  #[qjs(get, rename = "lastModified")]
  pub fn last_modified(&self) -> i64 {
    self.last_modified
  }

  #[qjs(get, rename = "size")]
  pub fn size(&self) -> usize {
    self.data.len()
  }

  #[qjs(get, rename = "type")]
  pub fn type_(&self) -> String {
    self.type_.clone()
  }

  #[qjs(rename = "text")]
  pub fn text(&self) -> String {
    String::from_utf8_lossy(&self.data).into_owned()
  }

  #[qjs(rename = "arrayBuffer")]
  pub fn array_buffer<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    rquickjs::ArrayBuffer::new(ctx, self.data.clone()).map(rquickjs::ArrayBuffer::into_value)
  }

  #[qjs(rename = "bytes")]
  pub fn bytes<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    Ok(TypedArray::<u8>::new(ctx, self.data.clone())?.into_value())
  }

  /// Spec: slicing a `File` yields a plain `Blob` (the name is dropped).
  #[qjs(rename = "slice")]
  pub fn slice(&self, start: Opt<i64>, end: Opt<i64>, content_type: Opt<String>) -> BlobJs {
    BlobJs::slice_bytes(&self.data, start.0, end.0, content_type.0)
  }

  #[qjs(rename = "stream")]
  pub fn stream<'js>(
    &self,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Class<'js, ferridriver_jsstd::stream_web::ReadableStream<'js>>> {
    crate::bindings::streams::from_bytes(&ctx, self.data.clone())
  }
}
