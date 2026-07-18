//! WHATWG `Blob` (spec subset, no deps; semantics studied from the
//! read-only llrt reference). `new Blob(parts?, { type? })` where parts
//! is an iterable of string / `Blob` / `Uint8Array` / `ArrayBuffer`;
//! `size`, `type`, `text()`, `arrayBuffer()`, `bytes()`, `slice()`,
//! `stream()` (a `ReadableStream`). Body readers are returned
//! synchronously (await-transparent), consistent with the rest of the
//! fetch subset.

use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, Object, TypedArray, Value, class::Trace};

#[derive(Trace)]
#[rquickjs::class(rename = "Blob")]
pub struct BlobJs {
  #[qjs(skip_trace)]
  data: Vec<u8>,
  #[qjs(skip_trace)]
  type_: String,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for BlobJs {
  type Changed<'to> = BlobJs;
}

impl BlobJs {
  pub fn new_parts(data: Vec<u8>, type_: String) -> Self {
    Self { data, type_ }
  }

  pub fn bytes_ref(&self) -> &[u8] {
    &self.data
  }

  pub fn mime(&self) -> &str {
    &self.type_
  }

  /// Bytes + mime of a JS value if it is a `Blob` — or a `File`, which
  /// IS a `Blob` per spec but is a distinct rquickjs class.
  pub fn from_js_blob(v: &Value<'_>) -> Option<(Vec<u8>, String)> {
    if let Ok(b) = Class::<BlobJs>::from_value(v) {
      let b = b.borrow();
      return Some((b.data.clone(), b.type_.clone()));
    }
    crate::bindings::file::FileJs::from_js_file(v).map(|(bytes, ct, _)| (bytes, ct))
  }

  /// The shared `slice()` body: a byte range with spec index
  /// normalization (negative counts from the end). `File.slice()` yields
  /// a plain `Blob`, so both classes route here.
  pub fn slice_bytes(data: &[u8], start: Option<i64>, end: Option<i64>, content_type: Option<String>) -> BlobJs {
    let len = i64::try_from(data.len()).unwrap_or(i64::MAX);
    let norm = |v: i64| if v < 0 { (len + v).max(0) } else { v.min(len) };
    let s = norm(start.unwrap_or(0)) as usize;
    let e = norm(end.unwrap_or(len)) as usize;
    BlobJs {
      data: if s < e { data[s..e].to_vec() } else { Vec::new() },
      type_: content_type.map(|t| t.to_ascii_lowercase()).unwrap_or_default(),
    }
  }
}

/// Flatten a `BlobPart` sequence into bytes. Shared with `File`.
pub fn blob_parts(parts: Option<&Value<'_>>) -> Vec<u8> {
  let mut data = Vec::new();
  if let Some(arr) = parts.and_then(rquickjs::Value::as_array) {
    for i in 0..arr.len() {
      if let Ok(elem) = arr.get::<Value<'_>>(i) {
        push_part(&mut data, &elem);
      }
    }
  }
  data
}

/// The `type` of a `Blob`/`File` options bag: lowercased, and dropped
/// entirely when it is not ASCII-printable, per spec. Shared with `File`.
pub fn normalize_type(options: Option<&Object<'_>>) -> String {
  options
    .and_then(|o| o.get::<_, String>("type").ok())
    .map(|t| t.to_ascii_lowercase())
    .filter(|t| t.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c)))
    .unwrap_or_default()
}

/// Concatenate one `BlobPart` (string -> UTF-8, `Blob`/`Uint8Array`/
/// `ArrayBuffer` -> raw bytes; anything else ignored) into `out`.
fn push_part(out: &mut Vec<u8>, elem: &Value<'_>) {
  if let Some(s) = elem.as_string().and_then(|s| s.to_string().ok()) {
    out.extend_from_slice(s.as_bytes());
    return;
  }
  if let Some((bytes, _)) = BlobJs::from_js_blob(elem) {
    out.extend_from_slice(&bytes);
    return;
  }
  if let Ok(ta) = TypedArray::<u8>::from_value(elem.clone()) {
    let b: &[u8] = ta.as_ref();
    out.extend_from_slice(b);
    return;
  }
  if let Some(ab) = rquickjs::ArrayBuffer::from_value(elem.clone())
    && let Some(b) = ab.as_bytes()
  {
    out.extend_from_slice(b);
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl BlobJs {
  #[qjs(constructor)]
  pub fn new<'js>(parts: Opt<Value<'js>>, options: Opt<Object<'js>>) -> Self {
    Self {
      data: blob_parts(parts.0.as_ref()),
      type_: normalize_type(options.0.as_ref()),
    }
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

  /// `slice(start?, end?, contentType?)` — byte range (negative indices
  /// count from the end), per spec.
  #[qjs(rename = "slice")]
  pub fn slice(&self, start: Opt<i64>, end: Opt<i64>, content_type: Opt<String>) -> BlobJs {
    Self::slice_bytes(&self.data, start.0, end.0, content_type.0)
  }

  /// `stream()` -> a `ReadableStream` of the blob bytes.
  #[qjs(rename = "stream")]
  pub fn stream<'js>(
    &self,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Class<'js, ferridriver_jsstd::stream_web::ReadableStream<'js>>> {
    crate::bindings::streams::from_bytes(&ctx, self.data.clone())
  }
}
