//! A WHATWG-ish `fetch` + `Headers` + `Response`, so npm packages that
//! expect `fetch` work. It is a thin surface over the SAME
//! `ferridriver::http_client` core the Playwright-style `request`
//! binding uses — one HTTP stack, one place the net policy applies. The
//! ergonomic `request` API stays; this just adds the standard entry
//! point.
//!
//! Web-standard names: `Headers`, `Request`, `Response` are the WHATWG
//! classes (the Playwright page-network `Request`/`Response` are no
//! longer globals — they were never globals in Playwright either, only
//! return values). `Headers` is spec (lowercase + RFC7230 validate,
//! value normalize, `, ` combine, separate `set-cookie` +
//! `getSetCookie`, sorted real iterators, `forEach`). `Response` /
//! `Request` are constructible with the spec accessors
//! (`status`/`ok`/`redirected`/`type`/`bodyUsed`/`headers`/...),
//! single-use bodies (`text`/`json`/`arrayBuffer`), `clone()`, and
//! static `Response.json`/`error`/`redirect`. `fetch(url, { signal })`
//! is wired to `AbortController`/`AbortSignal` (see [`super::abort`]):
//! an already-aborted signal rejects before I/O and an in-flight abort
//! drops the request future. `Response.body` is a `ReadableStream`
//! that pulls chunks live off the socket (the body is NOT buffered;
//! `text()`/`json()`/`arrayBuffer()` drain it on demand) — see
//! [`super::streams`]. `Blob` and `FormData` (see [`super::blob`] /
//! [`super::form_data`]) are accepted as bodies — a `Blob` sends its
//! bytes + type, a `FormData` is serialized as `multipart/form-data`.
//! Still a subset: `clone()` of a not-yet-read streamed `Response`
//! throws (no stream tee); a `signal` on a `Request`
//! instance is not yet forwarded (pass it via `init.signal`);
//! `init.redirect` maps onto the per-request redirect
//! cap (`manual`/`error` -> don't follow; a spec-exact opaque-redirect /
//! rejection is not distinguishable through reqwest's per-request
//! policy).
//!
//! Net policy: `fetch` is a facade over the SAME core a net-restricted
//! tool's `request` wraps, so the `allow.net` allow-list must bind here
//! too — otherwise a tool restricted to host X could reach anywhere via
//! the global `fetch`. The per-tool allow-list lives in [`NetPolicyUd`]
//! (VM userdata); `plugins::dispatch_tool` brackets each handler poll so
//! the policy in effect is whichever tool's continuation is running, and
//! `fetch` snapshots it synchronously at call time (before any I/O).

use std::sync::{Arc, Mutex};

use ferridriver::http_client::{HttpClient, RequestOptions};
use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Func, Opt};
use rquickjs::{Coerced, Ctx, IntoJs, Object, Value, class::Class, class::Trace};

use crate::bindings::convert::json_to_js;
use crate::bindings::http_client::net_check;

/// Per-VM carrier of the *currently active* tool net allow-list. `None`
/// (the resting state, and what the top-level script sees) means
/// unrestricted; `Some(list)` means default-deny against `list`.
///
/// One cell per session VM, stored as rquickjs userdata at
/// [`crate::engine::Session::create`]. `plugins::dispatch_tool` swaps the
/// active policy in/out around every poll of a tool handler's future so
/// nested and concurrently-interleaved tool calls each see their own
/// declared `allow.net` — the swap is synchronous and the `fetch` guard
/// reads the cell synchronously within the same poll, so single-threaded
/// QuickJS execution makes it race-free without locking the JS thread.
#[derive(Clone, Default)]
pub(crate) struct NetPolicy(Arc<Mutex<Option<Arc<[String]>>>>);

impl NetPolicy {
  fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<[String]>>> {
    self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Snapshot the active allow-list (cheap clone of the `Arc`).
  pub(crate) fn current(&self) -> Option<Arc<[String]>> {
    self.lock().clone()
  }

  /// Install `next` as the active policy, returning the previous value
  /// so a poll-scoped guard can restore it.
  pub(crate) fn swap(&self, next: Option<Arc<[String]>>) -> Option<Arc<[String]>> {
    std::mem::replace(&mut *self.lock(), next)
  }
}

/// rquickjs userdata wrapper for the session's [`NetPolicy`] cell.
pub(crate) struct NetPolicyUd(pub(crate) NetPolicy);

// SAFETY: holds only owned `'static` data (`Arc`/`Mutex`), no borrowed JS.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for NetPolicyUd {
  type Changed<'to> = NetPolicyUd;
}

/// Snapshot the session's active net allow-list, if any. Called
/// synchronously at `fetch()` invocation time so the snapshot reflects
/// the tool whose continuation is currently executing.
pub(crate) fn active_net(ctx: &Ctx<'_>) -> Option<Arc<[String]>> {
  ctx.userdata::<NetPolicyUd>().and_then(|u| u.0.current())
}

/// WHATWG `Headers` (spec subset, no external deps): names are
/// lowercased and RFC7230-validated, values are HTTP-whitespace
/// normalized and validated, `append` combines same-name values with
/// `, ` (`; ` for `cookie`) while `set-cookie` is kept as separate
/// entries, `getSetCookie()` returns them all, and iteration is sorted
/// by name. `keys`/`values`/`entries`/`[Symbol.iterator]` return real
/// iterator objects.
#[derive(Trace)]
#[rquickjs::class(rename = "Headers")]
pub struct HeadersJs {
  /// Lowercased name -> spec-combined value. `set-cookie` may appear
  /// multiple times (never combined).
  #[qjs(skip_trace)]
  pairs: Vec<(String, String)>,
}

