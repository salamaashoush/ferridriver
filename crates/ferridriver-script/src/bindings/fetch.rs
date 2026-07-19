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
//! drops the request future. `Response.body` is a WHATWG
//! `ReadableStream` that pulls chunks live off the socket (the body is
//! NOT buffered) — the same stream object on every access, drained by
//! `text()`/`json()`/`arrayBuffer()`, and pipeable through a
//! `TransformStream` into a `WritableStream`. `clone()` tees it, so both
//! responses read the full payload off one socket even when nothing has
//! been buffered yet. See [`super::streams`]. `Blob` and `FormData` (see
//! [`super::blob`] / [`super::form_data`]) are accepted as bodies — a
//! `Blob` sends its bytes + type, a `FormData` is serialized as
//! `multipart/form-data`.
//!
//! Net policy: `fetch` is a facade over the SAME core a net-restricted
//! tool's `request` wraps, so the `allow.net` allow-list must bind here
//! too — otherwise a tool restricted to host X could reach anywhere via
//! the global `fetch`. The per-tool allow-list lives in `NetPolicyUd`
//! (VM userdata); `extensions::dispatch_tool` brackets each handler poll so
//! the policy in effect is whichever tool's continuation is running, and
//! `fetch` snapshots it synchronously at call time (before any I/O).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferridriver::http_client::{Credentials, HttpClient, RedirectMode, RequestOptions};
use ferridriver_jsstd::abort::AbortSignal;
use ferridriver_jsstd::stream_web::ReadableStream;
use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Func, Opt, This};
use rquickjs::{Coerced, Ctx, IntoJs, Object, Value, class::Class, class::Trace};

use crate::bindings::blob::BlobJs;
use crate::bindings::convert::json_to_js;
use crate::bindings::form_data::FormDataJs;
use crate::bindings::http_client::net_check;

/// Hard cap on a single buffered `fetch` body (`text`/`json`/
/// `arrayBuffer`). QuickJS's `memory_limit` only bounds the JS heap;
/// the drained body is a Rust allocation, so without this a script
/// reading an unbounded/huge response could exhaust host memory well
/// past the JS quota. Streaming via `Response.body` is unaffected.
const MAX_FETCH_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Wall-clock bound on draining one streamed body, so a slow-loris /
/// never-ending response cannot pin a session forever (the per-script
/// interrupt-handler timeout does not fire during a native await).
const FETCH_BODY_DRAIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-VM carrier of the *currently active* tool net allow-list. `None`
/// (the resting state, and what the top-level script sees) means
/// unrestricted; `Some(list)` means default-deny against `list`.
///
/// One cell per session VM, stored as rquickjs userdata at
/// [`crate::engine::Session::create`]. `extensions::dispatch_tool` swaps the
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

/// The session's policy cell, if the VM has one installed.
pub(crate) fn policy_cell(ctx: &Ctx<'_>) -> Option<NetPolicy> {
  ctx.userdata::<NetPolicyUd>().map(|u| u.0.clone())
}

/// Run `f` with `net` installed as the active allow-list, restoring the
/// caller's policy after. For synchronous callback invocations (timers,
/// event listeners, route handlers): a callback registered by a
/// net-restricted tool keeps that tool's grant when it later fires from
/// a pump or job, instead of falling back to the unrestricted resting
/// state.
pub(crate) fn call_with_net<R>(ctx: &Ctx<'_>, net: Option<&Arc<[String]>>, f: impl FnOnce() -> R) -> R {
  let Some(cell) = policy_cell(ctx) else {
    return f();
  };
  let prev = cell.swap(net.cloned());
  let r = f();
  cell.swap(prev);
  r
}

/// Poll-bracket `fut` with `net` active: the cell holds `net` whenever
/// `fut`'s continuation runs and is restored to the caller's value
/// otherwise — correct under nesting and concurrent interleaving because
/// the swap and the synchronous `fetch`/`request` guards both run within
/// a single poll on the single QuickJS thread. The async analogue of
/// [`call_with_net`]; `extensions::dispatch_tool` and awaited callback
/// dispatches share this one implementation.
pub(crate) async fn bracket_net<F: std::future::Future>(
  cell: Option<NetPolicy>,
  net: Option<Arc<[String]>>,
  fut: F,
) -> F::Output {
  match cell {
    None => fut.await,
    Some(cell) => {
      let mut fut = std::pin::pin!(fut);
      std::future::poll_fn(move |cx| {
        let prev = cell.swap(net.clone());
        let r = fut.as_mut().poll(cx);
        cell.swap(prev);
        r
      })
      .await
    },
  }
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
pub struct FetchResponseJs<'js> {
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
  net: Option<crate::bindings::streams::NetBody>,
  /// The `Response.body` stream, created on first access (or by
  /// `clone()`, which tees it). Once present it is the authoritative
  /// body: `text()`/`json()`/`arrayBuffer()` drain it rather than the
  /// raw source, so a tee'd branch reads its own copy.
  body_stream: Option<Class<'js, ReadableStream<'js>>>,
}

