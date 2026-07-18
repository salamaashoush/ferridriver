//! Native web-platform globals: `TextEncoder` / `TextDecoder` / `URL`
//! plus `queueMicrotask` / `btoa` / `atob` / `structuredClone` /
//! `performance`.
//!
//! `TextEncoder` has `encodeInto`; `TextDecoder` honours `fatal`,
//! `ignoreBOM`, `{ stream: true }` and label validation (UTF-8 only —
//! any other label is a `RangeError` rather than a silent misdecode).
//!
//! These are real `#[rquickjs::class]` bindings (Rust is the source of
//! truth), not JS shims dispatching to hidden `__ferri*` helpers.
//! `URL` is backed by the `url` crate; `URLSearchParams` is the native
//! class in [`crate::bindings::url_search_params`] (installed
//! separately), constructed here directly from the query string.

use base64::Engine as _;
use base64::engine::GeneralPurpose;
use base64::engine::general_purpose::GeneralPurposeConfig;
use rquickjs::function::This;
use rquickjs::function::{Func, Opt};
use rquickjs::{Class, Ctx, Function, JsLifetime, Object, TypedArray, Value, class::Trace};

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

  /// `encodeInto(source, destination)` — writes UTF-8 into the caller's
  /// `Uint8Array` and reports `{ read, written }`. Per spec a partial
  /// code point is never written, so `read` counts whole UTF-16 units of
  /// the prefix that fit.
  #[qjs(rename = "encodeInto")]
  pub fn encode_into<'js>(
    &self,
    ctx: Ctx<'js>,
    source: String,
    destination: TypedArray<'js, u8>,
  ) -> rquickjs::Result<Object<'js>> {
    let capacity = destination.len();
    let mut read = 0usize;
    let mut written = 0usize;
    for ch in source.chars() {
      let need = ch.len_utf8();
      if written + need > capacity {
        break;
      }
      written += need;
      read += ch.len_utf16();
    }
    // `TypedArray` derefs to the caller's buffer, so this writes through
    // to the JS-visible array rather than a copy.
    let raw = destination
      .as_raw()
      .ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "encodeInto: destination is detached"))?;
    // SAFETY: `raw.ptr`/`raw.len` come from the live `Uint8Array` this
    // call was handed, and nothing re-enters JS between here and the
    // copy, so the buffer cannot be detached or resized underneath it.
    #[allow(unsafe_code)]
    let dest = unsafe { std::slice::from_raw_parts_mut(raw.ptr.as_ptr(), raw.len) };
    dest[..written].copy_from_slice(&source.as_bytes()[..written]);
    let res = Object::new(ctx)?;
    res.set("read", read)?;
    res.set("written", written)?;
    Ok(res)
  }
}

/// Monotonic base for `performance.now()`, and the wall-clock instant it
/// corresponds to (`performance.timeOrigin`). Both are fixed at first
/// use, which is process start for any real session.
static PROCESS_START: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);
static TIME_ORIGIN: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
  // Touch the monotonic base first so the two are taken together.
  let _ = *PROCESS_START;
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
});

/// The WHATWG encoding labels that map to UTF-8. Any other label is a
/// `RangeError` rather than a silent misdecode — only UTF-8 is
/// implemented, so claiming to honour e.g. `windows-1252` would corrupt
/// data quietly.
const UTF8_LABELS: [&str; 8] = [
  "utf-8",
  "utf8",
  "unicode-1-1-utf-8",
  "unicode11utf8",
  "unicode20utf8",
  "x-unicode20utf8",
  "unicode-1-1-utf8",
  "csutf8",
];

/// TextDecoder — UTF-8, with `fatal`, `ignoreBOM` and streaming.
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename = "TextDecoder")]
pub struct TextDecoder {
  #[qjs(skip_trace)]
  fatal: bool,
  #[qjs(skip_trace)]
  ignore_bom: bool,
  /// Bytes of a code point split across a `{ stream: true }` call,
  /// carried into the next `decode`.
  #[qjs(skip_trace)]
  pending: Vec<u8>,
  /// Cleared after the first chunk: the BOM is only stripped at the
  /// start of the stream.
  #[qjs(skip_trace)]
  at_start: bool,
}

/// Split `bytes` into the longest valid UTF-8 prefix and a trailing
/// remainder that is a possible (incomplete) code point. Returns `None`
/// for the remainder when the trailing bytes are genuinely invalid
/// rather than merely truncated.
fn split_incomplete(bytes: &[u8]) -> (usize, bool) {
  match std::str::from_utf8(bytes) {
    Ok(_) => (bytes.len(), true),
    Err(e) => {
      let valid = e.valid_up_to();
      // `error_len() == None` means "unexpected end of input": the tail
      // is a truncated code point, not a malformed one.
      (valid, e.error_len().is_none())
    },
  }
}

