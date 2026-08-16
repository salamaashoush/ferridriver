//! `node:buffer` — the `Buffer` value class.
//!
//! A byte-array class with `from` (string with utf8/base64/hex, array,
//! `ArrayBuffer`/view, `Buffer`), `alloc`, `concat`, `isBuffer`,
//! `byteLength`, instance `toString(utf8|base64|hex)`, `slice`, `equals`,
//! `length`, `toJSON` and `toUint8Array`.
//!
//! It is NOT a `Uint8Array` subclass and has no index accessors — call
//! `toUint8Array()` for byte-level access. Unsupported encodings throw a
//! `TypeError`-named `Error`.

use base64::Engine as _;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Object, Value, class::Trace};

use super::bytes::value_to_bytes;
use super::throw_named;

// ── Buffer ──────────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Buffer")]
pub struct BufferJs {
  #[qjs(skip_trace)]
  bytes: Vec<u8>,
}

impl BufferJs {
  #[must_use]
  pub fn bytes(&self) -> &[u8] {
    &self.bytes
  }
}
#[rquickjs::methods]
impl BufferJs {
  /// `new Buffer(value, encoding?)` — same lowering as `Buffer.from`
  /// (Node deprecates the constructor but it must exist for the
  /// statics to hang off, and legacy code still calls it).
  #[qjs(constructor)]
  pub fn new<'js>(ctx: Ctx<'js>, value: Value<'js>, encoding: Opt<String>) -> rquickjs::Result<BufferJs> {
    Ok(BufferJs {
      bytes: value_to_bytes(&ctx, &value, encoding.0.as_deref())?,
    })
  }

  /// `Buffer.from(string | Array | ArrayBuffer | view | Buffer, encoding?)`.
  #[qjs(static)]
  pub fn from<'js>(ctx: Ctx<'js>, value: Value<'js>, encoding: Opt<String>) -> rquickjs::Result<BufferJs> {
    Ok(BufferJs {
      bytes: value_to_bytes(&ctx, &value, encoding.0.as_deref())?,
    })
  }

  #[qjs(static)]
  pub fn alloc(size: usize) -> BufferJs {
    BufferJs { bytes: vec![0; size] }
  }

  #[qjs(static, rename = "isBuffer")]
  pub fn is_buffer(value: Value<'_>) -> bool {
    value.as_object().is_some_and(|o| o.as_class::<BufferJs>().is_some())
  }

  #[qjs(static)]
  pub fn concat<'js>(ctx: Ctx<'js>, list: Vec<Value<'js>>) -> rquickjs::Result<BufferJs> {
    let mut bytes = Vec::new();
    for item in &list {
      bytes.extend_from_slice(&value_to_bytes(&ctx, item, None)?);
    }
    Ok(BufferJs { bytes })
  }

  #[qjs(static, rename = "byteLength")]
  pub fn byte_length<'js>(ctx: Ctx<'js>, value: Value<'js>, encoding: Opt<String>) -> rquickjs::Result<usize> {
    Ok(value_to_bytes(&ctx, &value, encoding.0.as_deref())?.len())
  }

  #[qjs(get)]
  pub fn length(&self) -> usize {
    self.bytes.len()
  }

  /// `toString(encoding = 'utf8')`.
  #[qjs(rename = "toString")]
  pub fn to_string_js(&self, ctx: Ctx<'_>, encoding: Opt<String>) -> rquickjs::Result<String> {
    match encoding.0.as_deref().unwrap_or("utf8") {
      "utf8" | "utf-8" => Ok(String::from_utf8_lossy(&self.bytes).into_owned()),
      "base64" => Ok(base64::engine::general_purpose::STANDARD.encode(&self.bytes)),
      "hex" => Ok(
        self
          .bytes
          .iter()
          .fold(String::with_capacity(self.bytes.len() * 2), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
          }),
      ),
      other => Err(throw_named(
        &ctx,
        "TypeError",
        format!("unsupported Buffer encoding {other:?} (utf8 | base64 | hex)"),
      )),
    }
  }

  pub fn slice(&self, start: Opt<i64>, end: Opt<i64>) -> BufferJs {
    let len = i64::try_from(self.bytes.len()).unwrap_or(i64::MAX);
    let clamp = |v: i64| -> usize {
      let v = if v < 0 { len + v } else { v };
      usize::try_from(v.clamp(0, len)).unwrap_or(0)
    };
    let s = clamp(start.0.unwrap_or(0));
    let e = clamp(end.0.unwrap_or(len));
    BufferJs {
      bytes: self.bytes.get(s..e.max(s)).unwrap_or(&[]).to_vec(),
    }
  }

  pub fn equals(&self, other: rquickjs::Class<'_, BufferJs>) -> bool {
    self.bytes == other.borrow().bytes
  }

  /// Escape hatch for byte-level access (`Buffer` here is not a
  /// `Uint8Array` subclass).
  #[qjs(rename = "toUint8Array")]
  pub fn to_uint8_array<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::TypedArray<'js, u8>> {
    rquickjs::TypedArray::new(ctx, self.bytes.clone())
  }

  #[qjs(rename = "toJSON")]
  pub fn to_json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let o = Object::new(ctx.clone())?;
    o.set("type", "Buffer")?;
    o.set("data", self.bytes.clone())?;
    Ok(o)
  }
}

/// The `Buffer` constructor (statics included), for the module exports.
pub fn buffer_constructor<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
  rquickjs::Class::<BufferJs>::define(&ctx.globals())?;
  let ctor = rquickjs::Class::<BufferJs>::create_constructor(ctx)?
    .ok_or_else(|| throw_named(ctx, "Error", "Buffer constructor unavailable"))?;
  Ok(ctor.into_value())
}