/// WHATWG `Request` (spec subset). Constructible (`new Request(input,
/// init?)` where `input` is a URL string or another `Request`), with
/// `url`/`method`/`headers`/`redirect`/`credentials`/`bodyUsed`
/// accessors and `text`/`json`/`arrayBuffer`/`clone`. A `signal` passed
/// in `init` is carried (as the native abort channel) and forwarded by
/// `fetch(request)`; `fetch` reads `url`/`method`/`headers`/`body`/
/// `redirect`/`credentials`/`signal` off a `Request` argument.
#[derive(Trace)]
#[rquickjs::class(rename = "Request")]
pub struct FetchRequestJs<'js> {
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
  /// Spec attributes ferridriver does not act on but must round-trip:
  /// reading them back off a `Request` (or a `clone()`) has to return
  /// what the caller passed in `init`.
  #[qjs(skip_trace)]
  cache: String,
  #[qjs(skip_trace)]
  mode: String,
  #[qjs(skip_trace)]
  referrer: String,
  #[qjs(skip_trace)]
  referrer_policy: String,
  #[qjs(skip_trace)]
  integrity: String,
  #[qjs(skip_trace)]
  keepalive: bool,
  #[qjs(skip_trace)]
  destination: String,
  /// The native abort channel of a `signal` passed in `init`, kept so
  /// `fetch(request)` can drop the in-flight future on abort. Native
  /// (`Arc<AbortInner>`), never a captured JS value — GC-safe.
  #[qjs(skip_trace)]
  signal_inner: Option<Arc<crate::bindings::abort::AbortInner>>,
  /// The `signal` exactly as handed to the constructor, so the `signal`
  /// getter returns the caller's own `AbortSignal` object rather than a
  /// fresh one built from the native channel.
  signal: Option<Class<'js, AbortSignal<'js>>>,
  /// The `Request.body` stream, created on first access. Once present it
  /// is the authoritative body and the body readers drain it, mirroring
  /// `Response.body`.
  body_stream: Option<Class<'js, ReadableStream<'js>>>,
}

// SAFETY: only owned `'static` data.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for HeadersJs {
  type Changed<'to> = HeadersJs;
}
#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for FetchResponseJs<'js> {
  type Changed<'to> = FetchResponseJs<'to>;
}
#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for FetchRequestJs<'js> {
  type Changed<'to> = FetchRequestJs<'to>;
}

/// Extract a request/response body from a JS value, returning the bytes
/// and the default `content-type` the body type implies (string ->
/// `text/plain;charset=UTF-8`, object -> JSON; `Headers`/null/undefined
/// -> none). Caller applies the content-type only if not already set.
fn extract_body<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> (Vec<u8>, Option<String>) {
  if v.is_undefined() || v.is_null() {
    return (Vec::new(), None);
  }
  if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
    return (s.into_bytes(), Some("text/plain;charset=UTF-8".to_string()));
  }
  // A `FormData` body is multipart with a generated boundary, and a
  // `Blob`/`File` body is its raw bytes typed by its own `type` — both
  // would otherwise fall through to the JSON branch and serialize as
  // `{}`.
  if let Ok(fd) = Class::<crate::bindings::form_data::FormDataJs>::from_value(v) {
    let (bytes, ct) = fd.borrow().to_multipart();
    return (bytes, Some(ct));
  }
  if let Some((bytes, ct)) = crate::bindings::blob::BlobJs::from_js_blob(v) {
    return (bytes, (!ct.is_empty()).then_some(ct));
  }
  if let Some(ta) = rquickjs::TypedArray::<u8>::from_value(v.clone())
    .ok()
    .and_then(|ta| ta.as_bytes().map(<[u8]>::to_vec))
  {
    return (ta, None);
  }
  if let Some(bytes) = rquickjs::ArrayBuffer::from_value(v.clone()).and_then(|ab| ab.as_bytes().map(<[u8]>::to_vec)) {
    return (bytes, None);
  }
  if v.is_object() {
    if let Ok(j) = crate::bindings::convert::serde_from_js::<serde_json::Value>(ctx, v.clone()) {
      return (j.to_string().into_bytes(), Some("application/json".to_string()));
    }
  }
  (Vec::new(), None)
}

