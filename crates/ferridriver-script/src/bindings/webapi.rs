//! Native web-platform globals: `TextEncoder` / `TextDecoder` / `URL`
//! plus `queueMicrotask` / `btoa` / `atob`.
//!
//! These are real `#[rquickjs::class]` bindings (Rust is the source of
//! truth), not JS shims dispatching to hidden `__ferri*` helpers.
//! `URL` is backed by the `url` crate; `URLSearchParams` comes from
//! `rquickjs-extra-url` (installed separately) and is instantiated here
//! via its registered global constructor — no string `eval`.

use base64::Engine as _;
use rquickjs::function::{Constructor, Func, Opt};
use rquickjs::{Class, Ctx, Function, JsLifetime, TypedArray, Value, class::Trace};

/// TextEncoder — UTF-8 only, matching the WHATWG default.
#[derive(Trace, JsLifetime, Default)]
#[rquickjs::class(rename = "TextEncoder")]
pub struct TextEncoder {}

#[rquickjs::methods]
impl TextEncoder {
  #[qjs(constructor)]
  pub fn new() -> Self {
    Self {}
  }

  #[qjs(get, rename = "encoding")]
  pub fn encoding(&self) -> &'static str {
    "utf-8"
  }

  pub fn encode<'js>(&self, ctx: Ctx<'js>, input: Opt<String>) -> rquickjs::Result<TypedArray<'js, u8>> {
    TypedArray::new(ctx, input.0.unwrap_or_default().into_bytes())
  }
}

/// TextDecoder — UTF-8, lossy (matches `fatal: false`, the default).
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "TextDecoder")]
pub struct TextDecoder {
  encoding: String,
}

#[rquickjs::methods]
impl TextDecoder {
  #[qjs(constructor)]
  pub fn new(label: Opt<String>) -> Self {
    // We only implement utf-8; report it back regardless of label
    // rather than pretending to honour an unsupported encoding.
    let _ = label;
    Self {
      encoding: "utf-8".to_string(),
    }
  }

  #[qjs(get, rename = "encoding")]
  pub fn encoding(&self) -> String {
    self.encoding.clone()
  }

  pub fn decode(&self, input: Opt<Value<'_>>) -> rquickjs::Result<String> {
    let Some(v) = input.0 else {
      return Ok(String::new());
    };
    let bytes = value_to_bytes(&v)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
  }
}

/// Extract a byte buffer from a `Uint8Array`/`TypedArray`, an
/// `ArrayBuffer`, or an array-like of numbers.
fn value_to_bytes(v: &Value<'_>) -> rquickjs::Result<Vec<u8>> {
  if let Ok(ta) = TypedArray::<u8>::from_value(v.clone())
    && let Some(b) = ta.as_bytes()
  {
    return Ok(b.to_vec());
  }
  if let Some(obj) = v.as_object()
    && let Some(buf) = rquickjs::ArrayBuffer::from_object(obj.clone())
    && let Some(b) = buf.as_bytes()
  {
    return Ok(b.to_vec());
  }
  if let Some(arr) = v.as_array() {
    let mut out = Vec::with_capacity(arr.len());
    for item in arr.iter::<u8>() {
      out.push(item?);
    }
    return Ok(out);
  }
  Ok(Vec::new())
}

/// WHATWG `URL`, backed by the `url` crate.
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "URL")]
pub struct Url {
  #[qjs(skip_trace)]
  inner: url::Url,
}

#[rquickjs::methods]
impl Url {
  #[qjs(constructor)]
  pub fn new(url: String, base: Opt<String>) -> rquickjs::Result<Self> {
    let parsed = match base.0 {
      Some(b) => url::Url::parse(&b)
        .and_then(|base| base.join(&url))
        .map_err(|e| rquickjs::Error::new_from_js_message("URL", "TypeError", e.to_string()))?,
      None => {
        url::Url::parse(&url).map_err(|e| rquickjs::Error::new_from_js_message("URL", "TypeError", e.to_string()))?
      },
    };
    Ok(Self { inner: parsed })
  }

