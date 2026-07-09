//! Native web-platform globals: `TextEncoder` / `TextDecoder` / `URL`
//! plus `queueMicrotask` / `btoa` / `atob`.
//!
//! These are real `#[rquickjs::class]` bindings (Rust is the source of
//! truth), not JS shims dispatching to hidden `__ferri*` helpers.
//! `URL` is backed by the `url` crate; `URLSearchParams` is the native
//! class in [`crate::bindings::url_search_params`] (installed
//! separately), constructed here directly from the query string.

use base64::Engine as _;
use base64::engine::GeneralPurpose;
use base64::engine::general_purpose::GeneralPurposeConfig;
use rquickjs::function::{Func, Opt};
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

  /// `url.href = ...` reparses; an invalid value is a `TypeError`
  /// (WHATWG: the `href` setter is the one component setter that
  /// throws).
  #[qjs(set, rename = "href")]
  pub fn set_href(&mut self, value: String) -> rquickjs::Result<()> {
    self.inner =
      url::Url::parse(&value).map_err(|e| rquickjs::Error::new_from_js_message("URL", "TypeError", e.to_string()))?;
    Ok(())
  }

  #[qjs(get, rename = "origin")]
  pub fn origin(&self) -> String {
    self.inner.origin().ascii_serialization()
  }

  #[qjs(get, rename = "protocol")]
  pub fn protocol(&self) -> String {
    format!("{}:", self.inner.scheme())
  }

  /// Component setters mirror the WHATWG URL setter steps: an invalid
  /// value is ignored (no throw), matching browser behaviour.
  #[qjs(set, rename = "protocol")]
  pub fn set_protocol(&mut self, value: String) {
    let scheme = value.strip_suffix(':').unwrap_or(&value);
    let _ = self.inner.set_scheme(scheme);
  }

  #[qjs(get, rename = "username")]
  pub fn username(&self) -> String {
    self.inner.username().to_string()
  }

  #[qjs(set, rename = "username")]
  pub fn set_username(&mut self, value: String) {
    let _ = self.inner.set_username(&value);
  }

  #[qjs(get, rename = "password")]
  pub fn password(&self) -> String {
    self.inner.password().unwrap_or("").to_string()
  }

  #[qjs(set, rename = "password")]
  pub fn set_password(&mut self, value: String) {
    let _ = self
      .inner
      .set_password(if value.is_empty() { None } else { Some(&value) });
  }

  #[qjs(get, rename = "hostname")]
  pub fn hostname(&self) -> String {
    self.inner.host_str().unwrap_or("").to_string()
  }

  #[qjs(set, rename = "hostname")]
  pub fn set_hostname(&mut self, value: String) {
    let _ = self.inner.set_host(Some(&value));
  }

  #[qjs(get, rename = "port")]
  pub fn port(&self) -> String {
    self.inner.port().map(|p| p.to_string()).unwrap_or_default()
  }

  #[qjs(set, rename = "port")]
  pub fn set_port(&mut self, value: String) {
    let port = value.trim().parse::<u16>().ok();
    let _ = self.inner.set_port(port);
  }

  #[qjs(get, rename = "host")]
  pub fn host(&self) -> String {
    match (self.inner.host_str(), self.inner.port()) {
      (Some(h), Some(p)) => format!("{h}:{p}"),
      (Some(h), None) => h.to_string(),
      (None, _) => String::new(),
    }
  }

  #[qjs(set, rename = "host")]
  pub fn set_host(&mut self, value: String) {
    if let Some((h, p)) = value.rsplit_once(':') {
      if self.inner.set_host(Some(h)).is_ok() {
        let _ = self.inner.set_port(p.parse::<u16>().ok());
      }
    } else {
      let _ = self.inner.set_host(Some(&value));
    }
  }

  #[qjs(get, rename = "pathname")]
  pub fn pathname(&self) -> String {
    self.inner.path().to_string()
  }

  #[qjs(set, rename = "pathname")]
  pub fn set_pathname(&mut self, value: String) {
    self.inner.set_path(&value);
  }

  #[qjs(get, rename = "search")]
  pub fn search(&self) -> String {
    match self.inner.query() {
      Some(q) if !q.is_empty() => format!("?{q}"),
      _ => String::new(),
    }
  }

  #[qjs(set, rename = "search")]
  pub fn set_search(&mut self, value: String) {
    let q = value.strip_prefix('?').unwrap_or(&value);
    self.inner.set_query(if q.is_empty() { None } else { Some(q) });
  }

  #[qjs(get, rename = "hash")]
  pub fn hash(&self) -> String {
    match self.inner.fragment() {
      Some(f) if !f.is_empty() => format!("#{f}"),
      _ => String::new(),
    }
  }

  #[qjs(set, rename = "hash")]
  pub fn set_hash(&mut self, value: String) {
    let f = value.strip_prefix('#').unwrap_or(&value);
    self.inner.set_fragment(if f.is_empty() { None } else { Some(f) });
  }

  /// Live-ish `URLSearchParams` over this URL's query (a snapshot —
  /// mutations do not write back to the URL).
  #[qjs(get, rename = "searchParams")]
  pub fn search_params<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let params = crate::bindings::url_search_params::UrlSearchParams::from_query(self.inner.query().unwrap_or(""));
    Ok(Class::instance(ctx, params)?.into_value())
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