/// Parse a `Response`/`Request` `init` bag's `headers` into raw pairs
/// and apply `default_ct` as `content-type` unless already present.
fn init_headers(init: Option<&Object<'_>>, default_ct: Option<String>) -> Vec<(String, String)> {
  let mut pairs = init
    .and_then(|o| o.get::<_, Value<'_>>("headers").ok())
    .map(|v| header_pairs_from(&v))
    .unwrap_or_default();
  if let Some(ct) = default_ct
    && !pairs.iter().any(|(k, _)| k == "content-type")
  {
    pairs.push(("content-type".to_string(), ct));
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
      // WHATWG "Headers append": every non-`set-cookie` repeat combines
      // with `, ` (0x2C 0x20). There is no per-name separator in the
      // spec — the old `; ` for `cookie` was a non-standard deviation.
      self.pairs[i].1 = format!("{}, {value}", self.pairs[i].1);
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

impl<'js> FetchResponseJs<'js> {
  /// The `Response` a `fetch()` resolves to: status/headers are known,
  /// the body streams from `stream` (not buffered).
  fn from_stream(
    status: u16,
    status_text: String,
    url: String,
    headers: Vec<(String, String)>,
    redirected: bool,
    type_: &'static str,
    stream: ferridriver::http_client::HttpStreamResponse,
  ) -> Self {
    Self {
      status,
      status_text,
      url,
      headers,
      body: Vec::new(),
      redirected,
      type_,
      body_used: false,
      net: Some(Arc::new(tokio::sync::Mutex::new(Some(stream)))),
      body_stream: None,
    }
  }

  /// A WHATWG opaque-redirect filtered response (`redirect: manual` on a
  /// 3xx): type "opaqueredirect", status 0, empty headers, null body.
  fn opaque_redirect() -> Self {
    Self {
      status: 0,
      status_text: String::new(),
      url: String::new(),
      headers: Vec::new(),
      body: Vec::new(),
      redirected: false,
      type_: "opaqueredirect",
      body_used: false,
      net: None,
      body_stream: None,
    }
  }

  /// The `Response.body` stream, created on first access.
  ///
  /// A streamed `fetch` result shares the live response with the stream
  /// (so neither buffers it); anything else streams the in-memory bytes.
  fn ensure_body_stream(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStream<'js>>> {
    if let Some(s) = &self.body_stream {
      return Ok(s.clone());
    }
    let stream = match self.net.take() {
      Some(net) => crate::bindings::streams::from_net(ctx, net)?,
      None => crate::bindings::streams::from_bytes(ctx, std::mem::take(&mut self.body))?,
    };
    self.body_stream = Some(stream.clone());
    Ok(stream)
  }

  /// Read a `ReadableStream` to completion through its public JS reader,
  /// so a tee'd branch, a user-constructed stream and a live socket all
  /// drain the same way.
  async fn drain_stream(ctx: &Ctx<'js>, stream: Class<'js, ReadableStream<'js>>) -> rquickjs::Result<Vec<u8>> {
    let obj = stream
      .into_value()
      .into_object()
      .ok_or_else(|| rquickjs::Error::new_from_js_message("Response", "TypeError", "body is not a ReadableStream"))?;
    let reader: Object<'js> = obj.get::<_, rquickjs::Function<'js>>("getReader")?.call((This(obj),))?;
    let read: rquickjs::Function<'js> = reader.get("read")?;
    let mut out = Vec::new();
    loop {
      let step: rquickjs::Promise<'js> = read.call((This(reader.clone()),))?;
      let res: Object<'js> = step.into_future().await?;
      if res.get::<_, bool>("done").unwrap_or(false) {
        return Ok(out);
      }
      let chunk = chunk_bytes(&res.get::<_, Value<'js>>("value")?);
      if out.len() + chunk.len() > MAX_FETCH_BODY_BYTES {
        return Err(rquickjs::Exception::throw_type(
          ctx,
          &format!("response body exceeded {MAX_FETCH_BODY_BYTES} bytes"),
        ));
      }
      out.extend_from_slice(&chunk);
    }
  }

  /// WHATWG "consume body": a second read is a `TypeError`. Drains the
  /// body stream when one has been vended (`.body`, or a `clone()` tee),
  /// else the live response, else the in-memory bytes.
  async fn consume(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Vec<u8>> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(ctx, "Body has already been consumed"));
    }
    if let Some(stream) = self.body_stream.clone() {
      if stream.borrow().is_readable_stream_locked() {
        return Err(rquickjs::Exception::throw_type(ctx, "Body is locked to a reader"));
      }
      self.body_used = true;
      return match tokio::time::timeout(FETCH_BODY_DRAIN_TIMEOUT, Self::drain_stream(ctx, stream)).await {
        Ok(r) => r,
        Err(_) => Err(rquickjs::Exception::throw_type(ctx, "response body read timed out")),
      };
    }
    self.body_used = true;
    if let Some(net) = &self.net {
      let mut guard = net.lock().await;
      let mut out = Vec::new();
      if let Some(resp) = guard.as_mut() {
        let drained = tokio::time::timeout(FETCH_BODY_DRAIN_TIMEOUT, async {
          while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            if out.len() + chunk.len() > MAX_FETCH_BODY_BYTES {
              return Err(format!("response body exceeded {MAX_FETCH_BODY_BYTES} bytes"));
            }
            out.extend_from_slice(&chunk);
          }
          Ok::<(), String>(())
        })
        .await;
        // Free the socket regardless of outcome so a rejected read does
        // not leave the connection (and its buffers) pinned.
        *guard = None;
        match drained {
          Ok(Ok(())) => {},
          Ok(Err(msg)) => return Err(rquickjs::Exception::throw_type(ctx, &msg)),
          Err(_) => {
            return Err(rquickjs::Exception::throw_type(ctx, "response body read timed out"));
          },
        }
        return Ok(out);
      }
      *guard = None;
      return Ok(out);
    }
    Ok(std::mem::take(&mut self.body))
  }
}

impl<'js> BodyMixin<'js> for FetchResponseJs<'js> {
  async fn consume_body(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Vec<u8>> {
    self.consume(ctx).await
  }

  fn content_type(&self) -> Option<String> {
    header_value(&self.headers, "content-type")
  }
}

impl<'js> BodyMixin<'js> for FetchRequestJs<'js> {
  async fn consume_body(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Vec<u8>> {
    self.consume(ctx).await
  }

  fn content_type(&self) -> Option<String> {
    header_value(&self.headers, "content-type")
  }
}

/// Case-insensitive lookup over a raw header pair list.
fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
  headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case(name))
    .map(|(_, v)| v.clone())
}

