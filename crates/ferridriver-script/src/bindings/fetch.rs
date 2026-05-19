//! A WHATWG-ish `fetch` + `Headers` + `Response`, so npm packages that
//! expect `fetch` work. It is a thin surface over the SAME
//! `ferridriver::http_client` core the Playwright-style `request`
//! binding uses — one HTTP stack, one place the net policy applies. The
//! ergonomic `request` API stays; this just adds the standard entry
//! point.
//!
//! `Headers` is WHATWG-spec (lowercased + RFC7230-validated names,
//! value normalization, `, ` combine, separate `set-cookie` +
//! `getSetCookie`, sorted real iterators, `forEach`). The response is
//! still a subset (`text()`/`json()`/`arrayBuffer()`), and its class is
//! `FetchResponse` for now (the standard constructible global
//! `Response` lands with the network-class de-globalisation in a
//! follow-up); no streaming / `Blob` / `FormData` / `AbortController`
//! yet.
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

#[derive(Trace)]
#[rquickjs::class(rename = "FetchResponse")]
pub struct FetchResponseJs {
  #[qjs(skip_trace)]
  status: u16,
  #[qjs(skip_trace)]
  ok: bool,
  #[qjs(skip_trace)]
  status_text: String,
  #[qjs(skip_trace)]
  url: String,
  #[qjs(skip_trace)]
  headers: Vec<(String, String)>,
  #[qjs(skip_trace)]
  body: Vec<u8>,
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

#[rquickjs::methods]
impl FetchResponseJs {
  #[qjs(get, rename = "status")]
  pub fn status(&self) -> u16 {
    self.status
  }
  #[qjs(get, rename = "ok")]
  pub fn ok(&self) -> bool {
    self.ok
  }
  #[qjs(get, rename = "statusText")]
  pub fn status_text(&self) -> String {
    self.status_text.clone()
  }
  #[qjs(get, rename = "url")]
  pub fn url(&self) -> String {
    self.url.clone()
  }

  #[qjs(get, rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, HeadersJs>> {
    Class::instance(ctx, HeadersJs::from_pairs(self.headers.iter().cloned()))
  }

  #[qjs(rename = "text")]
  pub fn text(&self) -> String {
    String::from_utf8_lossy(&self.body).into_owned()
  }

  #[qjs(rename = "json")]
  pub fn json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let v: serde_json::Value = serde_json::from_slice(&self.body)
      .map_err(|e| rquickjs::Error::new_from_js_message("Response.json", "Error", e.to_string()))?;
    json_to_js(&ctx, &v)
  }

  #[qjs(rename = "arrayBuffer")]
  pub fn array_buffer<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    rquickjs::ArrayBuffer::new(ctx.clone(), self.body.clone()).map(rquickjs::ArrayBuffer::into_value)
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
    let url = input
      .as_string()
      .and_then(|s| s.to_string().ok())
      .or_else(|| input.as_object().and_then(|o| o.get::<_, String>("url").ok()))
      .unwrap_or_default();
    // Snapshot the net policy NOW (synchronously, while this `fetch()`
    // call is still on the calling tool's stack) so the allow-list
    // checked below is the caller's, not whatever runs by the time the
    // request future is polled.
    let net = active_net(&ctx);
    let init = init.0;
    let method = init.as_ref().and_then(|o| o.get::<_, String>("method").ok());
    let headers = init
      .as_ref()
      .and_then(|o| o.get::<_, Value<'_>>("headers").ok())
      .map(|v| header_pairs_from(&v));
    // body: string -> raw; object -> JSON (+ content-type unless set).
    let (data, json_data) = match init.as_ref().and_then(|o| o.get::<_, Value<'_>>("body").ok()) {
      Some(b) if b.is_string() => (
        b.as_string().and_then(|s| s.to_string().ok()).map(String::into_bytes),
        None,
      ),
      Some(b) if b.is_object() => {
        let j: Option<serde_json::Value> = crate::bindings::convert::serde_from_js(&ctx, b).ok();
        (None, j)
      },
      _ => (None, None),
    };
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
        ..Default::default()
      };
      let resp = cx
        .fetch(&url, Some(opts))
        .await
        .map_err(|e| rquickjs::Error::new_from_js_message("fetch", "Error", e.to_string()))?;
      let out = FetchResponseJs {
        status: resp.status(),
        ok: resp.ok(),
        status_text: resp.status_text().to_string(),
        url: resp.url().to_string(),
        headers: resp.headers().to_vec(),
        body: resp.text().map(String::into_bytes).unwrap_or_default(),
      };
      Ok::<_, rquickjs::Error>(out)
    });
    promised.into_js(&ctx)
  }
}