#[derive(Clone, Copy)]
enum IterKind {
  Entries,
  Keys,
  Values,
}

/// A real JS iterator over a sorted header snapshot: `{ next(),
/// [Symbol.iterator]() }`. Captures only `Send` data (the crate builds
/// rquickjs with `parallel`, so `Func` closures must be `Send`); JS
/// values are built from `ctx` inside `next`. `[Symbol.iterator]`
/// returns an object sharing THIS cursor's position (`pos`), so it
/// behaves as the spec's "return the iterator itself" — `[...it]` after
/// a partial `next()` continues rather than restarting.
fn make_header_iter<'js>(
  ctx: &Ctx<'js>,
  data: Arc<Vec<(String, String)>>,
  pos: Arc<std::sync::atomic::AtomicUsize>,
  kind: IterKind,
) -> rquickjs::Result<Object<'js>> {
  let it = Object::new(ctx.clone())?;
  {
    let data = data.clone();
    let pos = pos.clone();
    it.set(
      PredefinedAtom::Next,
      Func::from(move |ctx: Ctx<'js>| -> rquickjs::Result<Object<'js>> {
        let r = Object::new(ctx.clone())?;
        let i = pos.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some((k, v)) = data.get(i) {
          let value: Value<'js> = match kind {
            IterKind::Entries => {
              let a = rquickjs::Array::new(ctx.clone())?;
              a.set(0, k.clone())?;
              a.set(1, v.clone())?;
              a.into_value()
            },
            IterKind::Keys => k.clone().into_js(&ctx)?,
            IterKind::Values => v.clone().into_js(&ctx)?,
          };
          r.set(PredefinedAtom::Value, value)?;
          r.set(PredefinedAtom::Done, false)?;
        } else {
          pos.store(data.len(), std::sync::atomic::Ordering::Relaxed);
          r.set(PredefinedAtom::Value, Value::new_undefined(ctx.clone()))?;
          r.set(PredefinedAtom::Done, true)?;
        }
        Ok(r)
      }),
    )?;
  }
  {
    let data = data.clone();
    let pos = pos.clone();
    it.set(
      PredefinedAtom::SymbolIterator,
      Func::from(move |ctx: Ctx<'js>| make_header_iter(&ctx, data.clone(), pos.clone(), kind)),
    )?;
  }
  Ok(it)
}

/// Fresh iterator (cursor at 0) over a header snapshot.
fn new_header_iter<'js>(ctx: &Ctx<'js>, data: Vec<(String, String)>, kind: IterKind) -> rquickjs::Result<Object<'js>> {
  make_header_iter(
    ctx,
    Arc::new(data),
    Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    kind,
  )
}