#[rquickjs::methods]
impl TextDecoder {
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'_>, label: Opt<String>, options: Opt<Object<'_>>) -> rquickjs::Result<Self> {
    if let Some(label) = label.0 {
      let normalized = label.trim().to_ascii_lowercase();
      if !UTF8_LABELS.contains(&normalized.as_str()) {
        return Err(rquickjs::Exception::throw_range(
          &ctx,
          &format!("TextDecoder constructor: the given encoding '{label}' is not supported"),
        ));
      }
    }
    let flag = |name: &str| {
      options
        .0
        .as_ref()
        .and_then(|o| o.get::<_, bool>(name).ok())
        .unwrap_or(false)
    };
    Ok(Self {
      fatal: flag("fatal"),
      ignore_bom: flag("ignoreBOM"),
      pending: Vec::new(),
      at_start: true,
    })
  }

  #[qjs(get, rename = "encoding")]
  pub fn encoding(&self) -> &'static str {
    "utf-8"
  }

  #[qjs(get, rename = "fatal")]
  pub fn fatal(&self) -> bool {
    self.fatal
  }

  #[qjs(get, rename = "ignoreBOM")]
  pub fn ignore_bom(&self) -> bool {
    self.ignore_bom
  }

  /// `decode(input?, { stream? })`. With `stream: true` a code point
  /// split across chunk boundaries is held back and prepended to the
  /// next call; the final call (no `stream`) flushes, so a still-
  /// incomplete tail is an error under `fatal` and U+FFFD otherwise.
  pub fn decode(&mut self, ctx: Ctx<'_>, input: Opt<Value<'_>>, options: Opt<Object<'_>>) -> rquickjs::Result<String> {
    let streaming = options
      .0
      .as_ref()
      .and_then(|o| o.get::<_, bool>("stream").ok())
      .unwrap_or(false);

    let mut bytes = std::mem::take(&mut self.pending);
    if let Some(v) = input.0 {
      bytes.extend_from_slice(&value_to_bytes(&v)?);
    }
    if !self.ignore_bom && self.at_start && bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
      bytes.drain(..3);
    }
    if !bytes.is_empty() {
      self.at_start = false;
    }

    let mut consume = bytes.len();
    if streaming {
      let (valid_up_to, truncated) = split_incomplete(&bytes);
      if truncated {
        // Hold the truncated tail back for the next chunk.
        self.pending = bytes[valid_up_to..].to_vec();
        consume = valid_up_to;
      }
    } else {
      self.at_start = true;
    }

    let chunk = &bytes[..consume];
    if self.fatal {
      return std::str::from_utf8(chunk).map(str::to_owned).map_err(|_| {
        rquickjs::Exception::throw_type(&ctx, "TextDecoder.decode: the encoded data was not valid UTF-8")
      });
    }
    Ok(String::from_utf8_lossy(chunk).into_owned())
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

/// HTML `structuredClone(value)` — a deep clone by the structured-clone
/// algorithm.
///
/// Handles cycles and repeated references (the same object reached twice
/// stays the same object in the clone), `Array`, plain `Object`, `Map`,
/// `Set`, `Date`, `RegExp`, `ArrayBuffer` and typed arrays. Functions,
/// symbols and class instances are not cloneable and raise a
/// `DataCloneError` `DOMException`, per spec — never a silent
/// pass-through, which would alias the original.
fn structured_clone<'js>(ctx: Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Value<'js>> {
  let mut seen: Vec<(Value<'js>, Value<'js>)> = Vec::new();
  clone_value(&ctx, &value, &mut seen)
}

fn data_clone_error(ctx: &Ctx<'_>, what: &str) -> rquickjs::Error {
  let ex = ferridriver_jsstd::exceptions::DOMException::new_with_name(
    ctx,
    ferridriver_jsstd::exceptions::DOMExceptionName::DataCloneError,
    format!("{what} could not be cloned"),
  );
  match ex.and_then(|ex| Class::instance(ctx.clone(), ex)) {
    Ok(ex) => ctx.throw(ex.into_value()),
    Err(e) => e,
  }
}