/// The WHATWG `Body` mixin, shared verbatim by `Request` and
/// `Response` (spec: both objects expose the same readers over the same
/// "consume body" step). Implementors supply only how to take the bytes
/// and what content type describes them; every reader below is defined
/// once here.
///
/// `#[rquickjs::methods]` cannot see methods that come from a trait (or
/// a macro), so each class still registers six one-line delegators — but
/// no reader logic is duplicated.
pub(crate) trait BodyMixin<'js> {
  /// WHATWG "consume body": yields the bytes, marks the body used, and
  /// fails on a second read.
  fn consume_body(&mut self, ctx: &Ctx<'js>) -> impl Future<Output = rquickjs::Result<Vec<u8>>>;

  /// The `content-type` header value, which types the `Blob` from
  /// `blob()` and selects the `formData()` parser.
  fn content_type(&self) -> Option<String>;

  async fn mixin_text(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<String> {
    let b = self.consume_body(ctx).await?;
    Ok(String::from_utf8_lossy(&b).into_owned())
  }

  async fn mixin_json(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let b = self.consume_body(ctx).await?;
    let v: serde_json::Value =
      serde_json::from_slice(&b).map_err(|e| rquickjs::Error::new_from_js_message("json", "Error", e.to_string()))?;
    json_to_js(ctx, &v)
  }

  async fn mixin_array_buffer(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let b = self.consume_body(ctx).await?;
    rquickjs::ArrayBuffer::new(ctx.clone(), b).map(rquickjs::ArrayBuffer::into_value)
  }

  /// Spec: `bytes()` resolves with a `Uint8Array` (not an `ArrayBuffer`).
  async fn mixin_bytes(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let b = self.consume_body(ctx).await?;
    Ok(rquickjs::TypedArray::new(ctx.clone(), b)?.into_value())
  }

  /// Spec: the blob's `type` is the body's content type, or `""`.
  async fn mixin_blob(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let mime = self.content_type().unwrap_or_default();
    let b = self.consume_body(ctx).await?;
    Ok(Class::instance(ctx.clone(), BlobJs::new_parts(b, mime))?.into_value())
  }

  /// Spec: parses `multipart/form-data` and
  /// `application/x-www-form-urlencoded`; any other type is a
  /// `TypeError`.
  async fn mixin_form_data(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let content_type = self.content_type().unwrap_or_default();
    let boundary = ferridriver::http_client::multipart_boundary_of(&content_type);
    let urlencoded = content_type
      .split(';')
      .next()
      .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"));
    if boundary.is_none() && !urlencoded {
      return Err(rquickjs::Exception::throw_type(
        ctx,
        &format!("Could not parse content as FormData: unsupported content type {content_type:?}"),
      ));
    }

    let bytes = self.consume_body(ctx).await?;
    let form = match boundary {
      Some(boundary) => {
        FormDataJs::from_multipart_fields(&ferridriver::http_client::parse_multipart(&bytes, &boundary))
      },
      None => FormDataJs::from_urlencoded(&String::from_utf8_lossy(&bytes)),
    };
    Ok(Class::instance(ctx.clone(), form)?.into_value())
  }
}

/// Bytes behind a stream chunk (`Uint8Array`/`ArrayBuffer`/string).
fn chunk_bytes(v: &Value<'_>) -> Vec<u8> {
  if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
    return s.into_bytes();
  }
  if let Ok(ta) = rquickjs::TypedArray::<u8>::from_value(v.clone()) {
    let b: &[u8] = ta.as_ref();
    return b.to_vec();
  }
  if let Some(ab) = rquickjs::ArrayBuffer::from_value(v.clone())
    && let Some(b) = ab.as_bytes()
  {
    return b.to_vec();
  }
  Vec::new()
}