/// RFC 7230 token: a valid header field name.
fn is_header_name(name: &str) -> bool {
  !name.is_empty()
    && name.bytes().all(|b| {
      matches!(b,
        b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+'
        | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
        | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z')
    })
}

/// A valid (already-normalized) header field value: HTAB, SP, VCHAR,
/// form-feed, or NBSP.
fn is_header_value(value: &str) -> bool {
  value
    .chars()
    .all(|c| c == '\t' || c == ' ' || ('\u{21}'..='\u{7E}').contains(&c) || c == '\u{0C}' || c == '\u{00A0}')
}

/// WHATWG header value normalization (WPT `headers-normalize`): strip
/// leading/trailing SP/HTAB, drop bare CR/LF, and treat an obs-fold
/// (CRLF + SP/HTAB) as a single space; runs of inner whitespace
/// collapse to the last one seen.
fn normalize_header_value(text: &str) -> String {
  let input = text.as_bytes();
  let mut out: Vec<u8> = Vec::with_capacity(input.len());
  let mut read = 0;
  while read < input.len() && (input[read] == b' ' || input[read] == b'\t') {
    read += 1;
  }
  let mut pending: Option<u8> = None;
  while read < input.len() {
    match input[read] {
      b'\r'
        if read + 2 < input.len()
          && input[read + 1] == b'\n'
          && (input[read + 2] == b' ' || input[read + 2] == b'\t') =>
      {
        pending = Some(input[read + 2]);
        read += 3;
      },
      b'\r' | b'\n' => read += 1,
      b' ' | b'\t' => {
        pending = Some(input[read]);
        read += 1;
      },
      byte => {
        if let Some(ws) = pending.take()
          && !out.is_empty()
        {
          out.push(ws);
        }
        out.push(byte);
        read += 1;
      },
    }
  }
  while matches!(out.last(), Some(b' ' | b'\t')) {
    out.pop();
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// WHATWG `Response` (spec subset). Constructible (`new Response(body?,
/// init?)`), with `status`/`ok`/`statusText`/`url`/`redirected`/`type`/
/// `bodyUsed`/`headers` accessors, `text`/`json`/`arrayBuffer` body
/// readers (single-use: a second read throws, per spec), `clone()`
/// (throws once the body is used), and static `Response.json`,
/// `Response.error`, `Response.redirect`. This is the global `Response`
/// (the Playwright page-network `Response` is no longer a global — it is
/// only ever a return value, matching Playwright itself).
#[derive(Trace)]
#[rquickjs::class(rename = "Response")]
pub struct FetchResponseJs {
  #[qjs(skip_trace)]
  status: u16,
  #[qjs(skip_trace)]
  status_text: String,
  #[qjs(skip_trace)]
  url: String,
  #[qjs(skip_trace)]
  headers: Vec<(String, String)>,
  #[qjs(skip_trace)]
  body: Vec<u8>,
  #[qjs(skip_trace)]
  redirected: bool,
  #[qjs(skip_trace)]
  type_: &'static str,
  #[qjs(skip_trace)]
  body_used: bool,
  /// `Some` for a `fetch()` result: the live, not-yet-buffered
  /// response. `text`/`json`/`arrayBuffer` drain it; `body` hands it to
  /// a `ReadableStream`. `None` for a constructed/`Response.json/error/
  /// redirect` (the bytes are in `body`).
  #[qjs(skip_trace)]
  net: Option<Arc<tokio::sync::Mutex<Option<ferridriver::http_client::HttpStreamResponse>>>>,
}

/// WHATWG `Request` (spec subset). Constructible (`new Request(input,
/// init?)` where `input` is a URL string or another `Request`), with
/// `url`/`method`/`headers`/`redirect`/`credentials`/`bodyUsed`
/// accessors and `text`/`json`/`arrayBuffer`/`clone`. `signal` is
/// accepted and stored but not yet wired (AbortController follow-up);
/// `fetch` reads `url`/`method`/`headers`/`body`/`redirect` off a
/// `Request` argument.
#[derive(Trace)]
#[rquickjs::class(rename = "Request")]
pub struct FetchRequestJs {
  #[qjs(skip_trace)]
  url: String,
  #[qjs(skip_trace)]
  method: String,
  #[qjs(skip_trace)]
  headers: Vec<(String, String)>,
  #[qjs(skip_trace)]
  body: Vec<u8>,
  #[qjs(skip_trace)]
  redirect: String,
  #[qjs(skip_trace)]
  credentials: String,
  #[qjs(skip_trace)]
  body_used: bool,
}

// SAFETY: only owned `'static` data.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for HeadersJs {
  type Changed<'to> = HeadersJs;
}
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for FetchResponseJs {
  type Changed<'to> = FetchResponseJs;
}
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for FetchRequestJs {
  type Changed<'to> = FetchRequestJs;
}

/// Extract a request/response body from a JS value, returning the bytes
/// and the default `content-type` the body type implies (string ->
/// `text/plain;charset=UTF-8`, object -> JSON; `Headers`/null/undefined
/// -> none). Caller applies the content-type only if not already set.
fn extract_body<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> (Vec<u8>, Option<&'static str>) {
  if v.is_undefined() || v.is_null() {
    return (Vec::new(), None);
  }
  if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
    return (s.into_bytes(), Some("text/plain;charset=UTF-8"));
  }
  if v.is_object() {
    if let Ok(j) = crate::bindings::convert::serde_from_js::<serde_json::Value>(ctx, v.clone()) {
      return (j.to_string().into_bytes(), Some("application/json"));
    }
  }
  (Vec::new(), None)
}

/// Parse a `Response`/`Request` `init` bag's `headers` into raw pairs
/// and apply `default_ct` as `content-type` unless already present.
fn init_headers(init: Option<&Object<'_>>, default_ct: Option<&'static str>) -> Vec<(String, String)> {
  let mut pairs = init
    .and_then(|o| o.get::<_, Value<'_>>("headers").ok())
    .map(|v| header_pairs_from(&v))
    .unwrap_or_default();
  if let Some(ct) = default_ct
    && !pairs.iter().any(|(k, _)| k == "content-type")
  {
    pairs.push(("content-type".to_string(), ct.to_string()));
  }
  pairs
}

/// Infallible best-effort extraction of `(name,value)` pairs from a JS
/// value (`Headers` instance, `[[k,v],...]` sequence, or record) for
/// the outbound request `headers` — invalid entries are skipped rather
/// than thrown (the throwing path is the `Headers` constructor).
fn header_pairs_from(v: &Value<'_>) -> Vec<(String, String)> {
  if let Ok(h) = Class::<HeadersJs>::from_value(v) {
    return h.borrow().pairs.clone();
  }
  let mut acc = HeadersJs { pairs: Vec::new() };
  if let Some(arr) = v.as_array() {
    for i in 0..arr.len() {
      if let Ok(entry) = arr.get::<Value<'_>>(i)
        && let Some(pair) = entry.as_array()
        && pair.len() == 2
        && let (Ok(k), Ok(val)) = (pair.get::<Coerced<String>>(0), pair.get::<Coerced<String>>(1))
        && is_header_name(&k.0)
      {
        acc.append_normalized(k.0.to_ascii_lowercase(), normalize_header_value(&val.0));
      }
    }
    return acc.pairs;
  }
  if let Some(obj) = v.as_object()
    && let Ok(keys) = obj.keys::<String>().collect::<rquickjs::Result<Vec<_>>>()
  {
    for k in keys {
      if let Ok(val) = obj.get::<_, Coerced<String>>(k.as_str())
        && is_header_name(&k)
      {
        acc.append_normalized(k.to_ascii_lowercase(), normalize_header_value(&val.0));
      }
    }
  }
  acc.pairs
}

impl HeadersJs {
  /// Spec "append": `set-cookie` is never combined; other repeats join
  /// with `, ` (`; ` for `cookie`). `name_lc` must already be lowercased
  /// and `value` normalized.
  fn append_normalized(&mut self, name_lc: String, value: String) {
    if name_lc == "set-cookie" {
      self.pairs.push((name_lc, value));
      return;
    }
    if let Some(i) = self.pairs.iter().position(|(k, _)| k == &name_lc) {
      let sep = if name_lc == "cookie" { "; " } else { ", " };
      self.pairs[i].1 = format!("{}{sep}{value}", self.pairs[i].1);
    } else {
      self.pairs.push((name_lc, value));
    }
  }

  /// Build from known server/response pairs (lowercase + normalize +
  /// spec-combine). Used by `FetchResponseJs::headers`.
  pub(crate) fn from_pairs<I: IntoIterator<Item = (String, String)>>(it: I) -> Self {
    let mut h = Self { pairs: Vec::new() };
    for (k, v) in it {
      h.append_normalized(k.to_ascii_lowercase(), normalize_header_value(&v));
    }
    h
  }

  /// Sorted-by-name snapshot for iteration (`sort_by` is stable, so
  /// repeated `set-cookie` keep insertion order).
  fn sorted(&self) -> Vec<(String, String)> {
    let mut v = self.pairs.clone();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
  }

  fn check_name(ctx: &Ctx<'_>, name: &str) -> rquickjs::Result<String> {
    if is_header_name(name) {
      Ok(name.to_ascii_lowercase())
    } else {
      Err(rquickjs::Exception::throw_type(
        ctx,
        &format!("Invalid header name: {name:?}"),
      ))
    }
  }

  fn check_value(ctx: &Ctx<'_>, raw: &str) -> rquickjs::Result<String> {
    let v = normalize_header_value(raw);
    if is_header_value(&v) {
      Ok(v)
    } else {
      Err(rquickjs::Exception::throw_type(ctx, "Invalid header value"))
    }
  }

  fn fill_from_value<'js>(&mut self, ctx: &Ctx<'js>, v: &Value<'js>) -> rquickjs::Result<()> {
    if let Ok(other) = Class::<HeadersJs>::from_value(v) {
      for (k, val) in &other.borrow().pairs {
        self.append_normalized(k.clone(), val.clone());
      }
      return Ok(());
    }
    if let Some(arr) = v.as_array() {
      for i in 0..arr.len() {
        let entry = arr.get::<Value<'js>>(i)?;
        let pair = entry
          .as_array()
          .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "Header init entry is not a [name, value] pair"))?;
        if pair.len() != 2 {
          return Err(rquickjs::Exception::throw_type(
            ctx,
            "Header init entry must be a [name, value] pair",
          ));
        }
        let name = Self::check_name(ctx, &pair.get::<Coerced<String>>(0)?.0)?;
        let value = Self::check_value(ctx, &pair.get::<Coerced<String>>(1)?.0)?;
        self.append_normalized(name, value);
      }
      return Ok(());
    }
    if let Some(obj) = v.as_object() {
      for k in obj.keys::<String>().collect::<rquickjs::Result<Vec<_>>>()? {
        let name = Self::check_name(ctx, &k)?;
        let value = Self::check_value(ctx, &obj.get::<_, Coerced<String>>(k.as_str())?.0)?;
        self.append_normalized(name, value);
      }
    }
    Ok(())
  }
}