fn clone_value<'js>(
  ctx: &Ctx<'js>,
  value: &Value<'js>,
  seen: &mut Vec<(Value<'js>, Value<'js>)>,
) -> rquickjs::Result<Value<'js>> {
  if value.is_function() {
    return Err(data_clone_error(ctx, "a function"));
  }
  if value.type_of() == rquickjs::Type::Symbol {
    return Err(data_clone_error(ctx, "a symbol"));
  }
  let Some(obj) = value.as_object() else {
    // Primitives are immutable: cloning is identity.
    return Ok(value.clone());
  };
  if let Some((_, clone)) = seen.iter().find(|(orig, _)| orig.as_object() == Some(obj)) {
    return Ok(clone.clone());
  }

  let globals = ctx.globals();
  let is_a = |name: &str| -> rquickjs::Result<bool> {
    let ctor: Value<'js> = globals.get(name)?;
    Ok(obj.is_instance_of(&ctor))
  };

  // Dates and RegExps round-trip through their own constructors.
  if is_a("Date")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("Date")?;
    let time: f64 = obj
      .get::<_, rquickjs::Function<'js>>("getTime")?
      .call((This(obj.clone()),))?;
    return ctor.construct::<_, Value<'js>>((time,));
  }
  if is_a("RegExp")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("RegExp")?;
    let source: String = obj.get("source")?;
    let flags: String = obj.get("flags")?;
    return ctor.construct::<_, Value<'js>>((source, flags));
  }
  if let Some(buf) = rquickjs::ArrayBuffer::from_object(obj.clone()) {
    let bytes = buf.as_bytes().unwrap_or_default().to_vec();
    return Ok(rquickjs::ArrayBuffer::new(ctx.clone(), bytes)?.into_value());
  }
  if let Ok(ta) = TypedArray::<u8>::from_value(value.clone()) {
    let bytes = ta.as_bytes().unwrap_or_default().to_vec();
    return Ok(TypedArray::new(ctx.clone(), bytes)?.into_value());
  }

  if let Some(arr) = value.as_array() {
    let out = rquickjs::Array::new(ctx.clone())?;
    seen.push((value.clone(), out.clone().into_value()));
    for i in 0..arr.len() {
      let item: Value<'js> = arr.get(i)?;
      out.set(i, clone_value(ctx, &item, seen)?)?;
    }
    return Ok(out.into_value());
  }

  if is_a("Map")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("Map")?;
    let out: Value<'js> = ctor.construct(())?;
    seen.push((value.clone(), out.clone()));
    let out_obj = out.as_object().cloned().unwrap_or_else(|| obj.clone());
    let set: rquickjs::Function<'js> = out_obj.get("set")?;
    for entry in iterate_entries(ctx, obj)? {
      let (k, v) = entry?;
      set.call::<_, ()>((
        This(out_obj.clone()),
        clone_value(ctx, &k, seen)?,
        clone_value(ctx, &v, seen)?,
      ))?;
    }
    return Ok(out);
  }
  if is_a("Set")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("Set")?;
    let out: Value<'js> = ctor.construct(())?;
    seen.push((value.clone(), out.clone()));
    let out_obj = out.as_object().cloned().unwrap_or_else(|| obj.clone());
    let add: rquickjs::Function<'js> = out_obj.get("add")?;
    for entry in iterate_entries(ctx, obj)? {
      let (k, _) = entry?;
      add.call::<_, ()>((This(out_obj.clone()), clone_value(ctx, &k, seen)?))?;
    }
    return Ok(out);
  }

  // Anything with a non-Object prototype (a class instance, including
  // the native web classes) is not a cloneable "plain object".
  let object_ctor: Value<'js> = globals.get("Object")?;
  let proto = obj.get_prototype();
  let object_proto = object_ctor
    .as_object()
    .and_then(|o| o.get::<_, Value<'js>>("prototype").ok())
    .and_then(|v| v.as_object().cloned());
  if proto.is_some() && proto != object_proto {
    return Err(data_clone_error(ctx, "an object that is not a plain object"));
  }

  let out = Object::new(ctx.clone())?;
  seen.push((value.clone(), out.clone().into_value()));
  for key in obj.keys::<String>() {
    let key = key?;
    let v: Value<'js> = obj.get(&key)?;
    out.set(key, clone_value(ctx, &v, seen)?)?;
  }
  Ok(out.into_value())
}

/// `[...target.entries()]` as `(key, value)` pairs — how a `Map`'s
/// contents (and, with the value ignored, a `Set`'s) are read without
/// assuming an internal representation.
#[allow(clippy::type_complexity)]
fn iterate_entries<'js>(
  ctx: &Ctx<'js>,
  target: &Object<'js>,
) -> rquickjs::Result<Vec<rquickjs::Result<(Value<'js>, Value<'js>)>>> {
  let entries: rquickjs::Function<'js> = target.get("entries")?;
  let iter: Value<'js> = entries.call((This(target.clone()),))?;
  let array_ctor: Value<'js> = ctx.globals().get("Array")?;
  let from: rquickjs::Function<'js> = array_ctor
    .as_object()
    .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "Array is not an object"))?
    .get("from")?;
  let list: rquickjs::Array<'js> = from.call((This(array_ctor), iter))?;
  Ok(
    (0..list.len())
      .map(|i| {
        let pair: rquickjs::Array<'js> = list.get(i)?;
        Ok((pair.get(0)?, pair.get(1)?))
      })
      .collect(),
  )
}

/// Install the native web-API classes + globals. Called once at
/// `Session::create`; persists across executions like the rest of the
/// browser-like runtime surface.
pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  Class::<TextEncoder>::define(&globals)?;
  Class::<TextDecoder>::define(&globals)?;
  Class::<Url>::define(&globals)?;

  globals.set("structuredClone", Func::from(structured_clone))?;

  // `performance.now()` — milliseconds (fractional) since the session's
  // process start, plus the `timeOrigin` those are relative to. A
  // monotonic `Instant` base, so it cannot go backwards across a wall-
  // clock adjustment the way `Date.now()` deltas can.
  {
    let performance = Object::new(ctx.clone())?;
    performance.set("now", Func::from(|| PROCESS_START.elapsed().as_secs_f64() * 1000.0))?;
    performance.set("timeOrigin", *TIME_ORIGIN)?;
    globals.set("performance", performance)?;
  }

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