/// WHATWG "forgiving-base64 decode"
/// (<https://infra.spec.whatwg.org/#forgiving-base64-decode>): strip
/// ALL ASCII whitespace (not just the ends), reject a length ≡ 1 mod 4,
/// tolerate missing/partial `=` padding, and discard non-zero trailing
/// bits. `base64::STANDARD` does none of this (canonical padding only,
/// no whitespace), so a spec-conformant `atob` needs the explicit
/// algorithm here.
fn forgiving_base64_decode(input: &str) -> Result<Vec<u8>, &'static str> {
  let mut s: String = input
    .chars()
    .filter(|c| !matches!(c, '\t' | '\n' | '\u{0C}' | '\r' | ' '))
    .collect();
  // At most two trailing '=' are stripped; any remaining '=' (or one
  // that leaves length ≡ 1 mod 4) is invalid.
  if s.ends_with('=') {
    s.pop();
    if s.ends_with('=') {
      s.pop();
    }
  }
  if s.len() % 4 == 1 || s.contains('=') {
    return Err("invalid base64 length");
  }
  if !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
    return Err("invalid base64 character");
  }
  // No-pad alphabet, padding indifferent (we stripped it), trailing
  // bits discarded — exactly the forgiving contract.
  let engine = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
      .with_encode_padding(false)
      .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
      .with_decode_allow_trailing_bits(true),
  );
  engine.decode(s.as_bytes()).map_err(|_| "invalid base64")
}

/// WHATWG `queueMicrotask(cb)`. A named generic fn so `Ctx`, the
/// callback, and the wrapper share one `'js` (an inline closure would
/// give each its own lifetime).
fn queue_microtask<'js>(ctx: Ctx<'js>, cb: Function<'js>) -> rquickjs::Result<()> {
  match crate::bindings::fetch::active_net(&ctx) {
    None => cb.defer::<()>(()),
    Some(list) => {
      // The wrapper captures only plain data (`net`); the real callback
      // rides the deferred args (a native closure must never capture a
      // JS value — untraceable GC cycle at teardown).
      let net = Some(list);
      let wrapper = Function::new(ctx.clone(), move |args: rquickjs::function::Rest<Value<'_>>| {
        crate::bindings::timers::deferred_call_with_net(net.as_ref(), &args.0)
      })?;
      wrapper.defer((cb,))
    },
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
  // primitive setImmediate uses). Capability follows the registrar:
  // the job queue drains outside a tool handler's net-policy bracket,
  // so a microtask queued by a net-restricted handler must carry that
  // grant with it (same rule as `setTimeout`/`setImmediate`).
  globals.set("queueMicrotask", Func::from(queue_microtask))?;

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
      let bytes = forgiving_base64_decode(&s)
        .map_err(|m| rquickjs::Error::new_from_js_message("atob", "InvalidCharacterError", m.to_string()))?;
      Ok(bytes.into_iter().map(|b| b as char).collect())
    }),
  )?;

  Ok(())
}