#[rquickjs::methods]
impl HeadersJs {
  #[qjs(constructor)]
  pub fn new<'js>(ctx: Ctx<'js>, init: Opt<Value<'js>>) -> rquickjs::Result<Self> {
    let mut h = Self { pairs: Vec::new() };
    if let Some(v) = init.0 {
      if v.is_null() || v.is_number() {
        return Err(rquickjs::Exception::throw_type(
          &ctx,
          "Failed to construct 'Headers': invalid init",
        ));
      }
      if !v.is_undefined() {
        h.fill_from_value(&ctx, &v)?;
      }
    }
    Ok(h)
  }

  #[qjs(rename = "append")]
  pub fn append(&mut self, ctx: Ctx<'_>, name: String, value: Coerced<String>) -> rquickjs::Result<()> {
    let n = Self::check_name(&ctx, &name)?;
    let v = Self::check_value(&ctx, &value.0)?;
    self.append_normalized(n, v);
    Ok(())
  }

  #[qjs(rename = "set")]
  pub fn set(&mut self, ctx: Ctx<'_>, name: String, value: Coerced<String>) -> rquickjs::Result<()> {
    let n = Self::check_name(&ctx, &name)?;
    let v = Self::check_value(&ctx, &value.0)?;
    self.pairs.retain(|(k, _)| k != &n);
    self.pairs.push((n, v));
    Ok(())
  }

  #[qjs(rename = "get")]
  pub fn get<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let n = Self::check_name(&ctx, &name)?;
    let matches: Vec<&str> = self
      .pairs
      .iter()
      .filter(|(k, _)| k == &n)
      .map(|(_, v)| v.as_str())
      .collect();
    if matches.is_empty() {
      Ok(Value::new_null(ctx))
    } else {
      matches.join(", ").into_js(&ctx)
    }
  }

  #[qjs(rename = "getSetCookie")]
  pub fn get_set_cookie(&self) -> Vec<String> {
    self
      .pairs
      .iter()
      .filter(|(k, _)| k == "set-cookie")
      .map(|(_, v)| v.clone())
      .collect()
  }

  #[qjs(rename = "has")]
  pub fn has(&self, ctx: Ctx<'_>, name: String) -> rquickjs::Result<bool> {
    let n = Self::check_name(&ctx, &name)?;
    Ok(self.pairs.iter().any(|(k, _)| k == &n))
  }

  #[qjs(rename = "delete")]
  pub fn delete(&mut self, ctx: Ctx<'_>, name: String) -> rquickjs::Result<()> {
    let n = Self::check_name(&ctx, &name)?;
    self.pairs.retain(|(k, _)| k != &n);
    Ok(())
  }

  #[qjs(rename = "entries")]
  pub fn entries<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    new_header_iter(&ctx, self.sorted(), IterKind::Entries)
  }

  #[qjs(rename = "keys")]
  pub fn keys<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    new_header_iter(&ctx, self.sorted(), IterKind::Keys)
  }

  #[qjs(rename = "values")]
  pub fn values<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    new_header_iter(&ctx, self.sorted(), IterKind::Values)
  }

  #[qjs(rename = PredefinedAtom::SymbolIterator)]
  pub fn js_iterator<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    new_header_iter(&ctx, self.sorted(), IterKind::Entries)
  }

  #[qjs(rename = "forEach")]
  pub fn for_each(&self, cb: rquickjs::Function<'_>) -> rquickjs::Result<()> {
    for (k, v) in self.sorted() {
      cb.call::<_, ()>((v, k))?;
    }
    Ok(())
  }
}

