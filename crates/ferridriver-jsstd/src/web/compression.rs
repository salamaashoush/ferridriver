//! WHATWG `CompressionStream` / `DecompressionStream`.
//!
//! Both are "generic transform streams": not `TransformStream`
//! subclasses, but objects exposing the `readable` / `writable` pair of
//! one. This builds a real `TransformStream` (the vendored class in
//! [`crate::stream_web`]) whose `transform` and `flush` are
//! native functions, so backpressure, cancellation and `pipeThrough`
//! all come from the spec-exact stream machinery rather than being
//! reimplemented here.
//!
//! The spec defines exactly three formats — `gzip`, `deflate` (zlib
//! wrapper) and `deflate-raw` — all of which `flate2` covers. Brotli and
//! zstd are deliberately absent: they are not in the Compression Streams
//! spec, and accepting them would be a silent extension callers could
//! not rely on elsewhere.
//!
//! The native closures capture only an `Arc<Mutex<..>>` of Rust state,
//! never a JS value, per the GC-cycle discipline.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use rquickjs::function::Func;
use rquickjs::{Class, Ctx, Function, Object, TypedArray, Value, class::Trace};

/// The streaming coder behind one `CompressionStream` /
/// `DecompressionStream`. Each variant writes into a `Vec<u8>` that is
/// drained after every chunk, so nothing accumulates beyond one
/// transform step.
enum Coder {
  GzipEncode(flate2::write::GzEncoder<Vec<u8>>),
  DeflateEncode(flate2::write::ZlibEncoder<Vec<u8>>),
  DeflateRawEncode(flate2::write::DeflateEncoder<Vec<u8>>),
  GzipDecode(flate2::write::GzDecoder<Vec<u8>>),
  DeflateDecode(flate2::write::ZlibDecoder<Vec<u8>>),
  DeflateRawDecode(flate2::write::DeflateDecoder<Vec<u8>>),
}

impl Coder {
  fn new(format: &str, decompress: bool) -> Option<Self> {
    let level = flate2::Compression::default();
    Some(match (format, decompress) {
      ("gzip", false) => Self::GzipEncode(flate2::write::GzEncoder::new(Vec::new(), level)),
      ("deflate", false) => Self::DeflateEncode(flate2::write::ZlibEncoder::new(Vec::new(), level)),
      ("deflate-raw", false) => Self::DeflateRawEncode(flate2::write::DeflateEncoder::new(Vec::new(), level)),
      ("gzip", true) => Self::GzipDecode(flate2::write::GzDecoder::new(Vec::new())),
      ("deflate", true) => Self::DeflateDecode(flate2::write::ZlibDecoder::new(Vec::new())),
      ("deflate-raw", true) => Self::DeflateRawDecode(flate2::write::DeflateDecoder::new(Vec::new())),
      _ => return None,
    })
  }

  /// Feed input and return whatever output became available. Returning
  /// an empty vec is normal — a coder may buffer internally until it has
  /// a full block.
  fn push(&mut self, data: &[u8]) -> std::io::Result<Vec<u8>> {
    match self {
      Self::GzipEncode(c) => {
        c.write_all(data)?;
        Ok(std::mem::take(c.get_mut()))
      },
      Self::DeflateEncode(c) => {
        c.write_all(data)?;
        Ok(std::mem::take(c.get_mut()))
      },
      Self::DeflateRawEncode(c) => {
        c.write_all(data)?;
        Ok(std::mem::take(c.get_mut()))
      },
      Self::GzipDecode(c) => {
        c.write_all(data)?;
        Ok(std::mem::take(c.get_mut()))
      },
      Self::DeflateDecode(c) => {
        c.write_all(data)?;
        Ok(std::mem::take(c.get_mut()))
      },
      Self::DeflateRawDecode(c) => {
        c.write_all(data)?;
        Ok(std::mem::take(c.get_mut()))
      },
    }
  }

  /// End the stream and return the trailing output (gzip's CRC/length
  /// trailer, the deflate final block, …).
  fn finish(self) -> std::io::Result<Vec<u8>> {
    match self {
      Self::GzipEncode(c) => c.finish(),
      Self::DeflateEncode(c) => c.finish(),
      Self::DeflateRawEncode(c) => c.finish(),
      Self::GzipDecode(c) => c.finish(),
      Self::DeflateDecode(c) => c.finish(),
      Self::DeflateRawDecode(c) => c.finish(),
    }
  }
}

/// `Some` until `flush` consumes it; `None` after, so a late write
/// cannot resurrect a finished coder.
type SharedCoder = Arc<Mutex<Option<Coder>>>;

fn lock(coder: &SharedCoder) -> std::sync::MutexGuard<'_, Option<Coder>> {
  coder.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Bytes of a `BufferSource` chunk. Per spec anything else is a
/// `TypeError` — a string is NOT encoded implicitly, because the caller
/// would silently get UTF-8 where they may have meant something else.
fn buffer_source_bytes<'js>(ctx: &Ctx<'js>, chunk: &Value<'js>) -> rquickjs::Result<Vec<u8>> {
  // The shared extractor, with this call site's spec wording on failure.
  crate::node::bytes::buffer_source_bytes(ctx, chunk).map_err(|_| {
    rquickjs::Exception::throw_type(
      ctx,
      "Failed to execute 'write': chunk could not be converted to a BufferSource",
    )
  })
}

/// Hand `bytes` to the transform controller, skipping an empty step (the
/// spec enqueues only when the coder actually produced output).
fn enqueue(controller: &Object<'_>, bytes: Vec<u8>) -> rquickjs::Result<()> {
  if bytes.is_empty() {
    return Ok(());
  }
  let chunk = TypedArray::new(controller.ctx().clone(), bytes)?.into_value();
  controller
    .get::<_, Function<'_>>("enqueue")?
    .call::<_, ()>((rquickjs::function::This(controller.clone()), chunk))
}

