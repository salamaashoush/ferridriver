//! Node-compat primitives behind the native `path` / `buffer` modules
//! (see `native_modules.rs`). Documented subsets, not full Node parity:
//!
//! - `path`: POSIX-style pure string ops (`join`, `resolve`, `dirname`,
//!   `basename`, `extname`, `normalize`, `relative`, `isAbsolute`,
//!   `sep`, `delimiter`). `resolve` roots at `process.cwd()` (the
//!   sandbox root) when no absolute segment is given. No win32 flavor.
//! - `Buffer`: byte-array value class with `from` (string with
//!   utf8/base64/hex encodings, array, ArrayBuffer/view, Buffer),
//!   `alloc`, `concat`, `isBuffer`, `byteLength`, instance
//!   `toString(utf8|base64|hex)`, `slice`, `equals`, `length`,
//!   `toUint8Array`. It is NOT a `Uint8Array` subclass and has no index
//!   accessors — call `toUint8Array()` for byte-level access.
//!   Unsupported encodings throw a `TypeError`-named `Error`.

use base64::Engine as _;
use rquickjs::function::{Func, Opt, Rest};
use rquickjs::{Ctx, JsLifetime, Object, Value, class::Trace};

use crate::bindings::convert::throw_named;

// ── path ────────────────────────────────────────────────────────────────

fn normalize_str(path: &str) -> String {
  let absolute = path.starts_with('/');
  let mut out: Vec<&str> = Vec::new();
  for seg in path.split('/') {
    match seg {
      "" | "." => {},
      ".." => {
        if matches!(out.last(), Some(&"..")) || (out.is_empty() && !absolute) {
          out.push("..");
        } else {
          out.pop();
        }
      },
      s => out.push(s),
    }
  }
  let joined = out.join("/");
  let trailing = path.len() > 1 && path.ends_with('/') && !joined.is_empty();
  match (absolute, joined.is_empty()) {
    (true, true) => "/".to_string(),
    (true, false) => format!("/{joined}{}", if trailing { "/" } else { "" }),
    (false, true) => ".".to_string(),
    (false, false) => format!("{joined}{}", if trailing { "/" } else { "" }),
  }
}

fn join_segments(segments: &[String]) -> String {
  let parts: Vec<&str> = segments.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
  if parts.is_empty() {
    return ".".to_string();
  }
  normalize_str(&parts.join("/"))
}

fn dirname_str(path: &str) -> String {
  let trimmed = path.trim_end_matches('/');
  match trimmed.rfind('/') {
    Some(0) => "/".to_string(),
    Some(i) => trimmed[..i].to_string(),
    None => {
      if path.starts_with('/') {
        "/".to_string()
      } else {
        ".".to_string()
      }
    },
  }
}

fn basename_str(path: &str, ext: Option<&str>) -> String {
  let trimmed = path.trim_end_matches('/');
  let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
  match ext {
    Some(e) if base.len() > e.len() && base.ends_with(e) => base[..base.len() - e.len()].to_string(),
    _ => base.to_string(),
  }
}

fn extname_str(path: &str) -> String {
  let base = basename_str(path, None);
  match base.rfind('.') {
    // A leading dot (`.gitignore`) is not an extension.
    Some(i) if i > 0 => base[i..].to_string(),
    _ => String::new(),
  }
}

fn resolve_segments(cwd: &str, segments: &[String]) -> String {
  let mut acc = cwd.to_string();
  for seg in segments {
    if seg.is_empty() {
      continue;
    }
    if seg.starts_with('/') {
      acc.clone_from(seg);
    } else {
      acc = format!("{acc}/{seg}");
    }
  }
  let n = normalize_str(&acc);
  // `resolve` never returns a trailing slash (except root).
  if n.len() > 1 {
    n.trim_end_matches('/').to_string()
  } else {
    n
  }
}

fn relative_str(from: &str, to: &str) -> String {
  let f = normalize_str(from);
  let t = normalize_str(to);
  let fp: Vec<&str> = f.split('/').filter(|s| !s.is_empty()).collect();
  let tp: Vec<&str> = t.split('/').filter(|s| !s.is_empty()).collect();
  let common = fp.iter().zip(tp.iter()).take_while(|(a, b)| a == b).count();
  let mut out: Vec<&str> = vec![".."; fp.len() - common];
  out.extend(&tp[common..]);
  out.join("/")
}

/// The current working directory the JS surface reports: the sandbox
/// root via the `process` shim, falling back to `/`.
fn js_cwd(ctx: &Ctx<'_>) -> String {
  let cwd: rquickjs::Result<String> = (|| {
    let process: Object<'_> = ctx.globals().get("process")?;
    let cwd_fn: rquickjs::Function<'_> = process.get("cwd")?;
    cwd_fn.call(())
  })();
  cwd.unwrap_or_else(|_| "/".to_string())
}

/// Build the `path` module object (fresh per call; only built once per
/// session by the module loader).
pub fn path_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let o = Object::new(ctx.clone())?;
  o.set("sep", "/")?;
  o.set("delimiter", ":")?;
  o.set("join", Func::from(|segs: Rest<String>| join_segments(&segs.0)))?;
  o.set(
    "resolve",
    Func::from(|ctx: Ctx<'_>, segs: Rest<String>| -> String { resolve_segments(&js_cwd(&ctx), &segs.0) }),
  )?;
  o.set("normalize", Func::from(|p: String| normalize_str(&p)))?;
  o.set("dirname", Func::from(|p: String| dirname_str(&p)))?;
  o.set(
    "basename",
    Func::from(|p: String, ext: Opt<String>| basename_str(&p, ext.0.as_deref())),
  )?;
  o.set("extname", Func::from(|p: String| extname_str(&p)))?;
  o.set(
    "relative",
    Func::from(|ctx: Ctx<'_>, from: String, to: String| -> String {
      let cwd = js_cwd(&ctx);
      relative_str(&resolve_segments(&cwd, &[from]), &resolve_segments(&cwd, &[to]))
    }),
  )?;
  o.set("isAbsolute", Func::from(|p: String| p.starts_with('/')))?;
  Ok(o)
}

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

fn value_to_bytes<'js>(ctx: &Ctx<'js>, value: &Value<'js>, encoding: Option<&str>) -> rquickjs::Result<Vec<u8>> {
  if let Some(s) = value.as_string() {
    return decode(ctx, &s.to_string()?, encoding.unwrap_or("utf8"));
  }
  if let Some(obj) = value.as_object() {
    if let Some(buf) = obj.as_class::<BufferJs>() {
      return Ok(buf.borrow().bytes.clone());
    }
    if let Some(arr) = obj.as_array() {
      let mut out = Vec::with_capacity(arr.len());
      for i in 0..arr.len() {
        out.push(arr.get::<u8>(i)?);
      }
      return Ok(out);
    }
  }
  crate::bindings::crypto::buffer_source_bytes(ctx, value)
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