impl FetchResponseJs {
  /// The `Response` a `fetch()` resolves to: status/headers are known,
  /// the body streams from `stream` (not buffered).
  fn from_stream(
    status: u16,
    status_text: String,
    url: String,
    headers: Vec<(String, String)>,
    redirected: bool,
    stream: ferridriver::http_client::HttpStreamResponse,
  ) -> Self {
    Self {
      status,
      status_text,
      url,
      headers,
      body: Vec::new(),
      redirected,
      type_: "basic",
      body_used: false,
      net: Some(Arc::new(tokio::sync::Mutex::new(Some(stream)))),
    }
  }

  /// WHATWG "consume body": a second read is a `TypeError`. Drains the
  /// live response to completion when this is a streamed `fetch` body,
  /// else returns the in-memory bytes.
  async fn consume(&mut self, ctx: &Ctx<'_>) -> rquickjs::Result<Vec<u8>> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(ctx, "Body has already been consumed"));
    }
    self.body_used = true;
    if let Some(net) = &self.net {
      let mut guard = net.lock().await;
      let mut out = Vec::new();
      if let Some(resp) = guard.as_mut() {
        while let Some(chunk) = resp
          .chunk()
          .await
          .map_err(|e| rquickjs::Exception::throw_type(ctx, &e.to_string()))?
        {
          out.extend_from_slice(&chunk);
        }
      }
      *guard = None;
      return Ok(out);
    }
    Ok(std::mem::take(&mut self.body))
  }
}