fn io_error(ctx: &Ctx<'_>, what: &str, e: &std::io::Error) -> rquickjs::Error {
  rquickjs::Exception::throw_type(ctx, &format!("{what}: {e}"))
}

/// Build the `TransformStream` that backs a generic transform stream,
/// with native `transform` / `flush` driving `coder`.
fn transform_stream<'js>(ctx: &Ctx<'js>, coder: SharedCoder) -> rquickjs::Result<Object<'js>> {
  let transformer = Object::new(ctx.clone())?;
  {
    let coder = coder.clone();
    transformer.set(
      "transform",
      Func::from(
        move |ctx: Ctx<'js>, chunk: Value<'js>, controller: Object<'js>| -> rquickjs::Result<()> {
          let bytes = buffer_source_bytes(&ctx, &chunk)?;
          let out = {
            let mut guard = lock(&coder);
            let Some(coder) = guard.as_mut() else {
              return Err(rquickjs::Exception::throw_type(&ctx, "the stream is already closed"));
            };
            coder
              .push(&bytes)
              .map_err(|e| io_error(&ctx, "compression failed", &e))?
          };
          enqueue(&controller, out)
        },
      ),
    )?;
  }
  {
    let coder = coder.clone();
    transformer.set(
      "flush",
      Func::from(move |ctx: Ctx<'js>, controller: Object<'js>| -> rquickjs::Result<()> {
        let out = match lock(&coder).take() {
          None => return Ok(()),
          Some(coder) => coder
            .finish()
            .map_err(|e| io_error(&ctx, "compression failed at end of stream", &e))?,
        };
        enqueue(&controller, out)
      }),
    )?;
  }

  ctx
    .globals()
    .get::<_, rquickjs::function::Constructor<'js>>("TransformStream")?
    .construct((transformer,))
}

/// The `readable` / `writable` pair every generic transform stream
/// exposes. Both classes below are this plus a constructor.
#[derive(Trace)]
struct Duplex<'js> {
  readable: Value<'js>,
  writable: Value<'js>,
}

impl<'js> Duplex<'js> {
  fn new(ctx: &Ctx<'js>, format: &str, decompress: bool) -> rquickjs::Result<Self> {
    let what = if decompress {
      "DecompressionStream"
    } else {
      "CompressionStream"
    };
    let Some(coder) = Coder::new(format, decompress) else {
      return Err(rquickjs::Exception::throw_type(
        ctx,
        &format!(
          "Failed to construct '{what}': '{format}' is not a valid enum value of type CompressionFormat \
           (expected 'gzip', 'deflate' or 'deflate-raw')"
        ),
      ));
    };
    let stream = transform_stream(ctx, Arc::new(Mutex::new(Some(coder))))?;
    Ok(Self {
      readable: stream.get("readable")?,
      writable: stream.get("writable")?,
    })
  }
}

/// WHATWG `CompressionStream`.
#[derive(Trace)]
#[rquickjs::class(rename = "CompressionStream")]
pub struct CompressionStreamJs<'js> {
  inner: Duplex<'js>,
}

/// WHATWG `DecompressionStream`.
#[derive(Trace)]
#[rquickjs::class(rename = "DecompressionStream")]
pub struct DecompressionStreamJs<'js> {
  inner: Duplex<'js>,
}

#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for CompressionStreamJs<'js> {
  type Changed<'to> = CompressionStreamJs<'to>;
}
#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for DecompressionStreamJs<'js> {
  type Changed<'to> = DecompressionStreamJs<'to>;
}

#[rquickjs::methods]
impl<'js> CompressionStreamJs<'js> {
  /// Spec: every platform object carries `Symbol.toStringTag`, so
  /// `Object.prototype.toString.call(x)` reads `[object CompressionStream]`.
  #[qjs(prop, rename = rquickjs::atom::PredefinedAtom::SymbolToStringTag, configurable)]
  pub fn to_string_tag() -> &'static str {
    "CompressionStream"
  }

  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'js>, format: String) -> rquickjs::Result<Self> {
    Ok(Self {
      inner: Duplex::new(&ctx, &format, false)?,
    })
  }

  #[qjs(get, rename = "readable")]
  pub fn readable(&self) -> Value<'js> {
    self.inner.readable.clone()
  }

  #[qjs(get, rename = "writable")]
  pub fn writable(&self) -> Value<'js> {
    self.inner.writable.clone()
  }
}

#[rquickjs::methods]
impl<'js> DecompressionStreamJs<'js> {
  /// Spec: every platform object carries `Symbol.toStringTag`, so
  /// `Object.prototype.toString.call(x)` reads `[object DecompressionStream]`.
  #[qjs(prop, rename = rquickjs::atom::PredefinedAtom::SymbolToStringTag, configurable)]
  pub fn to_string_tag() -> &'static str {
    "DecompressionStream"
  }

  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'js>, format: String) -> rquickjs::Result<Self> {
    Ok(Self {
      inner: Duplex::new(&ctx, &format, true)?,
    })
  }

  #[qjs(get, rename = "readable")]
  pub fn readable(&self) -> Value<'js> {
    self.inner.readable.clone()
  }

  #[qjs(get, rename = "writable")]
  pub fn writable(&self) -> Value<'js> {
    self.inner.writable.clone()
  }
}

pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();
  Class::<CompressionStreamJs<'_>>::define(&globals)?;
  Class::<DecompressionStreamJs<'_>>::define(&globals)?;
  Ok(())
}