#[rquickjs::methods]
impl<'js> FetchResponseJs<'js> {
  /// `new Response(body?, init?)` — `init`: `{ status?, statusText?,
  /// headers? }`. `status` outside 200..=599 is a `RangeError`.
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'js>, body: Opt<Value<'js>>, init: Opt<Object<'js>>) -> rquickjs::Result<Self> {
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
    // WHATWG: a null-body status (204/205/304) with a non-null body is
    // a `TypeError`.
    let has_body = body.0.as_ref().is_some_and(|v| !v.is_null() && !v.is_undefined());
    if has_body && matches!(status, 204 | 205 | 304) {
      return Err(rquickjs::Exception::throw_type(
        &ctx,
        "Failed to construct 'Response': Response with null body status cannot have body",
      ));
    }
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
      body_stream: None,
    })
  }

  /// `Response.json(data, init?)` — JSON body + `application/json`.
  #[qjs(static, rename = "json")]
  pub fn json_static(ctx: Ctx<'js>, data: Value<'js>, init: Opt<Object<'js>>) -> rquickjs::Result<Self> {
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
      headers: init_headers(init.as_ref(), Some("application/json".to_string())),
      body: json.to_string().into_bytes(),
      redirected: false,
      type_: "default",
      body_used: false,
      net: None,
      body_stream: None,
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
      body_stream: None,
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
      body_stream: None,
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
  pub fn headers(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, HeadersJs>> {
    Class::instance(ctx, HeadersJs::from_pairs(self.headers.iter().cloned()))
  }

  /// `Response.body` — the body `ReadableStream`. For a streamed
  /// `fetch` result each pull takes the next chunk off the socket (the
  /// body is NOT buffered); for a constructed `Response` it streams the
  /// in-memory bytes. The same stream object is returned every time, and
  /// `text()`/`json()`/`arrayBuffer()` drain it.
  #[qjs(get, rename = "body")]
  pub fn body(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStream<'js>>> {
    self.ensure_body_stream(&ctx)
  }

  #[qjs(rename = "text")]
  pub async fn text(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<String> {
    self.mixin_text(&ctx).await
  }

  #[qjs(rename = "json")]
  pub async fn json(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_json(&ctx).await
  }

  #[qjs(rename = "arrayBuffer")]
  pub async fn array_buffer(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_array_buffer(&ctx).await
  }

  #[qjs(rename = "bytes")]
  pub async fn bytes(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_bytes(&ctx).await
  }

  #[qjs(rename = "blob")]
  pub async fn blob(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_blob(&ctx).await
  }

  #[qjs(rename = "formData")]
  pub async fn form_data(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_form_data(&ctx).await
  }

  /// `Response.clone()` — WHATWG "clone a response": the body stream is
  /// tee'd, so BOTH responses can be read independently, including an
  /// unread streamed `fetch` body (neither branch buffers ahead of its
  /// own consumer).
  #[qjs(rename = "clone")]
  pub fn clone_(this: This<Class<'js, Self>>, ctx: Ctx<'js>) -> rquickjs::Result<Self> {
    let (branch1, branch2) = {
      let mut me = this.borrow_mut();
      if me.body_used {
        return Err(rquickjs::Exception::throw_type(&ctx, "Cannot clone a used Response"));
      }
      let stream = me.ensure_body_stream(&ctx)?;
      drop(me);
      ferridriver_jsstd::stream_web::tee_readable_stream(ctx.clone(), stream)?
    };
    let mut me = this.borrow_mut();
    me.body_stream = Some(branch1);
    Ok(Self {
      status: me.status,
      status_text: me.status_text.clone(),
      url: me.url.clone(),
      headers: me.headers.clone(),
      body: Vec::new(),
      redirected: me.redirected,
      type_: me.type_,
      body_used: false,
      net: None,
      body_stream: Some(branch2),
    })
  }
}

impl<'js> FetchRequestJs<'js> {
  /// The `Request.body` stream, created on first access — over the
  /// in-memory bytes, since a `Request` is never backed by a live
  /// socket.
  ///
  /// Unlike `Response`, the bytes are COPIED rather than moved into the
  /// stream: `fetch(request)` reads them straight off the `Request`, so
  /// moving them would make merely touching `.body` send an empty body.
  /// Single-use semantics are unaffected — `consume()` prefers the
  /// stream once one exists, and `fetch()` refuses a disturbed body.
  fn ensure_body_stream(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStream<'js>>> {
    if let Some(s) = &self.body_stream {
      return Ok(s.clone());
    }
    let stream = crate::bindings::streams::from_bytes(ctx, self.body.clone())?;
    self.body_stream = Some(stream.clone());
    Ok(stream)
  }

  /// Whether the body has been read — consumed through a reader, or
  /// drained/locked through the `.body` stream. Spec: fetching a
  /// Request with a disturbed body is a `TypeError`.
  fn body_is_disturbed(&self) -> bool {
    if self.body_used {
      return true;
    }
    self.body_stream.as_ref().is_some_and(|s| {
      let s = s.borrow();
      s.disturbed || s.is_readable_stream_locked()
    })
  }

  /// WHATWG "consume body": a second read is a `TypeError`. Drains the
  /// body stream when one has been vended (`.body`), else the bytes.
  async fn consume(&mut self, ctx: &Ctx<'js>) -> rquickjs::Result<Vec<u8>> {
    if self.body_used {
      return Err(rquickjs::Exception::throw_type(ctx, "Body has already been consumed"));
    }
    if let Some(stream) = self.body_stream.clone() {
      if stream.borrow().is_readable_stream_locked() {
        return Err(rquickjs::Exception::throw_type(ctx, "Body is locked to a reader"));
      }
      self.body_used = true;
      return match tokio::time::timeout(FETCH_BODY_DRAIN_TIMEOUT, FetchResponseJs::drain_stream(ctx, stream)).await {
        Ok(r) => r,
        Err(_) => Err(rquickjs::Exception::throw_type(ctx, "request body read timed out")),
      };
    }
    self.body_used = true;
    Ok(std::mem::take(&mut self.body))
  }
}

#[rquickjs::methods]
impl<'js> FetchRequestJs<'js> {
  /// `new Request(input, init?)` — `input` is a URL string or another
  /// `Request`; `init`: `{ method?, headers?, body?, redirect?,
  /// credentials?, signal?, cache?, mode?, referrer?, referrerPolicy?,
  /// integrity?, keepalive? }`.
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'js>, input: Value<'js>, init: Opt<Object<'js>>) -> rquickjs::Result<Self> {
    let init = init.0;
    let mut req = if let Ok(other) = Class::<FetchRequestJs<'js>>::from_value(&input) {
      let o = other.borrow();
      Self {
        url: o.url.clone(),
        method: o.method.clone(),
        headers: o.headers.clone(),
        body: o.body.clone(),
        redirect: o.redirect.clone(),
        credentials: o.credentials.clone(),
        body_used: false,
        cache: o.cache.clone(),
        mode: o.mode.clone(),
        referrer: o.referrer.clone(),
        referrer_policy: o.referrer_policy.clone(),
        integrity: o.integrity.clone(),
        keepalive: o.keepalive,
        destination: o.destination.clone(),
        signal_inner: o.signal_inner.clone(),
        signal: o.signal.clone(),
        body_stream: None,
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
        cache: "default".to_string(),
        mode: "cors".to_string(),
        referrer: "about:client".to_string(),
        referrer_policy: String::new(),
        integrity: String::new(),
        keepalive: false,
        // Spec: a Request built by script has an empty destination;
        // only the fetch a browser initiates for a specific consumer
        // (script/image/...) carries one.
        destination: String::new(),
        signal_inner: None,
        signal: None,
        body_stream: None,
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
      if let Ok(v) = o.get::<_, String>("cache") {
        req.cache = v;
      }
      if let Ok(v) = o.get::<_, String>("mode") {
        req.mode = v;
      }
      if let Ok(v) = o.get::<_, String>("referrer") {
        req.referrer = v;
      }
      if let Ok(v) = o.get::<_, String>("referrerPolicy") {
        req.referrer_policy = v;
      }
      if let Ok(v) = o.get::<_, String>("integrity") {
        req.integrity = v;
      }
      if let Ok(v) = o.get::<_, bool>("keepalive") {
        req.keepalive = v;
      }
      if let Ok(sig) = o.get::<_, Value<'js>>("signal")
        && let Ok(s) = Class::<AbortSignal<'js>>::from_value(&sig)
      {
        req.signal_inner = Some(crate::bindings::abort::native_channel(&ctx, &s)?);
        req.signal = Some(s);
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
          if let Ok(existing) = Class::<FetchRequestJs<'js>>::from_value(&input) {
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
    Ok(req)
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
  #[qjs(get, rename = "cache")]
  pub fn cache(&self) -> String {
    self.cache.clone()
  }
  #[qjs(get, rename = "mode")]
  pub fn mode(&self) -> String {
    self.mode.clone()
  }
  #[qjs(get, rename = "referrer")]
  pub fn referrer(&self) -> String {
    self.referrer.clone()
  }
  #[qjs(get, rename = "referrerPolicy")]
  pub fn referrer_policy(&self) -> String {
    self.referrer_policy.clone()
  }
  #[qjs(get, rename = "integrity")]
  pub fn integrity(&self) -> String {
    self.integrity.clone()
  }
  #[qjs(get, rename = "keepalive")]
  pub fn keepalive(&self) -> bool {
    self.keepalive
  }
  #[qjs(get, rename = "destination")]
  pub fn destination(&self) -> String {
    self.destination.clone()
  }
  #[qjs(get, rename = "headers")]
  pub fn headers(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, HeadersJs>> {
    Class::instance(ctx, HeadersJs::from_pairs(self.headers.iter().cloned()))
  }

  /// `Request.signal` — the `AbortSignal` passed in `init`. Spec always
  /// exposes one, so a request built without a signal reports a fresh,
  /// never-aborted instance.
  #[qjs(get, rename = "signal")]
  pub fn signal(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, AbortSignal<'js>>> {
    if let Some(s) = &self.signal {
      return Ok(s.clone());
    }
    let fresh = crate::bindings::abort::fresh_instance(&ctx)?;
    self.signal = Some(fresh.clone());
    Ok(fresh)
  }

  /// `Request.body` — the body `ReadableStream`, same object on every
  /// access, drained by the body readers.
  #[qjs(get, rename = "body")]
  pub fn body(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStream<'js>>> {
    self.ensure_body_stream(&ctx)
  }

  #[qjs(rename = "text")]
  pub async fn text(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<String> {
    self.mixin_text(&ctx).await
  }

  #[qjs(rename = "json")]
  pub async fn json(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_json(&ctx).await
  }

  #[qjs(rename = "arrayBuffer")]
  pub async fn array_buffer(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_array_buffer(&ctx).await
  }

  #[qjs(rename = "bytes")]
  pub async fn bytes(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_bytes(&ctx).await
  }

  #[qjs(rename = "blob")]
  pub async fn blob(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_blob(&ctx).await
  }

  #[qjs(rename = "formData")]
  pub async fn form_data(&mut self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    self.mixin_form_data(&ctx).await
  }

  /// WHATWG "clone a request". When a body stream has already been
  /// vended the bytes live in it, so the stream is tee'd exactly as
  /// `Response.clone()` does — otherwise the clone would come out with
  /// an empty body.
  #[qjs(rename = "clone")]
  pub fn clone_(this: This<Class<'js, Self>>, ctx: Ctx<'js>) -> rquickjs::Result<Self> {
    {
      let me = this.borrow();
      if me.body_used {
        return Err(rquickjs::Exception::throw_type(&ctx, "Cannot clone a used Request"));
      }
    }
    let branch2 = if this.borrow().body_stream.is_some() {
      let stream = this.borrow_mut().ensure_body_stream(&ctx)?;
      let (branch1, branch2) = ferridriver_jsstd::stream_web::tee_readable_stream(ctx.clone(), stream)?;
      this.borrow_mut().body_stream = Some(branch1);
      Some(branch2)
    } else {
      None
    };
    let me = this.borrow();
    Ok(Self {
      url: me.url.clone(),
      method: me.method.clone(),
      headers: me.headers.clone(),
      body: me.body.clone(),
      redirect: me.redirect.clone(),
      credentials: me.credentials.clone(),
      body_used: false,
      cache: me.cache.clone(),
      mode: me.mode.clone(),
      referrer: me.referrer.clone(),
      referrer_policy: me.referrer_policy.clone(),
      integrity: me.integrity.clone(),
      keepalive: me.keepalive,
      destination: me.destination.clone(),
      signal_inner: me.signal_inner.clone(),
      signal: me.signal.clone(),
      body_stream: branch2,
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
  // the extension dispatch closure.
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
    let req = Class::<FetchRequestJs<'js>>::from_value(&input).ok();
    // Spec: fetching a Request whose body was already read is a
    // TypeError, rather than silently sending nothing.
    if let Some(r) = req.as_ref()
      && r.borrow().body_is_disturbed()
    {
      return Err(rquickjs::Exception::throw_type(
        &ctx,
        "Cannot fetch a Request whose body has already been read",
      ));
    }
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
    // `init.redirect` (or the Request's) maps onto the WHATWG redirect
    // mode: "follow" (default) follows up to the engine cap; "manual"
    // returns an opaque-redirect Response for a 3xx; "error" rejects.
    let redirect = init
      .as_ref()
      .and_then(|o| o.get::<_, String>("redirect").ok())
      .or_else(|| req.as_ref().map(|r| r.borrow().redirect.clone()));
    let redirect = match redirect.as_deref() {
      Some("manual") => RedirectMode::Manual,
      Some("error") => RedirectMode::Error,
      _ => RedirectMode::Follow,
    };
    // `init.credentials` (or the Request's): "omit" sends no cookies;
    // "same-origin" (default) / "include" send them.
    let credentials = init
      .as_ref()
      .and_then(|o| o.get::<_, String>("credentials").ok())
      .or_else(|| req.as_ref().map(|r| r.borrow().credentials.clone()));
    let credentials = match credentials.as_deref() {
      Some("omit") => Credentials::Omit,
      Some("include") => Credentials::Include,
      _ => Credentials::SameOrigin,
    };
    // `signal`: an `AbortSignal` from `init`, else the one carried on the
    // `Request` argument. Grab its native channel so the request future
    // can be dropped when it aborts.
    let signal = init
      .as_ref()
      .and_then(|o| o.get::<_, Value<'_>>("signal").ok())
      .and_then(|v| Class::<AbortSignal<'js>>::from_value(&v).ok())
      .and_then(|s| crate::bindings::abort::native_channel(&ctx, &s).ok())
      .or_else(|| req.as_ref().and_then(|r| r.borrow().signal_inner.clone()));
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
        redirect,
        credentials: Some(credentials),
        // Same sandbox policy as the `request` binding (one core
        // implementation): the active `allow.net` list is enforced on
        // the initial URL AND every redirect hop, and the cloud
        // metadata endpoints are blocked for every script `fetch`
        // regardless of allow-list (closes the default-open SSRF).
        net_guard: Some(ferridriver::http_client::NetGuard {
          allowlist: net.clone(),
          block_metadata: true,
          block_private: false,
        }),
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
      // A network failure is a WHATWG `TypeError`.
      let fut = cx.fetch_stream(&url, Some(opts));
      let resp = match &signal {
        Some(sig) => {
          tokio::select! {
            r = fut => r.map_err(|e| rquickjs::Error::new_from_js_message("fetch", "TypeError", e.to_string()))?,
            () = sig.aborted() => {
              return Err(rquickjs::Error::new_from_js_message("fetch", "AbortError", sig.reason_message()));
            }
          }
        },
        None => fut
          .await
          .map_err(|e| rquickjs::Error::new_from_js_message("fetch", "TypeError", e.to_string()))?,
      };
      // `redirect: manual` on a 3xx yields an opaque-redirect filtered
      // response: type "opaqueredirect", status 0, empty headers, null
      // body, url "" (WHATWG). The unread 3xx is dropped here.
      if resp.unfollowed_redirect() {
        return Ok(FetchResponseJs::opaque_redirect());
      }
      let out = FetchResponseJs::from_stream(
        resp.status(),
        resp.status_text().to_string(),
        resp.url().to_string(),
        resp.headers().to_vec(),
        resp.redirected(),
        resp.response_type().as_str(),
        resp,
      );
      Ok::<_, rquickjs::Error>(out)
    });
    promised.into_js(&ctx)
  }
}