#[rquickjs::methods]
impl FetchResponseJs {
  /// `new Response(body?, init?)` — `init`: `{ status?, statusText?,
  /// headers? }`. `status` outside 200..=599 is a `RangeError`.
  #[qjs(constructor)]
  pub fn new<'js>(ctx: Ctx<'js>, body: Opt<Value<'js>>, init: Opt<Object<'js>>) -> rquickjs::Result<Self> {
    let init = init.0;
    let status = match init.as_ref().and_then(|o| o.get::<_, i64>("status").ok()) {
      Some(s) if !(200..=599).contains(&s) => {
        return Err(rquickjs::Exception::throw_range(
          &ctx,
          "Failed to construct 'Response': status is outside the range [200, 599]",
        ));
      },
      Some(s) => s as u16,
      None => 200,
    };
    let status_text = init
      .as_ref()
      .and_then(|o| o.get::<_, String>("statusText").ok())
      .unwrap_or_default();
    let (bytes, default_ct) = body.0.map_or((Vec::new(), None), |v| extract_body(&ctx, &v));
    Ok(Self {
      status,
      status_text,
      url: String::new(),
      headers: init_headers(init.as_ref(), default_ct),
      body: bytes,
      redirected: false,
      type_: "default",
      body_used: false,
      net: None,
    })
  }

  /// `Response.json(data, init?)` — JSON body + `application/json`.
  #[qjs(static, rename = "json")]
  pub fn json_static<'js>(ctx: Ctx<'js>, data: Value<'js>, init: Opt<Object<'js>>) -> rquickjs::Result<Self> {
    let init = init.0;
    let json: serde_json::Value = crate::bindings::convert::serde_from_js(&ctx, data)?;
    let status = init
      .as_ref()
      .and_then(|o| o.get::<_, i64>("status").ok())
      .unwrap_or(200) as u16;
    let status_text = init
      .as_ref()
      .and_then(|o| o.get::<_, String>("statusText").ok())
      .unwrap_or_default();
    Ok(Self {
      status,
      status_text,
      url: String::new(),
      headers: init_headers(init.as_ref(), Some("application/json")),
      body: json.to_string().into_bytes(),
      redirected: false,
      type_: "default",
      body_used: false,
      net: None,
    })
  }

  /// `Response.error()` — a network-error response (status 0).
  #[qjs(static, rename = "error")]
  pub fn error() -> Self {
    Self {
      status: 0,
      status_text: String::new(),
      url: String::new(),
      headers: Vec::new(),
      body: Vec::new(),
      redirected: false,
      type_: "error",
      body_used: false,
      net: None,
    }
  }

  /// `Response.redirect(url, status=302)` — status must be a redirect
  /// code (301/302/303/307/308) or it is a `RangeError`.
  #[qjs(static, rename = "redirect")]
  pub fn redirect(ctx: Ctx<'_>, url: String, status: Opt<i64>) -> rquickjs::Result<Self> {
    let status = status.0.unwrap_or(302);
    if ![301, 302, 303, 307, 308].contains(&status) {
      return Err(rquickjs::Exception::throw_range(&ctx, "Invalid redirect status code"));
    }
    Ok(Self {
      status: status as u16,
      status_text: String::new(),
      url: String::new(),
      headers: vec![("location".to_string(), url)],
      body: Vec::new(),
      redirected: false,
      type_: "default",
      body_used: false,
      net: None,
    })
  }

  #[qjs(get, rename = "status")]
  pub fn status(&self) -> u16 {
    self.status
  }
  #[qjs(get, rename = "ok")]
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status)
  }
  #[qjs(get, rename = "statusText")]
  pub fn status_text(&self) -> String {
    self.status_text.clone()
  }
  #[qjs(get, rename = "url")]
  pub fn url(&self) -> String {
    self.url.clone()
  }
  #[qjs(get, rename = "redirected")]
  pub fn redirected(&self) -> bool {
    self.redirected
  }
  #[qjs(get, rename = "type")]
  pub fn type_(&self) -> String {
    self.type_.to_string()
  }
  #[qjs(get, rename = "bodyUsed")]
  pub fn body_used(&self) -> bool {
    self.body_used
  }

  #[qjs(get, rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, HeadersJs>> {
    Class::instance(ctx, HeadersJs::from_pairs(self.headers.iter().cloned()))
  }

  /// `Response.body` — a `ReadableStream`. For a streamed `fetch`
  /// result the stream pulls chunks live off the socket (the body is
  /// NOT buffered); for a constructed `Response` it is the in-memory
  /// bytes. Empty/done once the body was consumed by
  /// `text()`/`json()`/`arrayBuffer()`.
  #[qjs(get, rename = "body")]
  pub fn body<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, crate::bindings::streams::ReadableStreamJs>> {
    let stream = match &self.net {
      Some(net) => crate::bindings::streams::ReadableStreamJs::from_net(net.clone()),
      None => crate::bindings::streams::ReadableStreamJs::from_bytes(self.body.clone()),
    };
    Class::instance(ctx, stream)
  }

  #[qjs(rename = "text")]
  pub async fn text(&mut self, ctx: Ctx<'_>) -> rquickjs::Result<String> {
    let b = self.consume(&ctx).await?;
    Ok(String::from_utf8_lossy(&b).into_owned())
  }

  #[qjs(rename = "json")]
  pub async fn json<'js>(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let b = self.consume(&ctx).await?;
    let v: serde_json::Value = serde_json::from_slice(&b)
      .map_err(|e| rquickjs::Error::new_from_js_message("Response.json", "Error", e.to_string()))?;
    json_to_js(&ctx, &v)
  }

  #[qjs(rename = "arrayBuffer")]
  pub async fn array_buffer<'js>(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let b = self.consume(&ctx).await?;
    rquickjs::ArrayBuffer::new(ctx.clone(), b).map(rquickjs::ArrayBuffer::into_value)
  }

  #[qjs(rename = "clone")]
  pub fn clone_(&self, ctx: Ctx<'_>) -> rquickjs::Result<Self> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(&ctx, "Cannot clone a used Response"));
    }
    if self.net.is_some() {
      // Cloning would require teeing the live stream; not supported in
      // this subset (the body has not been buffered).
      return Err(rquickjs::Exception::throw_type(
        &ctx,
        "Cannot clone a streaming Response (body is not buffered)",
      ));
    }
    Ok(Self {
      status: self.status,
      status_text: self.status_text.clone(),
      url: self.url.clone(),
      headers: self.headers.clone(),
      body: self.body.clone(),
      redirected: self.redirected,
      type_: self.type_,
      body_used: false,
      net: None,
    })
  }
}