  #[qjs(get, rename = "href")]
  pub fn href(&self) -> String {
    self.inner.as_str().to_string()
  }

  #[qjs(get, rename = "origin")]
  pub fn origin(&self) -> String {
    self.inner.origin().ascii_serialization()
  }

  #[qjs(get, rename = "protocol")]
  pub fn protocol(&self) -> String {
    format!("{}:", self.inner.scheme())
  }

  #[qjs(get, rename = "hostname")]
  pub fn hostname(&self) -> String {
    self.inner.host_str().unwrap_or("").to_string()
  }

  #[qjs(get, rename = "port")]
  pub fn port(&self) -> String {
    self.inner.port().map(|p| p.to_string()).unwrap_or_default()
  }

  #[qjs(get, rename = "host")]
  pub fn host(&self) -> String {
    match (self.inner.host_str(), self.inner.port()) {
      (Some(h), Some(p)) => format!("{h}:{p}"),
      (Some(h), None) => h.to_string(),
      (None, _) => String::new(),
    }
  }

  #[qjs(get, rename = "pathname")]
  pub fn pathname(&self) -> String {
    self.inner.path().to_string()
  }

  #[qjs(get, rename = "search")]
  pub fn search(&self) -> String {
    match self.inner.query() {
      Some(q) if !q.is_empty() => format!("?{q}"),
      _ => String::new(),
    }
  }

  #[qjs(get, rename = "hash")]
  pub fn hash(&self) -> String {
    match self.inner.fragment() {
      Some(f) if !f.is_empty() => format!("#{f}"),
      _ => String::new(),
    }
  }

  /// Live-ish `URLSearchParams` over this URL's query, built through the
  /// `URLSearchParams` global constructor (from `rquickjs-extra-url`).
  #[qjs(get, rename = "searchParams")]
  pub fn search_params<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let query = self.inner.query().unwrap_or("");
    let ctor: Constructor<'js> = ctx.globals().get("URLSearchParams")?;
    ctor.construct((query.to_string(),))
  }

  #[qjs(rename = "toString")]
  pub fn to_js_string(&self) -> String {
    self.inner.as_str().to_string()
  }

  #[qjs(rename = "toJSON")]
  pub fn to_json(&self) -> String {
    self.inner.as_str().to_string()
  }
}

/// Install the native web-API classes + globals. Called once at
/// `Session::create`; persists across executions like the rest of the
/// browser-like runtime surface.
pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  Class::<TextEncoder>::define(&globals)?;
  Class::<TextDecoder>::define(&globals)?;
  Class::<Url>::define(&globals)?;

  // queueMicrotask: defer the callback onto the job queue (same
  // primitive rquickjs-extra-timers' setImmediate uses).
  globals.set(
    "queueMicrotask",
    Func::from(|cb: Function<'_>| -> rquickjs::Result<()> {
      cb.defer::<()>(())?;
      Ok(())
    }),
  )?;

  // btoa/atob over a Latin1 "binary string", per the WHATWG contract.
  globals.set(
    "btoa",
    Func::from(|s: String| -> rquickjs::Result<String> {
      let mut bytes = Vec::with_capacity(s.len());
      for ch in s.chars() {
        let c = ch as u32;
        if c > 0xFF {
          return Err(rquickjs::Error::new_from_js_message(
            "btoa",
            "InvalidCharacterError",
            "string contains characters outside the Latin1 range".to_string(),
          ));
        }
        bytes.push(c as u8);
      }
      Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }),
  )?;
  globals.set(
    "atob",
    Func::from(|s: String| -> rquickjs::Result<String> {
      let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| rquickjs::Error::new_from_js_message("atob", "InvalidCharacterError", e.to_string()))?;
      Ok(bytes.into_iter().map(|b| b as char).collect())
    }),
  )?;

  Ok(())
}