#[rquickjs::methods]
impl FetchRequestJs {
  /// `new Request(input, init?)` — `input` is a URL string or another
  /// `Request`; `init`: `{ method?, headers?, body?, redirect?,
  /// credentials?, signal? }` (`signal` accepted, not yet wired).
  #[qjs(constructor)]
  pub fn new<'js>(ctx: Ctx<'js>, input: Value<'js>, init: Opt<Object<'js>>) -> Self {
    let init = init.0;
    let mut req = if let Ok(other) = Class::<FetchRequestJs>::from_value(&input) {
      let o = other.borrow();
      Self {
        url: o.url.clone(),
        method: o.method.clone(),
        headers: o.headers.clone(),
        body: o.body.clone(),
        redirect: o.redirect.clone(),
        credentials: o.credentials.clone(),
        body_used: false,
      }
    } else {
      Self {
        url: input.as_string().and_then(|s| s.to_string().ok()).unwrap_or_default(),
        method: "GET".to_string(),
        headers: Vec::new(),
        body: Vec::new(),
        redirect: "follow".to_string(),
        credentials: "same-origin".to_string(),
        body_used: false,
      }
    };
    if let Some(o) = init.as_ref() {
      if let Ok(m) = o.get::<_, String>("method") {
        req.method = m.to_ascii_uppercase();
      }
      if let Ok(r) = o.get::<_, String>("redirect") {
        req.redirect = r;
      }
      if let Ok(c) = o.get::<_, String>("credentials") {
        req.credentials = c;
      }
      let (bytes, default_ct) = o
        .get::<_, Value<'_>>("body")
        .ok()
        .map_or((Vec::new(), None), |v| extract_body(&ctx, &v));
      if !bytes.is_empty() {
        req.body = bytes;
      }
      req.headers = {
        let mut h = init_headers(init.as_ref(), default_ct);
        if h.is_empty() {
          std::mem::take(&mut req.headers)
        } else {
          if let Ok(existing) = Class::<FetchRequestJs>::from_value(&input) {
            for (k, v) in &existing.borrow().headers {
              if !h.iter().any(|(hk, _)| hk == k) {
                h.push((k.clone(), v.clone()));
              }
            }
          }
          h
        }
      };
    }
    req
  }

  #[qjs(get, rename = "url")]
  pub fn url(&self) -> String {
    self.url.clone()
  }
  #[qjs(get, rename = "method")]
  pub fn method(&self) -> String {
    self.method.clone()
  }
  #[qjs(get, rename = "redirect")]
  pub fn redirect(&self) -> String {
    self.redirect.clone()
  }
  #[qjs(get, rename = "credentials")]
  pub fn credentials(&self) -> String {
    self.credentials.clone()
  }
  #[qjs(get, rename = "bodyUsed")]
  pub fn body_used(&self) -> bool {
    self.body_used
  }
  #[qjs(get, rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, HeadersJs>> {
    Class::instance(ctx, HeadersJs::from_pairs(self.headers.iter().cloned()))
  }

  #[qjs(rename = "text")]
  pub fn text(&mut self, ctx: Ctx<'_>) -> rquickjs::Result<String> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(&ctx, "Body has already been consumed"));
    }
    self.body_used = true;
    Ok(String::from_utf8_lossy(&std::mem::take(&mut self.body)).into_owned())
  }

  #[qjs(rename = "json")]
  pub fn json<'js>(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(&ctx, "Body has already been consumed"));
    }
    self.body_used = true;
    let v: serde_json::Value = serde_json::from_slice(&std::mem::take(&mut self.body))
      .map_err(|e| rquickjs::Error::new_from_js_message("Request.json", "Error", e.to_string()))?;
    json_to_js(&ctx, &v)
  }

  #[qjs(rename = "clone")]
  pub fn clone_(&self, ctx: Ctx<'_>) -> rquickjs::Result<Self> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(&ctx, "Cannot clone a used Request"));
    }
    Ok(Self {
      url: self.url.clone(),
      method: self.method.clone(),
      headers: self.headers.clone(),
      body: self.body.clone(),
      redirect: self.redirect.clone(),
      credentials: self.credentials.clone(),
      body_used: false,
    })
  }
}

/// Install `globalThis.fetch`, bound to `cx` (the session's HTTP
/// context — same one the `request` binding wraps). Net policy that
/// applies to `request` applies here because it is the same core.
pub fn install(ctx: &Ctx<'_>, cx: Arc<HttpClient>) -> rquickjs::Result<()> {
  // Forward into a generic fn so `Ctx`/`Value`/return share one `'js`
  // (an inline closure gives each arg its own lifetime and the returned
  // promise Value cannot be proven to outlive them) — same pattern as
  // the plugin dispatch closure.
  let f = rquickjs::Function::new(ctx.clone(), move |ctx, input, init| {
    do_fetch(ctx, input, init, cx.clone())
  })?;
  ctx.globals().set("fetch", f)?;
  Ok(())
}

fn do_fetch<'js>(
  ctx: Ctx<'js>,
  input: Value<'js>,
  init: Opt<Object<'js>>,
  cx: Arc<HttpClient>,
) -> rquickjs::Result<Value<'js>> {
  {
    // `input` may be a URL string, a `Request` instance, or an object
    // with a `url`. A `Request` seeds method/headers/body/redirect; the
    // `init` bag overrides each.
    let req = Class::<FetchRequestJs>::from_value(&input).ok();
    let url = req
      .as_ref()
      .map(|r| r.borrow().url.clone())
      .or_else(|| input.as_string().and_then(|s| s.to_string().ok()))
      .or_else(|| input.as_object().and_then(|o| o.get::<_, String>("url").ok()))
      .unwrap_or_default();
    // Snapshot the net policy NOW (synchronously, while this `fetch()`
    // call is still on the calling tool's stack) so the allow-list
    // checked below is the caller's, not whatever runs by the time the
    // request future is polled.
    let net = active_net(&ctx);
    let init = init.0;
    let method = init
      .as_ref()
      .and_then(|o| o.get::<_, String>("method").ok())
      .or_else(|| req.as_ref().map(|r| r.borrow().method.clone()));
    let mut headers_vec: Vec<(String, String)> = init
      .as_ref()
      .and_then(|o| o.get::<_, Value<'_>>("headers").ok())
      .map(|v| header_pairs_from(&v))
      .or_else(|| req.as_ref().map(|r| r.borrow().headers.clone()))
      .unwrap_or_default();
    // body: string -> raw; `Blob` -> bytes (+ its type); `FormData` ->
    // multipart (content-type MUST be the boundary one); other object
    // -> JSON; else a Request's own body. `body_ct` is the content-type
    // the body implies (FormData overrides, Blob only fills if absent).
    let body_val = init.as_ref().and_then(|o| o.get::<_, Value<'_>>("body").ok());
    let (data, json_data, body_ct, force_ct) = if let Some(b) = &body_val {
      if let Some(s) = b.as_string().and_then(|s| s.to_string().ok()) {
        (Some(s.into_bytes()), None, None, false)
      } else if let Ok(fd) = Class::<crate::bindings::form_data::FormDataJs>::from_value(b) {
        let (bytes, ct) = fd.borrow().to_multipart();
        (Some(bytes), None, Some(ct), true)
      } else if let Some((bytes, ct)) = crate::bindings::blob::BlobJs::from_js_blob(b) {
        (Some(bytes), None, (!ct.is_empty()).then_some(ct), false)
      } else if b.is_object() {
        let j: Option<serde_json::Value> = crate::bindings::convert::serde_from_js(&ctx, b.clone()).ok();
        (None, j, None, false)
      } else {
        (None, None, None, false)
      }
    } else {
      match req.as_ref().map(|r| r.borrow().body.clone()) {
        Some(b) if !b.is_empty() => (Some(b), None, None, false),
        _ => (None, None, None, false),
      }
    };
    if let Some(ct) = body_ct {
      let has_ct = headers_vec.iter().any(|(k, _)| k == "content-type");
      if force_ct {
        headers_vec.retain(|(k, _)| k != "content-type");
        headers_vec.push(("content-type".to_string(), ct));
      } else if !has_ct {
        headers_vec.push(("content-type".to_string(), ct));
      }
    }
    let headers = (!headers_vec.is_empty()).then_some(headers_vec);
    // `init.redirect` (or the Request's) maps onto the per-request
    // redirect cap: "follow" (default) keeps the client default;
    // "manual"/"error" pin 0 so a 3xx is returned rather than followed.
    // (A spec-exact "manual" opaque-redirect / "error" rejection is not
    // distinguishable through reqwest's per-request policy; the 3xx is
    // surfaced instead. Documented subset.)
    let redirect = init
      .as_ref()
      .and_then(|o| o.get::<_, String>("redirect").ok())
      .or_else(|| req.as_ref().map(|r| r.borrow().redirect.clone()));
    let max_redirects = match redirect.as_deref() {
      Some("manual" | "error") => Some(0),
      _ => None,
    };
    // `init.signal` (an `AbortSignal`): grab its native channel so the
    // request future can be dropped when it aborts.
    let signal = init
      .as_ref()
      .and_then(|o| o.get::<_, Value<'_>>("signal").ok())
      .and_then(|v| Class::<crate::bindings::abort::AbortSignalJs<'js>>::from_value(&v).ok())
      .map(|s| crate::bindings::abort::AbortSignalJs::inner_of(&s));
    let promised = rquickjs::promise::Promised::from(async move {
      if let Some(list) = net.as_deref()
        && let Err(msg) = net_check(list, &url)
      {
        return Err(rquickjs::Error::new_from_js_message("fetch", "Error", msg));
      }
      let opts = RequestOptions {
        method,
        headers,
        data,
        json_data,
        max_redirects,
        ..Default::default()
      };
      if let Some(sig) = &signal
        && sig.is_aborted()
      {
        return Err(rquickjs::Error::new_from_js_message(
          "fetch",
          "AbortError",
          sig.reason_message(),
        ));
      }
      // Streamed: status/headers resolve here, the body is pulled
      // incrementally later (via Response.body / text() / json()).
      let fut = cx.fetch_stream(&url, Some(opts));
      let resp = match &signal {
        Some(sig) => {
          tokio::select! {
            r = fut => r.map_err(|e| rquickjs::Error::new_from_js_message("fetch", "Error", e.to_string()))?,
            () = sig.aborted() => {
              return Err(rquickjs::Error::new_from_js_message("fetch", "AbortError", sig.reason_message()));
            }
          }
        },
        None => fut
          .await
          .map_err(|e| rquickjs::Error::new_from_js_message("fetch", "Error", e.to_string()))?,
      };
      let final_url = resp.url().to_string();
      // Best-effort: a differing final URL means at least one hop was
      // followed (the core does not yet expose a redirect count).
      let redirected = !final_url.is_empty() && final_url != url;
      let out = FetchResponseJs::from_stream(
        resp.status(),
        resp.status_text().to_string(),
        final_url,
        resp.headers().to_vec(),
        redirected,
        resp,
      );
      Ok::<_, rquickjs::Error>(out)
    });
    promised.into_js(&ctx)
  }
}
