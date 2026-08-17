//! QuickJS bindings for the `expect` API — Jest-style value matchers,
//! Playwright web-first matchers, asymmetric matchers, and polling.
//!
//! All matcher logic delegates to [`ferridriver_expect`] so the Rust
//! tests and the script layer share one source of truth (per Rule 1 in
//! `CLAUDE.md` — Rust is the source of truth; bindings are thin
//! mirrors). Web-first matchers wrap `ferridriver::Locator` / `Page` /
//! `HttpResponse` directly and reuse [`ferridriver_expect::poll_until`]
//! for retry semantics that match Playwright.

use std::sync::Arc;
use std::time::Duration;

use ferridriver::Page;
use ferridriver::http_client::HttpResponse;
use ferridriver::locator::Locator;
use ferridriver_expect::{
  AssertionFailure, ExpectLive, ExpectValue, JsType, LiveError, LiveValue, POLL_INTERVALS, PromiseMismatch,
  PromiseMode, StringOrRegex, ThrowMatcher, ThrownError, deep_equal, expect_fn, expect_live, expect_value,
};
use rquickjs::{
  Array, Atom, Class, Ctx, Function, IntoJs, JsLifetime, Object, Persistent, Value, class::Trace, function::Opt,
};
use serde_json::Value as JsonValue;

use crate::bindings::convert::{json_to_js, serde_from_js};
use crate::bindings::http_client::HttpResponseJs;
use crate::bindings::locator::LocatorJs;
use crate::bindings::page::PageJs;

// ── ExpectJs ─────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Expect")]
pub struct ExpectJs {
  #[qjs(skip_trace)]
  subject: ExpectSubject,
  is_not: bool,
  is_soft: bool,
  #[qjs(skip_trace)]
  timeout: Duration,
  message: Option<String>,
  /// Index of this assertion's custom-matcher table, and the names in
  /// it. Both are plain data: the matcher FUNCTIONS live in a JS object
  /// the engine traces, never in a native closure.
  #[qjs(skip_trace)]
  matcher_table: u32,
  #[qjs(skip_trace)]
  matcher_names: Vec<String>,
}

/// What `expect(...)` was handed.
///
/// The value itself is kept alive next to the typed handle it resolved
/// to. Playwright's value matchers are defined over the live value —
/// `Object.is`, `instanceof`, `[...received]`, `typeof` — and a JSON
/// snapshot can answer none of them: it has no identity, collapses
/// `undefined` onto `null`, and cannot hold a function. The snapshot is
/// still taken, lazily, for the structural matchers (`toEqual`,
/// `toMatchObject`, ...) whose semantics really are structural.
#[derive(Clone)]
struct ExpectSubject {
  live: Persistent<Value<'static>>,
  kind: SubjectKind,
}

#[derive(Clone)]
enum SubjectKind {
  Locator(Locator),
  Page(Arc<Page>),
  ApiResponse(HttpResponse),
  Value,
}

impl SubjectKind {
  /// The `_apiName` Playwright reports for this receiver in the
  /// wrong-receiver message.
  fn api_name(&self) -> &'static str {
    match self {
      Self::Locator(_) => "Locator",
      Self::Page(_) => "Page",
      Self::ApiResponse(_) => "APIResponse",
      Self::Value => "value",
    }
  }
}

/// A live JS value under assertion — the host half of
/// [`ferridriver_expect::LiveValue`]. Every method here is one JS
/// operation; the matcher decisions all live in core.
struct JsLive<'js>(Value<'js>);

impl<'js> JsLive<'js> {
  fn ctx(&self) -> &Ctx<'js> {
    self.0.ctx()
  }

  fn well_known_symbol(&self, name: &str) -> rquickjs::Result<Atom<'js>> {
    let symbol: Object<'js> = self.ctx().globals().get("Symbol")?;
    let key: Value<'js> = symbol.get(name)?;
    Atom::from_value(self.ctx().clone(), &key)
  }

  fn json(&self) -> Option<JsonValue> {
    serde_from_js(self.ctx(), self.0.clone()).ok()
  }
}

impl<'js> LiveValue for JsLive<'js> {
  type Error = rquickjs::Error;

  fn js_type(&self) -> JsType {
    use rquickjs::Type;
    match self.0.type_of() {
      Type::Undefined | Type::Uninitialized | Type::Unknown => JsType::Undefined,
      Type::Null => JsType::Null,
      Type::Bool => JsType::Boolean,
      Type::Int | Type::Float => JsType::Number,
      Type::BigInt => JsType::BigInt,
      Type::String => JsType::String,
      Type::Symbol => JsType::Symbol,
      Type::Function | Type::Constructor => JsType::Function,
      Type::Array => JsType::Array,
      Type::Object | Type::Promise | Type::Exception | Type::Proxy | Type::Module => JsType::Object,
    }
  }

  fn same_value(&self, other: &Self) -> rquickjs::Result<bool> {
    let object: Object<'js> = self.ctx().globals().get("Object")?;
    let is: Function<'js> = object.get("is")?;
    is.call((self.0.clone(), other.0.clone()))
  }

  fn structurally_equal(&self, other: &Self) -> bool {
    match (self.json(), other.json()) {
      (Some(a), Some(b)) => deep_equal(&a, &b),
      _ => false,
    }
  }

  fn truthy(&self) -> bool {
    match self.js_type() {
      JsType::Undefined | JsType::Null => false,
      JsType::Boolean => self.0.as_bool().unwrap_or(false),
      JsType::Number => self.0.as_number().is_some_and(|n| n != 0.0 && !n.is_nan()),
      JsType::BigInt => self
        .0
        .as_big_int()
        .cloned()
        .and_then(|b| b.to_i64().ok())
        .is_none_or(|v| v != 0),
      JsType::String => self
        .0
        .as_string()
        .and_then(|s| s.to_string().ok())
        .is_some_and(|s| !s.is_empty()),
      _ => true,
    }
  }

  fn number(&self) -> Option<f64> {
    (self.js_type() == JsType::Number).then(|| self.0.as_number()).flatten()
  }

  fn text(&self) -> Option<String> {
    self.0.as_string().and_then(|s| s.to_string().ok())
  }

  fn length(&self) -> rquickjs::Result<Option<f64>> {
    // A JS string's `.length` counts UTF-16 code units, not characters.
    if let Some(s) = self.text() {
      return Ok(Some(s.encode_utf16().count() as f64));
    }
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    let len: Value<'js> = obj.get("length")?;
    Ok(len.as_number())
  }

  fn spread(&self) -> rquickjs::Result<Option<Vec<Self>>> {
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    let iterator = self.well_known_symbol("iterator")?;
    let factory: Value<'js> = obj.get(iterator)?;
    if !factory.is_function() {
      return Ok(None);
    }
    let mut out = Vec::new();
    let iter: rquickjs::JsIterator<'js, Value<'js>> = rquickjs::FromJs::from_js(self.ctx(), self.0.clone())?;
    for item in iter {
      out.push(Self(item?));
    }
    Ok(Some(out))
  }

  fn instance_of(&self, ctor: &Self) -> rquickjs::Result<bool> {
    // `v instanceof C` IS `C[Symbol.hasInstance](v)` — every function
    // inherits the ordinary implementation from `Function.prototype`,
    // and a class defining its own is honoured for free.
    let Some(ctor_obj) = ctor.0.as_object() else {
      return Ok(false);
    };
    let has_instance = self.well_known_symbol("hasInstance")?;
    let check: Function<'js> = ctor_obj.get(has_instance)?;
    check.call((rquickjs::function::This(ctor.0.clone()), self.0.clone()))
  }

  fn describe(&self) -> String {
    let mut out = String::new();
    if ferridriver_jsstd::node::inspect::Inspector::new(false)
      .quoted()
      .value(&mut out, &self.0, 0)
      .is_err()
    {
      return self.0.type_name().to_string();
    }
    out
  }

  fn shape(&self) -> rquickjs::Result<ferridriver_expect::Shape<Self>> {
    use ferridriver_expect::Shape;
    let Some(obj) = self.0.as_object() else {
      return Ok(Shape::Primitive);
    };
    if self.0.is_function() {
      return Ok(Shape::Function);
    }
    // `Object.prototype.toString` is how JS itself tells these apart,
    // and it is not fooled by a subclass or a missing prototype.
    match self.brand()?.as_str() {
      "Date" => {
        let get_time: Function<'js> = obj.get("getTime")?;
        let millis: f64 = get_time.call((rquickjs::function::This(self.0.clone()),))?;
        return Ok(Shape::Date(millis));
      },
      "RegExp" => {
        return Ok(Shape::RegExp {
          source: obj.get::<_, String>("source")?,
          flags: obj.get::<_, String>("flags")?,
        });
      },
      "Error" => {
        return Ok(Shape::Error {
          name: obj.get::<_, Option<String>>("name")?.unwrap_or_default(),
          message: obj.get::<_, Option<String>>("message")?.unwrap_or_default(),
        });
      },
      "Map" => {
        let mut entries = Vec::new();
        let iter: rquickjs::JsIterator<'js, Value<'js>> = rquickjs::FromJs::from_js(self.ctx(), self.0.clone())?;
        for pair in iter {
          let pair = pair?;
          let pair = pair.as_array().cloned().ok_or_else(|| {
            rquickjs::Error::new_from_js_message("expect", "toEqual", "a Map entry was not a [key, value] pair")
          })?;
          entries.push((Self(pair.get::<Value<'js>>(0)?), Self(pair.get::<Value<'js>>(1)?)));
        }
        return Ok(Shape::Map(entries));
      },
      "Set" => {
        let mut values = Vec::new();
        let iter: rquickjs::JsIterator<'js, Value<'js>> = rquickjs::FromJs::from_js(self.ctx(), self.0.clone())?;
        for value in iter {
          values.push(Self(value?));
        }
        return Ok(Shape::Set(values));
      },
      _ => {},
    }
    if let Some(bytes) = self.bytes()? {
      return Ok(Shape::Bytes(bytes));
    }
    if let Some(arr) = self.0.as_array() {
      // A hole is an index the array does not own, which is not the
      // same as an index holding `undefined`.
      let has_own: Function<'js> = self.ctx().globals().get::<_, Object<'js>>("Object")?.get("hasOwn")?;
      let mut items = Vec::with_capacity(arr.len());
      for i in 0..arr.len() {
        let present: bool = has_own.call((self.0.clone(), i.to_string()))?;
        items.push(if present {
          Some(Self(arr.get::<Value<'js>>(i)?))
        } else {
          None
        });
      }
      return Ok(Shape::Array(items));
    }
    let mut entries = Vec::new();
    for key in obj.keys::<String>() {
      let key = key?;
      let value: Value<'js> = obj.get(key.as_str())?;
      entries.push((key, Self(value)));
    }
    Ok(Shape::Object(entries))
  }

  fn get_prop(&self, key: &str) -> rquickjs::Result<Option<Self>> {
    // A property read, not a structural walk: `toHaveProperty` should
    // see a getter and an inherited field, which own-enumerable-keys
    // would miss.
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    if !obj.contains_key(key)? {
      return Ok(None);
    }
    Ok(Some(Self(obj.get(key)?)))
  }

  fn class_name(&self) -> Option<String> {
    let ctor: Value<'js> = self.0.as_object()?.get("constructor").ok()?;
    let name: Value<'js> = ctor.as_object()?.get("name").ok()?;
    name.as_string().and_then(|s| s.to_string().ok())
  }

  fn ref_id(&self) -> Option<u64> {
    // The engine needs a stable identity per object to stop a cyclic
    // comparison. rquickjs hashes a `Value` as its tag plus its raw
    // payload — for an object that payload IS the pointer — so a hasher
    // that records rather than mixes hands back the exact bits, with no
    // `unsafe` and no chance of two objects colliding.
    #[derive(Default)]
    struct RawBits {
      tag: i32,
      bits: u64,
    }
    impl std::hash::Hasher for RawBits {
      fn finish(&self) -> u64 {
        self.bits
      }
      fn write(&mut self, _bytes: &[u8]) {}
      fn write_i32(&mut self, value: i32) {
        self.tag = value;
      }
      fn write_u64(&mut self, value: u64) {
        self.bits = value;
      }
    }
    self.0.as_object()?;
    let mut hasher = RawBits::default();
    std::hash::Hash::hash(&self.0, &mut hasher);
    Some(std::hash::Hasher::finish(&hasher))
  }

  fn as_json(&self) -> Option<JsonValue> {
    self.json()
  }
}

impl<'js> JsLive<'js> {
  /// `Object.prototype.toString.call(v)` reduced to its brand, which is
  /// how JS distinguishes a Date from a Map from a plain object.
  fn brand(&self) -> rquickjs::Result<String> {
    let object: Object<'js> = self.ctx().globals().get("Object")?;
    let proto: Object<'js> = object.get("prototype")?;
    let to_string: Function<'js> = proto.get("toString")?;
    let tag: String = to_string.call((rquickjs::function::This(self.0.clone()),))?;
    Ok(tag.trim_start_matches("[object ").trim_end_matches(']').to_string())
  }

  /// An `ArrayBuffer` or a typed array as its bytes, so two views with
  /// the same contents compare equal.
  fn bytes(&self) -> rquickjs::Result<Option<Vec<u8>>> {
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    if let Some(buffer) = rquickjs::ArrayBuffer::from_object(obj.clone()) {
      return Ok(buffer.as_bytes().map(<[u8]>::to_vec));
    }
    let brand = self.brand()?;
    if !brand.ends_with("Array") || brand == "Array" {
      return Ok(None);
    }
    let Ok(buffer) = obj.get::<_, Value<'js>>("buffer") else {
      return Ok(None);
    };
    let Some(buffer) = buffer.as_object().cloned().and_then(rquickjs::ArrayBuffer::from_object) else {
      return Ok(None);
    };
    let offset: usize = obj.get::<_, Option<f64>>("byteOffset")?.unwrap_or(0.0) as usize;
    let length: usize = obj.get::<_, Option<f64>>("byteLength")?.unwrap_or(0.0) as usize;
    Ok(
      buffer
        .as_bytes()
        .map(|all| all[offset.min(all.len())..(offset + length).min(all.len())].to_vec()),
    )
  }
}

/// The `expect` block the test currently running in this VM resolved to.
/// The runner mirrors it in through `set_current_test`, so a bare
/// `expect(x)` in a spec starts from `[test.expect]` — or from the
/// project's own `expect` block, which REPLACES the config's whole
/// object rather than merging into it.
pub(crate) struct ExpectDefaultsUd(pub std::cell::RefCell<Arc<ferridriver_test::config::ExpectConfig>>);

// SAFETY: holds only `'static` data, same rationale as the other
// per-session registries in this crate.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for ExpectDefaultsUd {
  type Changed<'to> = ExpectDefaultsUd;
}

/// Publish the running test's resolved `expect` block to this VM.
pub(crate) fn set_expect_defaults(ctx: &Ctx<'_>, expect: Arc<ferridriver_test::config::ExpectConfig>) {
  if let Some(ud) = ctx.userdata::<ExpectDefaultsUd>() {
    *ud.0.borrow_mut() = expect;
    return;
  }
  let _ = ctx.store_userdata(ExpectDefaultsUd(std::cell::RefCell::new(expect)));
}

/// The running test's `expect` block, or the defaults when there is no
/// test (a `run_script` session).
fn expect_defaults(ctx: &Ctx<'_>) -> Arc<ferridriver_test::config::ExpectConfig> {
  ctx
    .userdata::<ExpectDefaultsUd>()
    .map_or_else(Arc::default, |ud| Arc::clone(&ud.0.borrow()))
}

/// What an assertion built right now starts from: the running test's
/// resolved `expect.timeout`, else the process-wide default the runner
/// set from the config.
fn default_timeout(ctx: &Ctx<'_>) -> Duration {
  ctx
    .userdata::<ExpectDefaultsUd>()
    .map_or_else(ferridriver_expect::default_expect_timeout, |ud| {
      Duration::from_millis(ud.0.borrow().timeout_ms())
    })
}

impl ExpectJs {
  fn new(subject: ExpectSubject, timeout: Duration) -> Self {
    Self {
      subject,
      is_not: false,
      is_soft: false,
      timeout,
      message: None,
      matcher_table: 0,
      matcher_names: Vec::new(),
    }
  }

  fn clone_with<F: FnOnce(&mut Self)>(&self, mutate: F) -> Self {
    let mut out = Self {
      subject: self.subject.clone(),
      is_not: self.is_not,
      is_soft: self.is_soft,
      timeout: self.timeout,
      message: self.message.clone(),
      matcher_table: self.matcher_table,
      matcher_names: self.matcher_names.clone(),
    };
    mutate(&mut out);
    out
  }

  /// The subject as the live JS value it still is.
  fn live<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<JsLive<'js>> {
    Ok(JsLive(self.subject.live.clone().restore(ctx)?))
  }

  /// The same assertion over a different subject — what a settled
  /// `.resolves` / `.rejects` chain runs its matcher against. The new
  /// subject is dispatched afresh, so a promise resolving to a Locator
  /// gets the Locator matchers.
  fn with_subject<'js>(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Self {
    let mut out = build_expect(ctx, value);
    out.is_not = self.is_not;
    out.is_soft = self.is_soft;
    out.timeout = self.timeout;
    out.message.clone_from(&self.message);
    out.matcher_table = self.matcher_table;
    out.matcher_names.clone_from(&self.matcher_names);
    out
  }

  /// The subject's structural snapshot, taken now rather than at
  /// `expect(...)` time — a subject that cannot be serialized only
  /// fails the matchers that actually need a snapshot.
  fn snapshot(&self, ctx: &Ctx<'_>) -> Result<JsonValue, rquickjs::Error> {
    serde_from_js(ctx, self.subject.live.clone().restore(ctx)?)
  }

  /// A live-value assertion carrying this assertion's `.not` / `.soft` /
  /// message state.
  fn live_expect<'a, 'js>(&self, actual: &'a JsLive<'js>) -> ExpectLive<'a, JsLive<'js>> {
    let mut e = expect_live(actual);
    if self.is_not {
      e = e.not();
    }
    if self.is_soft {
      e = e.soft();
    }
    if let Some(m) = &self.message {
      e = e.with_message(m.clone());
    }
    e
  }

  fn locator_target(&self, ctx: &Ctx<'_>, matcher: &'static str) -> Result<&Locator, rquickjs::Error> {
    match &self.subject.kind {
      SubjectKind::Locator(loc) => Ok(loc),
      _ => Err(self.wrong_receiver(ctx, matcher, "Locator")),
    }
  }

  fn page_target(&self, ctx: &Ctx<'_>, matcher: &'static str) -> Result<&Arc<Page>, rquickjs::Error> {
    match &self.subject.kind {
      SubjectKind::Page(p) => Ok(p),
      _ => Err(self.wrong_receiver(ctx, matcher, "Page")),
    }
  }

  fn api_response_target(&self, ctx: &Ctx<'_>, matcher: &'static str) -> Result<&HttpResponse, rquickjs::Error> {
    match &self.subject.kind {
      SubjectKind::ApiResponse(r) => Ok(r),
      _ => Err(self.wrong_receiver(ctx, matcher, "APIResponse")),
    }
  }

  /// Playwright's `expectTypes` message (`matcherHint.ts:65`), verbatim
  /// down to the receiver rendering.
  fn wrong_receiver(&self, ctx: &Ctx<'_>, matcher: &'static str, wanted: &str) -> rquickjs::Error {
    let received = self
      .live(ctx)
      .map_or_else(|_| self.subject.kind.api_name().to_string(), |v| v.describe());
    crate::bindings::convert::throw_named(
      ctx,
      "Error",
      format!("{matcher} can be only used with {wanted} object, was called with {received}"),
    )
  }

  /// The function a function-only matcher (`toThrow`, `toPass`) needs.
  fn function_target<'js>(&self, ctx: &Ctx<'js>, matcher: &'static str) -> rquickjs::Result<Function<'js>> {
    let live = self.live(ctx)?;
    live.0.as_function().cloned().ok_or_else(|| {
      crate::bindings::convert::throw_named(
        ctx,
        "TypeError",
        format!(
          "expect(received).{matcher}(expected)\n\nreceived value must be a function\n\nReceived: {}",
          live.describe()
        ),
      )
    })
  }

  /// The decision half of `toThrow`, shared with the `.rejects` path —
  /// there the rejection reason IS the thrown error, so nothing is
  /// called (Playwright's `createThrowMatcher(name, fromPromise)`).
  fn check_thrown<'js>(
    &self,
    ctx: &Ctx<'js>,
    caught: Option<ThrownError>,
    matcher: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let matcher = match matcher.0 {
      Some(v) if !v.is_undefined() => Some(parse_throw_matcher(ctx, v)?),
      _ => None,
    };
    let mut ef = expect_fn(caught);
    if self.is_not {
      ef = ef.not();
    }
    if let Some(m) = &self.message {
      ef = ef.with_message(m.clone());
    }
    ef.to_throw(matcher.as_ref()).map_err(|e| assertion_to_rq(ctx, e))
  }

  fn build_value_expect(&self, ctx: &Ctx<'_>) -> Result<ExpectValue, rquickjs::Error> {
    let mut ev = expect_value(self.snapshot(ctx)?);
    if self.is_not {
      ev = ev.not();
    }
    if self.is_soft {
      ev = ev.soft();
    }
    if let Some(m) = &self.message {
      ev = ev.with_message(m.clone());
    }
    Ok(ev)
  }

  /// Build a configured `ferridriver_expect::Expect<'_, Locator>` so
  /// every web-first locator matcher delegates to the shared Rust
  /// impl in `ferridriver-expect` (single source of truth). Matcher
  /// state (timeout, `.not`, `.soft`, message) is copied over once
  /// per call.
  /// Owned core handle for the snapshot matchers, crossing the bridge.
  fn snapshot_target(
    &self,
    ctx: &Ctx<'_>,
    matcher: &'static str,
  ) -> Result<ferridriver_test::host::SnapshotTarget, rquickjs::Error> {
    use ferridriver_test::host::SnapshotTarget;
    match &self.subject.kind {
      SubjectKind::Locator(l) => Ok(SnapshotTarget::Locator(l.clone())),
      SubjectKind::Page(p) => Ok(SnapshotTarget::Page(std::sync::Arc::clone(p))),
      // A subject with no serializable form (a function, a class
      // instance holding host state) reports the matcher's own
      // requirement rather than the serializer's error.
      SubjectKind::Value => match self.snapshot(ctx) {
        Ok(JsonValue::String(s)) => Ok(SnapshotTarget::Value(s)),
        Ok(other) => Ok(SnapshotTarget::Value(other.to_string())),
        Err(_) => Err(unsupported_snapshot_subject(matcher)),
      },
      SubjectKind::ApiResponse(_) => Err(unsupported_snapshot_subject(matcher)),
    }
  }

  fn build_locator_expect(
    &self,
    ctx: &Ctx<'_>,
    matcher: &'static str,
  ) -> Result<ferridriver_expect::Expect<'_, Locator>, rquickjs::Error> {
    let loc = self.locator_target(ctx, matcher)?;
    let mut e = ferridriver_expect::expect(loc).with_timeout(self.timeout);
    if self.is_not {
      e = e.not();
    }
    if self.is_soft {
      e = e.soft();
    }
    if let Some(m) = &self.message {
      e = e.with_message(m.clone());
    }
    Ok(e)
  }

  fn build_page_expect(
    &self,
    ctx: &Ctx<'_>,
    matcher: &'static str,
  ) -> Result<ferridriver_expect::Expect<'_, std::sync::Arc<Page>>, rquickjs::Error> {
    let p = self.page_target(ctx, matcher)?;
    let mut e = ferridriver_expect::expect(p).with_timeout(self.timeout);
    if self.is_not {
      e = e.not();
    }
    if self.is_soft {
      e = e.soft();
    }
    if let Some(m) = &self.message {
      e = e.with_message(m.clone());
    }
    Ok(e)
  }

  fn build_api_response_expect(
    &self,
    ctx: &Ctx<'_>,
    matcher: &'static str,
  ) -> Result<ferridriver_expect::Expect<'_, HttpResponse>, rquickjs::Error> {
    let r = self.api_response_target(ctx, matcher)?;
    let mut e = ferridriver_expect::expect(r);
    if self.is_not {
      e = e.not();
    }
    if self.is_soft {
      e = e.soft();
    }
    if let Some(m) = &self.message {
      e = e.with_message(m.clone());
    }
    Ok(e)
  }

  /// Per-call copy with the inline `{ timeout }` matcher option applied
  /// (Playwright: every web-first matcher accepts `{ timeout? }` in its
  /// trailing options bag, overriding the assertion default).
  fn for_call(&self, obj: Option<&Object<'_>>) -> Self {
    let timeout = u64_field(obj, "timeout");
    self.clone_with(|e| {
      if let Some(ms) = timeout {
        e.timeout = Duration::from_millis(ms);
      }
    })
  }

  /// Copy with negation toggled — backs boolean-state matcher options
  /// (`toBeChecked({ checked: false })`, `toBeAttached({ attached: false })`).
  fn negated(&self) -> Self {
    self.clone_with(|e| e.is_not = !e.is_not)
  }
}

fn unsupported_snapshot_subject(matcher: &'static str) -> rquickjs::Error {
  rquickjs::Error::new_from_js_message(
    "expect",
    matcher,
    "snapshot matchers apply to a locator, a page, or a serializable value",
  )
}

/// A snapshot-matcher failure (message produced runner-side, already
/// fully formatted), resolved by the same soft rule as every other
/// assertion.
fn snapshot_failure(ctx: &Ctx<'_>, is_soft: bool, message: String) -> rquickjs::Result<()> {
  report(ctx, AssertionFailure::new(message, None).with_soft(is_soft))
}

/// A failed assertion, resolved the way its softness says: a soft
/// failure is recorded against the running test and the caller carries
/// on; anything else is thrown.
///
/// Outside a test there is nothing to record into, so a soft failure
/// throws — the same rule `ferridriver_expect::soft` applies on the Rust
/// side, where an absent sink means the failure surfaces.
fn report(ctx: &Ctx<'_>, err: AssertionFailure) -> rquickjs::Result<()> {
  if err.soft
    && let Some(bridge) = crate::bindings::test::optional_bridge(ctx)
  {
    bridge.record_soft_error(assertion_text(&err), err.diff);
    return Ok(());
  }
  Err(assertion_to_rq(ctx, err))
}

/// The same one-string rendering the thrown error carries.
fn assertion_text(err: &AssertionFailure) -> String {
  match err.diff.as_deref() {
    Some(body) if !body.is_empty() => format!("{}\n\n{body}", err.message),
    _ => err.message.clone(),
  }
}

fn assertion_to_rq(ctx: &Ctx<'_>, err: AssertionFailure) -> rquickjs::Error {
  // Concatenate title + body for the JS-thrown message so the JS stack
  // shows the full failure on one Error. The JS stack itself comes from
  // QuickJS and is added to the Error automatically. Thrown as a real
  // `Error` with `name = "AssertionError"` so `e.name` checks match.
  let full = match err.diff.as_deref() {
    Some(body) if !body.is_empty() => format!("{}\n\n{body}", err.message),
    _ => err.message,
  };
  crate::bindings::convert::throw_named(ctx, "AssertionError", full)
}

/// The options bag of a web-first matcher call, if one was passed.
fn opts_obj<'js>(options: &Opt<Value<'js>>) -> Option<Object<'js>> {
  options.0.as_ref().and_then(Value::as_object).cloned()
}

fn u64_field(obj: Option<&Object<'_>>, key: &str) -> Option<u64> {
  let v = obj?.get::<_, rquickjs::Value<'_>>(key).ok()?;
  v.as_int()
    .and_then(|i| u64::try_from(i).ok())
    .or_else(|| v.as_number().filter(|n| *n >= 0.0).map(|n| n as u64))
}

fn bool_field(obj: Option<&Object<'_>>, key: &str) -> Option<bool> {
  obj?.get::<_, rquickjs::Value<'_>>(key).ok()?.as_bool()
}

fn f64_field(obj: Option<&Object<'_>>, key: &str) -> Option<f64> {
  let v = obj?.get::<_, rquickjs::Value<'_>>(key).ok()?;
  v.as_number().or_else(|| v.as_int().map(f64::from))
}

/// Parse a `number[]` option field (e.g. `intervals`), dropping
/// non-numeric entries; `None` when absent or empty.
fn u64_array_field(obj: Option<&Object<'_>>, key: &str) -> Option<Vec<u64>> {
  let arr = obj?.get::<_, rquickjs::Value<'_>>(key).ok()?.into_array()?;
  let out: Vec<u64> = arr
    .iter::<rquickjs::Value<'_>>()
    .filter_map(std::result::Result::ok)
    .filter_map(|v| {
      v.as_int()
        .and_then(|i| u64::try_from(i).ok())
        .or_else(|| v.as_number().filter(|n| *n >= 0.0).map(|n| n as u64))
    })
    .collect();
  (!out.is_empty()).then_some(out)
}

/// Throw when a Playwright option we cannot honor yet is present —
/// silently accepting and dropping it would be a false completion.
fn reject_unsupported_option(obj: Option<&Object<'_>>, matcher: &'static str, key: &str) -> rquickjs::Result<()> {
  if let Some(o) = obj
    && o.contains_key(key).unwrap_or(false)
  {
    return Err(rquickjs::Error::new_from_js_message(
      "expect",
      matcher,
      format!("the \"{key}\" option is not supported yet"),
    ));
  }
  Ok(())
}

/// True when the value is a string or a RegExp instance — the shapes
/// Playwright treats as an expected text value rather than an options
/// bag in overloads like `toHaveAttribute(name, value?, options?)`.
fn is_string_or_regex(value: &Value<'_>) -> bool {
  if value.as_string().is_some() {
    return true;
  }
  value.as_object().is_some_and(|obj| {
    let src = obj.get::<_, rquickjs::Value<'_>>("source").ok();
    let flags = obj.get::<_, rquickjs::Value<'_>>("flags").ok();
    matches!((src, flags), (Some(s), Some(f)) if s.as_string().is_some() && f.as_string().is_some())
  })
}

fn text_match_options(obj: Option<&Object<'_>>) -> ferridriver_expect::TextMatchOptions {
  ferridriver_expect::TextMatchOptions {
    ignore_case: bool_field(obj, "ignoreCase").unwrap_or(false),
    use_inner_text: bool_field(obj, "useInnerText").unwrap_or(false),
  }
}

fn parse_string_or_regex<'js>(_ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<StringOrRegex> {
  if let Some(s) = value.as_string() {
    return Ok(StringOrRegex::String(s.to_string()?));
  }
  // RegExp instance: read `.source` and `.flags`.
  if let Some(obj) = value.as_object() {
    let source: rquickjs::Result<rquickjs::Value<'js>> = obj.get("source");
    let flags: rquickjs::Result<rquickjs::Value<'js>> = obj.get("flags");
    if let (Ok(s), Ok(f)) = (source, flags)
      && let (Some(s), Some(f)) = (s.as_string(), f.as_string())
    {
      let pat = s.to_string()?;
      let flg = f.to_string()?;
      let re = ferridriver_expect::asymmetric::compile_js_regex(&pat, &flg)
        .map_err(|e| rquickjs::Error::new_from_js_message("expect", "RegExp", e.to_string()))?;
      return Ok(StringOrRegex::Regex(re));
    }
  }
  Err(rquickjs::Error::new_from_js_message(
    "expect",
    "argument",
    "expected a string or RegExp",
  ))
}

/// The `(string | RegExp)[]` half of the text matchers' overload.
/// `None` when the argument is not an array, so the caller falls
/// through to the single-value form.
fn parse_string_or_regex_array<'js>(
  ctx: &Ctx<'js>,
  value: &Value<'js>,
) -> rquickjs::Result<Option<Vec<StringOrRegex>>> {
  let Some(arr) = value.as_array() else {
    return Ok(None);
  };
  let mut out = Vec::with_capacity(arr.len());
  for item in arr.iter::<Value<'js>>() {
    out.push(parse_string_or_regex(ctx, &item?)?);
  }
  Ok(Some(out))
}

#[rquickjs::methods]
impl ExpectJs {
  // ── modifiers ────────────────────────────────────────────────────

  /// `.not` getter — returns a new ExpectJs with the negation flag
  /// toggled. Implemented as a method so `expect(x).not.toBe(y)` reads
  /// naturally; the JS-side `Object.defineProperty` shim in
  /// `install_expect` adapts it into a getter on the class prototype.
  #[qjs(rename = "_notInner")]
  pub fn not_inner(&self) -> ExpectJs {
    self.clone_with(|e| e.is_not = !e.is_not)
  }

  /// `.soft` modifier.
  #[qjs(rename = "soft")]
  pub fn soft(&self) -> ExpectJs {
    self.clone_with(|e| e.is_soft = true)
  }

  /// Override the timeout for web-first matchers on this assertion
  /// (milliseconds).
  #[qjs(rename = "withTimeout")]
  pub fn with_timeout(&self, timeout_ms: u32) -> ExpectJs {
    self.clone_with(|e| e.timeout = Duration::from_millis(u64::from(timeout_ms)))
  }

  /// Attach a custom failure-message prefix.
  #[qjs(rename = "withMessage")]
  pub fn with_message(&self, msg: String) -> ExpectJs {
    self.clone_with(|e| e.message = Some(msg))
  }

  // ── value matchers ───────────────────────────────────────────────

  /// Playwright: `Object.is` equality (jest `expectLibrary.ts:623`) —
  /// two structurally equal objects are NOT `toBe`-equal, and a
  /// reference IS `toBe`-equal to itself.
  #[qjs(rename = "toBe")]
  pub fn to_be<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    let expected = JsLive(expected);
    self
      .live_expect(&actual)
      .to_be(&expected)
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toEqual")]
  pub fn to_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let (actual, expected) = (self.live(&ctx)?, JsLive(expected));
    let ev = self.evaluator(&ctx);
    self
      .live_expect(&actual)
      .to_equal(
        &expected,
        ev.as_ref().map(|e| e as &dyn ferridriver_expect::CustomAsymmetric),
      )
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toStrictEqual")]
  pub fn to_strict_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let (actual, expected) = (self.live(&ctx)?, JsLive(expected));
    let ev = self.evaluator(&ctx);
    self
      .live_expect(&actual)
      .to_strict_equal(
        &expected,
        ev.as_ref().map(|e| e as &dyn ferridriver_expect::CustomAsymmetric),
      )
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeNull")]
  pub fn to_be_null(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self.live_expect(&actual).to_be_null().or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeUndefined")]
  pub fn to_be_undefined(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_undefined()
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeDefined")]
  pub fn to_be_defined(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_defined()
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeTruthy")]
  pub fn to_be_truthy(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_truthy()
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeFalsy")]
  pub fn to_be_falsy(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_falsy()
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeNaN")]
  pub fn to_be_nan(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self.live_expect(&actual).to_be_nan().or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toBeCloseTo")]
  pub fn to_be_close_to(&self, ctx: Ctx<'_>, expected: f64, digits: Opt<u8>) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_close_to(expected, digits.0)
      .or_else(|e| report(&ctx, e))
  }

  #[qjs(rename = "toBeGreaterThan")]
  pub fn to_be_greater_than(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_greater_than(expected)
      .or_else(|e| report(&ctx, e))
  }

  #[qjs(rename = "toBeGreaterThanOrEqual")]
  pub fn to_be_greater_than_or_equal(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_greater_than_or_equal(expected)
      .or_else(|e| report(&ctx, e))
  }

  #[qjs(rename = "toBeLessThan")]
  pub fn to_be_less_than(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_less_than(expected)
      .or_else(|e| report(&ctx, e))
  }

  #[qjs(rename = "toBeLessThanOrEqual")]
  pub fn to_be_less_than_or_equal(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_less_than_or_equal(expected)
      .or_else(|e| report(&ctx, e))
  }

  /// Playwright: substring for a string receiver, otherwise
  /// `[...received].indexOf(expected)` — strict equality over the live
  /// items, so a structurally equal copy is NOT contained.
  #[qjs(rename = "toContain")]
  pub fn to_contain<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    let expected = JsLive(expected);
    self
      .live_expect(&actual)
      .to_contain(&expected)
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toContainEqual")]
  pub fn to_contain_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let (actual, expected) = (self.live(&ctx)?, JsLive(expected));
    let ev = self.evaluator(&ctx);
    self
      .live_expect(&actual)
      .to_contain_equal(
        &expected,
        ev.as_ref().map(|e| e as &dyn ferridriver_expect::CustomAsymmetric),
      )
      .or_else(|e| live_report(&ctx, e))
  }

  /// Playwright reads the receiver's own `.length`, so a function's
  /// arity and a typed array's length answer here too.
  #[qjs(rename = "toHaveLength")]
  pub fn to_have_length(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_have_length(expected)
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toHaveProperty")]
  pub fn to_have_property<'js>(
    &self,
    ctx: Ctx<'js>,
    path: Value<'js>,
    expected: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let segments = parse_property_path(&ctx, &path)?;
    let expected = match expected.0 {
      Some(v) if !v.is_undefined() => Some(JsLive(v)),
      _ => None,
    };
    let actual = self.live(&ctx)?;
    let ev = self.evaluator(&ctx);
    self
      .live_expect(&actual)
      .to_have_property(
        &segments,
        expected.as_ref(),
        ev.as_ref().map(|e| e as &dyn ferridriver_expect::CustomAsymmetric),
      )
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toMatch")]
  pub fn to_match<'js>(&self, ctx: Ctx<'js>, pattern: Value<'js>) -> rquickjs::Result<()> {
    let pat = parse_string_or_regex(&ctx, &pattern)?;
    self
      .build_value_expect(&ctx)?
      .to_match(&pat)
      .or_else(|e| report(&ctx, e))
  }

  #[qjs(rename = "toMatchObject")]
  pub fn to_match_object<'js>(&self, ctx: Ctx<'js>, subset: Value<'js>) -> rquickjs::Result<()> {
    let (actual, subset) = (self.live(&ctx)?, JsLive(subset));
    let ev = self.evaluator(&ctx);
    self
      .live_expect(&actual)
      .to_match_object(
        &subset,
        ev.as_ref().map(|e| e as &dyn ferridriver_expect::CustomAsymmetric),
      )
      .or_else(|e| live_report(&ctx, e))
  }

  /// Playwright: the real `instanceof` operator (jest
  /// `expectLibrary.ts:789`) — a subclass instance IS an instance of
  /// its base, which a constructor-name comparison cannot see.
  #[qjs(rename = "toBeInstanceOf")]
  pub fn to_be_instance_of<'js>(&self, ctx: Ctx<'js>, ctor: Value<'js>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    let ctor = JsLive(ctor);
    self
      .live_expect(&actual)
      .to_be_instance_of(&ctor)
      .or_else(|e| live_report(&ctx, e))
  }

  #[qjs(rename = "toThrow")]
  pub async fn to_throw<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    matcher: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let f = self.function_target(&ctx, "toThrow")?;
        let call_outcome: rquickjs::Result<rquickjs::Value<'js>> = f.call(());
        // If the function returned a Promise (async fn), await it so a
        // post-microtask throw is captured.
        let final_outcome = match call_outcome {
          Ok(v) => match v.as_promise() {
            Some(p) => p.clone().into_future::<rquickjs::Value<'js>>().await,
            None => Ok(v),
          },
          Err(e) => Err(e),
        };
        let caught = match final_outcome {
          Ok(_) => None,
          Err(rquickjs::Error::Exception) => {
            let exc = ctx.catch();
            let (msg, name) = extract_error(&exc);
            Some(ThrownError {
              message: msg,
              class_name: name,
            })
          },
          Err(other) => Some(ThrownError {
            message: other.to_string(),
            class_name: None,
          }),
        };
        self.check_thrown(&ctx, caught, matcher)
      })
      .await
  }

  // ── Locator web-first matchers (delegated to ferridriver-expect) ──
  //
  // Every matcher takes Playwright's trailing options bag; `{ timeout }`
  // overrides the assertion timeout for that call. Boolean-state options
  // (`visible: false`, `enabled: false`, ...) dispatch to the matching
  // counterpart assertion, mirroring Playwright's matcher lowering.

  /// Playwright: `toBeVisible(options?: { timeout?, visible? })`.
  #[qjs(rename = "toBeVisible")]
  pub async fn to_be_visible<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        let me = self.for_call(o.as_ref());
        let want_visible = bool_field(o.as_ref(), "visible").unwrap_or(true);
        if want_visible {
          me.build_locator_expect(&ctx, "toBeVisible")?.to_be_visible().await
        } else {
          me.build_locator_expect(&ctx, "toBeVisible")?.to_be_hidden().await
        }
        .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeHidden(options?: { timeout? })`.
  #[qjs(rename = "toBeHidden")]
  pub async fn to_be_hidden<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toBeHidden")?
          .to_be_hidden()
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeEnabled(options?: { enabled?, timeout? })`.
  #[qjs(rename = "toBeEnabled")]
  pub async fn to_be_enabled<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        let me = self.for_call(o.as_ref());
        let want_enabled = bool_field(o.as_ref(), "enabled").unwrap_or(true);
        if want_enabled {
          me.build_locator_expect(&ctx, "toBeEnabled")?.to_be_enabled().await
        } else {
          me.build_locator_expect(&ctx, "toBeEnabled")?.to_be_disabled().await
        }
        .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeDisabled(options?: { timeout? })`.
  #[qjs(rename = "toBeDisabled")]
  pub async fn to_be_disabled<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toBeDisabled")?
          .to_be_disabled()
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeChecked(options?: { checked?, indeterminate?, timeout? })`.
  #[qjs(rename = "toBeChecked")]
  pub async fn to_be_checked<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        reject_unsupported_option(o.as_ref(), "toBeChecked", "indeterminate")?;
        let me = self.for_call(o.as_ref());
        let want_checked = bool_field(o.as_ref(), "checked").unwrap_or(true);
        let me = if want_checked { me } else { me.negated() };
        me.build_locator_expect(&ctx, "toBeChecked")?
          .to_be_checked()
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeEditable(options?: { editable?, timeout? })`.
  /// `editable: false` maps to the readonly assertion, matching
  /// Playwright's `to.be.readonly` lowering (not a plain negation).
  #[qjs(rename = "toBeEditable")]
  pub async fn to_be_editable<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        let me = self.for_call(o.as_ref());
        let want_editable = bool_field(o.as_ref(), "editable").unwrap_or(true);
        if want_editable {
          me.build_locator_expect(&ctx, "toBeEditable")?.to_be_editable().await
        } else {
          me.build_locator_expect(&ctx, "toBeEditable")?.to_be_readonly().await
        }
        .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeAttached(options?: { attached?, timeout? })`.
  #[qjs(rename = "toBeAttached")]
  pub async fn to_be_attached<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        let me = self.for_call(o.as_ref());
        let want_attached = bool_field(o.as_ref(), "attached").unwrap_or(true);
        let me = if want_attached { me } else { me.negated() };
        me.build_locator_expect(&ctx, "toBeAttached")?
          .to_be_attached()
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeEmpty(options?: { timeout? })`.
  #[qjs(rename = "toBeEmpty")]
  pub async fn to_be_empty<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toBeEmpty")?
          .to_be_empty()
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeFocused(options?: { timeout? })`.
  #[qjs(rename = "toBeFocused")]
  pub async fn to_be_focused<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toBeFocused")?
          .to_be_focused()
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeInViewport(options?: { ratio?, timeout? })`.
  #[qjs(rename = "toBeInViewport")]
  pub async fn to_be_in_viewport<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        let opts = ferridriver_expect::InViewportOptions {
          ratio: f64_field(o.as_ref(), "ratio"),
        };
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toBeInViewport")?
          .to_be_in_viewport_with(opts)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveText(expected: string | RegExp | (string | RegExp)[],
  /// options?: { ignoreCase?, timeout?, useInnerText? })`. The array
  /// form asserts over EVERY element the locator resolves to.
  #[qjs(rename = "toHaveText")]
  pub async fn to_have_text<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        if let Some(list) = parse_string_or_regex_array(&ctx, &expected)? {
          return self
            .for_call(o.as_ref())
            .build_locator_expect(&ctx, "toHaveText")?
            .to_have_text_array_with(&list, text_match_options(o.as_ref()))
            .await
            .or_else(|e| report(&ctx, e));
        }
        let exp = parse_string_or_regex(&ctx, &expected)?;
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveText")?
          .to_have_text_with(exp, text_match_options(o.as_ref()))
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toContainText(expected: string | RegExp | (string | RegExp)[],
  /// options?: { ignoreCase?, timeout?, useInnerText? })`.
  #[qjs(rename = "toContainText")]
  pub async fn to_contain_text<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        if let Some(list) = parse_string_or_regex_array(&ctx, &expected)? {
          return self
            .for_call(o.as_ref())
            .build_locator_expect(&ctx, "toContainText")?
            .to_contain_text_array_with(&list, text_match_options(o.as_ref()))
            .await
            .or_else(|e| report(&ctx, e));
        }
        let exp = parse_string_or_regex(&ctx, &expected)?;
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toContainText")?
          .to_contain_text_with(exp, text_match_options(o.as_ref()))
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveValue(value: string | RegExp, options?: { timeout? })`.
  #[qjs(rename = "toHaveValue")]
  pub async fn to_have_value<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveValue")?
          .to_have_value(exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveValues(values: Array<string | RegExp>,
  /// options?: { timeout? })`. RegExp entries are not supported yet —
  /// they throw rather than silently mismatching.
  #[qjs(rename = "toHaveValues")]
  pub async fn to_have_values<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Vec<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let mut values = Vec::with_capacity(expected.len());
        for v in &expected {
          let Some(s) = v.as_string() else {
            return Err(rquickjs::Error::new_from_js_message(
              "expect",
              "toHaveValues",
              "RegExp entries are not supported yet — pass strings",
            ));
          };
          values.push(s.to_string()?);
        }
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveValues")?
          .to_have_values(&values)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveCount(count: number, options?: { timeout? })`.
  #[qjs(rename = "toHaveCount")]
  pub async fn to_have_count<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: u32,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveCount")?
          .to_have_count(expected as usize)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright overloads: `toHaveAttribute(name, value: string | RegExp,
  /// options?: { ignoreCase?, timeout? })` and
  /// `toHaveAttribute(name, options?: { timeout? })` (presence check).
  /// A second argument that is a string/RegExp is the expected value;
  /// any other object is the options bag.
  #[qjs(rename = "toHaveAttribute")]
  pub async fn to_have_attribute<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name: String,
    value: Opt<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let (expected, opts_val) = match value.0 {
          Some(v) if !v.is_undefined() && !v.is_null() => {
            if is_string_or_regex(&v) {
              (Some(v), options.0)
            } else {
              (None, Some(v))
            }
          },
          _ => (None, options.0),
        };
        let o = opts_val.as_ref().and_then(Value::as_object).cloned();
        let me = self.for_call(o.as_ref());
        let ignore_case = bool_field(o.as_ref(), "ignoreCase").unwrap_or(false);
        let e = me.build_locator_expect(&ctx, "toHaveAttribute")?;
        match expected {
          Some(v) => {
            let exp = parse_string_or_regex(&ctx, &v)?;
            e.to_have_attribute_with(&name, exp, ignore_case).await
          },
          None => e.to_have_attribute_exists(&name).await,
        }
        .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveClass(expected: string | RegExp, options?: { timeout? })`.
  /// The array form is not supported yet — it throws rather than
  /// silently comparing wrong.
  #[qjs(rename = "toHaveClass")]
  pub async fn to_have_class<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        if expected.as_array().is_some() {
          return Err(rquickjs::Error::new_from_js_message(
            "expect",
            "toHaveClass",
            "the array form is not supported yet — pass a string or RegExp",
          ));
        }
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveClass")?
          .to_have_class(exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toContainClass(expected: string, options?: { timeout? })`.
  #[qjs(rename = "toContainClass")]
  pub async fn to_contain_class<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toContainClass")?
          .to_contain_class(&expected)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveCSS(name: string, value: string | RegExp,
  /// options?: { timeout? })`.
  #[qjs(rename = "toHaveCSS")]
  pub async fn to_have_css<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name: String,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveCSS")?
          .to_have_css_with(&name, exp, ferridriver_expect::HaveCssOptions::default())
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveId(id: string | RegExp, options?: { timeout? })`.
  #[qjs(rename = "toHaveId")]
  pub async fn to_have_id<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveId")?
          .to_have_id(exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveRole(role: string, options?: { timeout? })`.
  #[qjs(rename = "toHaveRole")]
  pub async fn to_have_role<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveRole")?
          .to_have_role(StringOrRegex::String(expected))
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveJSProperty(name: string, value: any,
  /// options?: { timeout? })`.
  #[qjs(rename = "toHaveJSProperty")]
  pub async fn to_have_js_property<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name: String,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp: JsonValue = serde_from_js(&ctx, expected)?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveJSProperty")?
          .to_have_js_property(&name, exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveAccessibleName(name: string | RegExp,
  /// options?: { ignoreCase?, timeout? })`. `ignoreCase` is not
  /// supported yet — it throws rather than being silently dropped.
  #[qjs(rename = "toHaveAccessibleName")]
  pub async fn to_have_accessible_name<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        reject_unsupported_option(o.as_ref(), "toHaveAccessibleName", "ignoreCase")?;
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveAccessibleName")?
          .to_have_accessible_name(exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveAccessibleDescription(description: string | RegExp,
  /// options?: { ignoreCase?, timeout? })`. `ignoreCase` is not
  /// supported yet — it throws rather than being silently dropped.
  #[qjs(rename = "toHaveAccessibleDescription")]
  pub async fn to_have_accessible_description<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        reject_unsupported_option(o.as_ref(), "toHaveAccessibleDescription", "ignoreCase")?;
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveAccessibleDescription")?
          .to_have_accessible_description(exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  // ── Page web-first matchers (delegated) ───────────────────────────

  /// Playwright: `toHaveTitle(title: string | RegExp, options?: { timeout? })`.
  #[qjs(rename = "toHaveTitle")]
  pub async fn to_have_title<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_page_expect(&ctx, "toHaveTitle")?
          .to_have_title(exp)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveURL(url: string | RegExp, options?: { ignoreCase?, timeout? })`.
  #[qjs(rename = "toHaveURL")]
  pub async fn to_have_url<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp = parse_string_or_regex(&ctx, &expected)?;
        let o = opts_obj(&options);
        let ignore_case = bool_field(o.as_ref(), "ignoreCase").unwrap_or(false);
        self
          .for_call(o.as_ref())
          .build_page_expect(&ctx, "toHaveURL")?
          .to_have_url_with(exp, ignore_case)
          .await
          .or_else(|e| report(&ctx, e))
      })
      .await
  }

  // ── Retrying function matcher ──────────────────────────────────────

  /// Playwright: `expect(callback).toPass(options?: { intervals?, timeout? })`.
  /// Retries the callback until it stops throwing. Timeout defaults to 0
  /// (unbounded, Playwright parity); intervals default to
  /// `[100, 250, 500, 1000]`. With `.not`, passes as soon as the callback
  /// throws.
  #[qjs(rename = "toPass")]
  pub async fn to_pass<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let func = Persistent::save(&ctx, self.function_target(&ctx, "toPass")?);
        let o = opts_obj(&options);
        // Playwright: the call's bag, then `expect.toPass` from config,
        // then its own defaults (`matchers/matchers.ts:504-505`).
        let cfg = expect_defaults(&ctx);
        let timeout_ms = u64_field(o.as_ref(), "timeout").or(cfg.to_pass.timeout).unwrap_or(0);
        let intervals = u64_array_field(o.as_ref(), "intervals")
          .or_else(|| cfg.to_pass.intervals.clone())
          .unwrap_or_else(|| POLL_INTERVALS.to_vec());
        // Playwright treats timeout 0 as "no deadline"; the retry loop needs
        // a finite instant, so unbounded is modeled as a year.
        let timeout = if timeout_ms == 0 {
          Duration::from_hours(8760)
        } else {
          Duration::from_millis(timeout_ms)
        };
        let is_not = self.is_not;
        let body = || {
          let func = func.clone();
          let ctx = ctx.clone();
          async move {
            let call: rquickjs::Result<()> = async {
              let f = func.restore(&ctx)?;
              let v: Value<'_> = f.call(())?;
              if let Some(p) = v.as_promise() {
                let _: Value<'_> = p.clone().into_future().await?;
              }
              Ok(())
            }
            .await;
            match call {
              Ok(()) if !is_not => Ok(()),
              Ok(()) => Err(AssertionFailure::new(
                "the callback unexpectedly passed".to_string(),
                None,
              )),
              Err(e) if is_not => {
                // `.not.toPass` succeeds once the callback fails — but the
                // pending exception must be consumed or it leaks into the
                // next VM call.
                let _ = crate::engine::caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), "toPass");
                Ok(())
              },
              Err(e) => Err(AssertionFailure::new(
                crate::engine::caught_to_script_error(rquickjs::CaughtError::from_error(&ctx, e), "toPass").message,
                None,
              )),
            }
          }
        };
        ferridriver_expect::to_pass_with_options(
          body,
          ferridriver_expect::ToPassOptions {
            timeout,
            intervals,
            message: self.message.clone(),
          },
        )
        .await
        .or_else(|e| report(&ctx, e))
      })
      .await
  }

  // ── Snapshot matchers (test-runner host only) ────────────────────
  //
  // These need the run's snapshot directory, update mode and the
  // `image`-crate compare, all of which live runner-side — the calls
  // cross the `TestHostBridge`. Outside `ferridriver test` (MCP,
  // `ferridriver run`, BDD steps) they throw a typed error, the same
  // stance Playwright takes for snapshot assertions without a runner.

  /// Playwright: `toMatchSnapshot(name?: string)` on a string value or
  /// a locator (compares the locator's text content).
  #[qjs(rename = "toMatchSnapshot")]
  pub async fn to_match_snapshot(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'_>,
    name: Opt<String>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let bridge = crate::bindings::test::current_bridge(&ctx, "expect(...).toMatchSnapshot()")?;
        if self.is_not {
          return Err(rquickjs::Error::new_from_js_message(
            "expect",
            "toMatchSnapshot",
            "not.toMatchSnapshot is not supported",
          ));
        }
        let target = self.snapshot_target(&ctx, "toMatchSnapshot")?;
        bridge
          .match_text_snapshot(target, name.0)
          .await
          .or_else(|m| snapshot_failure(&ctx, self.is_soft, m))
      })
      .await
  }

  /// Playwright: `toHaveScreenshot(name?: string, options?)` /
  /// `toHaveScreenshot(options?)` on a locator or page.
  #[qjs(rename = "toHaveScreenshot")]
  pub async fn to_have_screenshot<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name_or_options: Opt<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let bridge = crate::bindings::test::current_bridge(&ctx, "expect(...).toHaveScreenshot()")?;
        if self.is_not {
          return Err(rquickjs::Error::new_from_js_message(
            "expect",
            "toHaveScreenshot",
            "not.toHaveScreenshot is not supported",
          ));
        }
        let (name, opts_val) = match name_or_options.0 {
          Some(v) if v.as_string().is_some() => (v.as_string().and_then(|s| s.to_string().ok()), options.0),
          Some(v) if v.as_object().is_some() => (None, Some(v)),
          _ => (None, options.0),
        };
        let opts_json: serde_json::Value = match opts_val {
          Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(&ctx, v)?,
          _ => serde_json::json!({}),
        };
        let target = self.snapshot_target(&ctx, "toHaveScreenshot")?;
        bridge
          .match_screenshot(target, name, opts_json)
          .await
          .or_else(|m| snapshot_failure(&ctx, self.is_soft, m))
      })
      .await
  }

  /// Playwright: `toMatchAriaSnapshot(expected: string, options?: { timeout? })`
  /// on a locator or page.
  #[qjs(rename = "toMatchAriaSnapshot")]
  pub async fn to_match_aria_snapshot<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let bridge = crate::bindings::test::current_bridge(&ctx, "expect(...).toMatchAriaSnapshot()")?;
        let o = opts_obj(&options);
        let timeout_ms = u64_field(o.as_ref(), "timeout");
        let target = self.snapshot_target(&ctx, "toMatchAriaSnapshot")?;
        bridge
          .match_aria_snapshot(target, expected, self.is_not, timeout_ms)
          .await
          .or_else(|m| snapshot_failure(&ctx, self.is_soft, m))
      })
      .await
  }

  // ── APIResponse matcher (delegated) ──────────────────────────────

  #[qjs(rename = "toBeOK")]
  pub fn to_be_ok(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self
      .build_api_response_expect(&ctx, "toBeOK")?
      .to_be_ok()
      .or_else(|e| report(&ctx, e))
  }
}

fn parse_throw_matcher<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ThrowMatcher> {
  if let Some(s) = value.as_string() {
    return Ok(ThrowMatcher::Substring(s.to_string()?));
  }
  if let Some(obj) = value.as_object() {
    if let Ok(source) = obj.get::<_, rquickjs::Value<'js>>("source")
      && let Some(s) = source.as_string()
    {
      let flags = obj
        .get::<_, rquickjs::Value<'js>>("flags")
        .ok()
        .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
        .unwrap_or_default();
      let pat = s.to_string()?;
      let re = ferridriver_expect::asymmetric::compile_js_regex(&pat, &flags)
        .map_err(|e| rquickjs::Error::new_from_js_message("expect", "RegExp", e.to_string()))?;
      return Ok(ThrowMatcher::Regex(re));
    }
    // Plain object → treat as match-against-{message,name}
    let json: JsonValue = serde_from_js(ctx, value)?;
    return Ok(ThrowMatcher::Object(json));
  }
  if let Some(func) = value.as_function() {
    let name: String = func
      .get::<_, rquickjs::Value<'js>>("name")
      .ok()
      .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
      .unwrap_or_default();
    if !name.is_empty() {
      return Ok(ThrowMatcher::ClassName(name));
    }
  }
  Ok(ThrowMatcher::Any)
}

fn extract_error<'js>(v: &Value<'js>) -> (String, Option<String>) {
  if let Some(obj) = v.as_object() {
    let msg = obj
      .get::<_, rquickjs::Value<'js>>("message")
      .ok()
      .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
      .unwrap_or_default();
    let name = obj
      .get::<_, rquickjs::Value<'js>>("name")
      .ok()
      .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
      .filter(|s| !s.is_empty());
    return (msg, name);
  }
  if let Some(s) = v.as_string() {
    return (s.to_string().unwrap_or_default(), None);
  }
  (String::new(), None)
}

// ── ExpectPollJs ─────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "ExpectPoll")]
pub struct ExpectPollJs {
  #[qjs(skip_trace)]
  generator: Persistent<Function<'static>>,
  #[qjs(skip_trace)]
  timeout: Duration,
  #[qjs(skip_trace)]
  intervals: Vec<u64>,
  is_not: bool,
  message: Option<String>,
}

#[rquickjs::methods]
impl ExpectPollJs {
  #[qjs(rename = "withTimeout")]
  pub fn with_timeout(&self, timeout_ms: u32) -> ExpectPollJs {
    ExpectPollJs {
      generator: self.generator.clone(),
      timeout: Duration::from_millis(u64::from(timeout_ms)),
      intervals: self.intervals.clone(),
      is_not: self.is_not,
      message: self.message.clone(),
    }
  }

  #[qjs(rename = "_notInner")]
  pub fn not_inner(&self) -> ExpectPollJs {
    ExpectPollJs {
      generator: self.generator.clone(),
      timeout: self.timeout,
      intervals: self.intervals.clone(),
      is_not: !self.is_not,
      message: self.message.clone(),
    }
  }

  /// `expect.poll(fn).toBe(x)` is the same `Object.is` the non-polling
  /// matcher is, applied to each generated value while it is still live.
  #[qjs(rename = "toBe")]
  pub async fn to_be<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
  ) -> rquickjs::Result<()> {
    let expected = Persistent::save(&ctx, expected);
    call_site
      .scope(async move {
        let generator_fn = self.generator.clone();
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut interval_idx = 0;
        let last: String = loop {
          let actual = JsLive(call_generator_live(&ctx, &generator_fn).await?);
          let want = JsLive(expected.clone().restore(&ctx)?);
          let pass_raw = actual.same_value(&want)?;
          if if self.is_not { !pass_raw } else { pass_raw } {
            return Ok(());
          }
          let described = actual.describe();
          let now = tokio::time::Instant::now();
          if now >= deadline {
            break described;
          }
          let interval_ms = self
            .intervals
            .get(interval_idx)
            .copied()
            .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
          interval_idx += 1;
          tokio::time::sleep(Duration::from_millis(interval_ms).min(deadline - now)).await;
        };
        let want = JsLive(expected.clone().restore(&ctx)?).describe();
        let prefix = self.message.as_ref().map(|m| format!("{m}: ")).unwrap_or_default();
        Err(assertion_to_rq(
          &ctx,
          AssertionFailure::new(
            format!(
              "{prefix}expect.poll().toBe() timed out after {}ms\n\nExpected: {want}\nReceived: {last}",
              self.timeout.as_millis()
            ),
            None,
          ),
        ))
      })
      .await
  }

  #[qjs(rename = "toEqual")]
  pub async fn to_equal<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let exp: JsonValue = serde_from_js(&ctx, expected)?;
        self.poll_value(&ctx, "toEqual", &exp).await
      })
      .await
  }

  #[qjs(rename = "toSatisfy")]
  pub async fn to_satisfy<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    predicate: Function<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let saved_pred = Persistent::save(&ctx, predicate);
        let generator_fn = self.generator.clone();
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut interval_idx = 0;
        let is_not = self.is_not;
        let final_dbg: String = loop {
          let actual: rquickjs::Result<JsonValue> = call_generator(&ctx, &generator_fn).await;
          let actual = actual?;
          let dbg = ferridriver_expect::asymmetric::json_short(&actual);
          let pred = saved_pred.clone().restore(&ctx)?;
          let actual_js = json_to_js(&ctx, &actual)?;
          let result: rquickjs::Value<'_> = pred.call((actual_js,))?;
          let passes = result.as_bool().unwrap_or(false);
          let passes = if is_not { !passes } else { passes };
          if passes {
            return Ok(());
          }
          let interval_ms = self
            .intervals
            .get(interval_idx)
            .copied()
            .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
          interval_idx += 1;
          // Clamp the interval to the remaining budget so the final
          // attempt lands AT the deadline instead of bailing early.
          let now = tokio::time::Instant::now();
          if now >= deadline {
            break dbg;
          }
          let sleep_dur = Duration::from_millis(interval_ms).min(deadline - now);
          tokio::time::sleep(sleep_dur).await;
        };
        let last = final_dbg.as_str();
        Err(assertion_to_rq(
          &ctx,
          AssertionFailure::new(
            format!(
              "expect.poll().toSatisfy() timed out after {}ms; last value was {last}",
              self.timeout.as_millis()
            ),
            None,
          ),
        ))
      })
      .await
  }
}

impl ExpectPollJs {
  async fn poll_value(&self, ctx: &Ctx<'_>, method: &str, expected: &JsonValue) -> rquickjs::Result<()> {
    let generator_fn = self.generator.clone();
    let deadline = tokio::time::Instant::now() + self.timeout;
    let mut interval_idx = 0;
    let is_not = self.is_not;
    let last: JsonValue = loop {
      let actual: JsonValue = call_generator(ctx, &generator_fn).await?;
      let pass_raw = deep_equal(&actual, expected);
      let pass = if is_not { !pass_raw } else { pass_raw };
      if pass {
        return Ok(());
      }
      let interval_ms = self
        .intervals
        .get(interval_idx)
        .copied()
        .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
      interval_idx += 1;
      // Clamp the interval to the remaining budget so the final
      // attempt lands AT the deadline instead of bailing early.
      let now = tokio::time::Instant::now();
      if now >= deadline {
        break actual;
      }
      let sleep_dur = Duration::from_millis(interval_ms).min(deadline - now);
      tokio::time::sleep(sleep_dur).await;
    };
    Err(assertion_to_rq(
      ctx,
      AssertionFailure::new(
        format!(
          "expect.poll().{method}() timed out after {}ms\n\nExpected: {}\nReceived: {}",
          self.timeout.as_millis(),
          ferridriver_expect::asymmetric::json_short(expected),
          ferridriver_expect::asymmetric::json_short(&last)
        ),
        None,
      ),
    ))
  }
}

async fn call_generator(ctx: &Ctx<'_>, generator_fn: &Persistent<Function<'static>>) -> rquickjs::Result<JsonValue> {
  serde_from_js(ctx, call_generator_live(ctx, generator_fn).await?)
}

/// One round of the poll generator, awaited, with the result left as the
/// live JS value the identity matchers need.
async fn call_generator_live<'js>(
  ctx: &Ctx<'js>,
  generator_fn: &Persistent<Function<'static>>,
) -> rquickjs::Result<Value<'js>> {
  let f = generator_fn.clone().restore(ctx)?;
  let result: rquickjs::Value<'js> = f.call(())?;
  // Await the result if it's a thenable.
  if let Some(promise) = result.as_promise() {
    promise.clone().into_future::<rquickjs::Value<'js>>().await
  } else {
    Ok(result)
  }
}

// ── factory + asymmetric helpers ─────────────────────────────────────

/// Construct an [`ExpectJs`] from any JS value.
///
/// The value is kept as-is; the typed handle it resolves to (if any)
/// only decides which web-first matchers apply. No snapshot is taken
/// here — `expect(x)` itself never fails, exactly as upstream.
fn build_expect<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> ExpectJs {
  let kind = if let Ok(class) = Class::<LocatorJs>::from_value(&value) {
    let loc = class.borrow().inner_ref().clone();
    SubjectKind::Locator(loc)
  } else if let Ok(class) = Class::<PageJs>::from_value(&value) {
    SubjectKind::Page(class.borrow().page_arc())
  } else if let Ok(class) = Class::<HttpResponseJs>::from_value(&value) {
    SubjectKind::ApiResponse(class.borrow().inner_clone())
  } else {
    SubjectKind::Value
  };
  ExpectJs::new(
    ExpectSubject {
      live: Persistent::save(ctx, value),
      kind,
    },
    default_timeout(ctx),
  )
}

/// Playwright: `expect(actual, messageOrOptions?: string | { message?: string })`
/// (`types/test.d.ts:8934`) — the same trailing argument `expect.soft`
/// and `expect.poll` take.
fn custom_message<'js>(value: Option<&Value<'js>>) -> rquickjs::Result<Option<String>> {
  let Some(v) = value else { return Ok(None) };
  if let Some(s) = v.as_string() {
    return Ok(Some(s.to_string()?));
  }
  let Some(obj) = v.as_object() else { return Ok(None) };
  let message: Value<'js> = obj.get("message")?;
  Ok(message.as_string().and_then(|s| s.to_string().ok()))
}

/// Map a live-matcher outcome onto the JS error it throws: an assertion
/// failure becomes an `AssertionError`, a misuse becomes a real
/// `TypeError` (Playwright throws those and `.not` does not flip them),
/// and a JS exception raised while reading the value propagates as-is.
fn live_report(ctx: &Ctx<'_>, err: LiveError<rquickjs::Error>) -> rquickjs::Result<()> {
  match err {
    LiveError::Failed(f) => report(ctx, f),
    LiveError::BadInput(b) => Err(crate::bindings::convert::throw_named(ctx, "TypeError", b.to_string())),
    LiveError::Host(e) => Err(e),
  }
}

fn make_asymmetric<'js>(ctx: &Ctx<'js>, tag: &str, payload: Object<'js>) -> rquickjs::Result<Object<'js>> {
  payload.set(ferridriver_expect::ASYM_TAG_KEY, tag)?;
  let _ = ctx;
  Ok(payload)
}

/// Install the `expect` global. Exposes:
/// - `expect(value | locator | page | apiResponse | fn) -> Expect`
/// - `expect.poll(fn, opts?) -> ExpectPoll`
/// - `expect.soft(target) -> Expect` (with `.is_soft` set)
/// - Asymmetric matchers: `any`, `anything`, `arrayContaining`,
///   `objectContaining`, `stringContaining`, `stringMatching`,
///   `closeTo`, plus the `expect.not.*` shorthand.
pub fn install_expect<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
  // Define the class prototype once so `expect(x)` can return
  // `ExpectJs` instances JS can call methods on.
  Class::<ExpectJs>::define(&ctx.globals())?;
  Class::<ExpectPollJs>::define(&ctx.globals())?;

  let root = create_expect(ctx, &ferridriver_expect::ExpectMeta::default(), 0)?;
  ctx.globals().set("expect", root)?;
  crate::bindings::runtime::mirror_global(ctx, "expect")?;

  // Playwright exports `mergeExpects` from `@playwright/test`; the
  // module surface reads it off the globals like `expect` itself.
  let merge = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, expects: rquickjs::function::Rest<Value<'js>>| -> rquickjs::Result<Function<'js>> {
      merge_expects(&ctx, expects.0)
    },
  )?;
  merge.set_name("mergeExpects")?;
  ctx.globals().set("mergeExpects", merge)?;
  crate::bindings::runtime::mirror_global(ctx, "mergeExpects")?;
  Ok(())
}

/// Build one `expect` — Playwright's `createExpect(info)`.
///
/// `meta` is the assertion state this expect applies (message, soft,
/// timeout, and the NAMES of its custom matchers); `table` indexes the
/// JS object holding those matchers' functions. Both are plain data, so
/// the closures below capture no JS value.
fn create_expect<'js>(
  ctx: &Ctx<'js>,
  meta: &ferridriver_expect::ExpectMeta<()>,
  table: u32,
) -> rquickjs::Result<Function<'js>> {
  let call_meta = meta.clone();
  let expect_fn = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, value: Value<'js>, message: Opt<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let mut inst = build_expect(&ctx, value);
      inst.message = custom_message(message.0.as_ref())?.or_else(|| call_meta.message.clone());
      inst.is_soft = call_meta.is_soft;
      if let Some(timeout) = call_meta.timeout {
        inst.timeout = timeout;
      }
      inst.matcher_table = table;
      // Read the names off the table, not off the captured meta: an
      // `expect.extend({...})` whose result was discarded publishes its
      // new names by mutating the table this expect already points at
      // (Playwright's legacy behavior).
      inst.matcher_names = table_names(&ctx, table)?;
      let class = Class::instance(ctx.clone(), inst)?;
      // Wrap in the JS proxy that translates `.not` (a getter) to
      // `_notInner()` (the method-bound clone).
      {
        let val = class.into_value();
        install_not_getter(&ctx, &val)?;
        install_settled_getters(&ctx, &val)?;
        install_user_matchers(&ctx, &val)?;
        Ok(val)
      }
    },
  )?;
  expect_fn.set_name("expect")?;

  // Playwright: `expect.poll(actual, messageOrOptions?: string |
  // { message?, timeout?, intervals? })`.
  let poll_meta = meta.clone();
  let poll_fn = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, generator: Function<'js>, opts: Opt<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let o = opts_obj(&opts);
      let default_timeout = poll_meta.timeout.unwrap_or_else(|| default_timeout(&ctx)).as_millis() as u64;
      let timeout_ms = u64_field(o.as_ref(), "timeout").unwrap_or(default_timeout);
      let intervals = u64_array_field(o.as_ref(), "intervals").unwrap_or_else(|| POLL_INTERVALS.to_vec());
      let saved = Persistent::save(&ctx, generator);
      let inst = ExpectPollJs {
        generator: saved,
        timeout: Duration::from_millis(timeout_ms),
        intervals,
        is_not: false,
        message: custom_message(opts.0.as_ref())?.or_else(|| poll_meta.message.clone()),
      };
      let class = Class::instance(ctx.clone(), inst)?;
      {
        let val = class.into_value();
        install_poll_not_getter(&ctx, &val)?;
        install_poll_settled_refusal(&ctx, &val)?;
        Ok(val)
      }
    },
  )?;

  // Asymmetric matcher factories.
  let any_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, ctor: Value<'js>| -> rquickjs::Result<Object<'js>> {
      let name = ctor
        .as_function()
        .and_then(|f| f.get::<_, rquickjs::Value<'js>>("name").ok())
        .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
        .unwrap_or_else(|| "Object".into());
      let obj = Object::new(ctx.clone())?;
      obj.set("name", name)?;
      make_asymmetric(&ctx, "any", obj)
    },
  )?;
  let anything_fn = Function::new(ctx.clone(), |ctx: Ctx<'js>| -> rquickjs::Result<Object<'js>> {
    make_asymmetric(&ctx, "anything", Object::new(ctx.clone())?)
  })?;
  let array_containing_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, items: Array<'js>| -> rquickjs::Result<Object<'js>> {
      let obj = Object::new(ctx.clone())?;
      obj.set("items", items)?;
      make_asymmetric(&ctx, "arrayContaining", obj)
    },
  )?;
  let object_containing_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, subset: Object<'js>| -> rquickjs::Result<Object<'js>> {
      let obj = Object::new(ctx.clone())?;
      obj.set("subset", subset)?;
      make_asymmetric(&ctx, "objectContaining", obj)
    },
  )?;
  let string_containing_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, s: String| -> rquickjs::Result<Object<'js>> {
      let obj = Object::new(ctx.clone())?;
      obj.set("substring", s)?;
      make_asymmetric(&ctx, "stringContaining", obj)
    },
  )?;
  let string_matching_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, pat: Value<'js>| -> rquickjs::Result<Object<'js>> {
      let obj = Object::new(ctx.clone())?;
      if let Some(s) = pat.as_string() {
        obj.set("substring", s.to_string()?)?;
      } else if let Some(re_obj) = pat.as_object() {
        let source = re_obj.get::<_, rquickjs::Value<'js>>("source")?;
        let flags = re_obj
          .get::<_, rquickjs::Value<'js>>("flags")
          .unwrap_or(Value::new_undefined(ctx.clone()));
        if let Some(s) = source.as_string() {
          obj.set("regex", s.to_string()?)?;
        }
        if let Some(f) = flags.as_string() {
          obj.set("flags", f.to_string()?)?;
        }
      } else {
        return Err(rquickjs::Error::new_from_js_message(
          "expect",
          "argument",
          "expect.stringMatching expects a string or RegExp",
        ));
      }
      make_asymmetric(&ctx, "stringMatching", obj)
    },
  )?;
  // Playwright: `expect.arrayOf(sample)` — an array whose EVERY item
  // matches (`expectLibrary.ts:335`).
  let array_of_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, sample: Value<'js>| -> rquickjs::Result<Object<'js>> {
      let obj = Object::new(ctx.clone())?;
      obj.set("sample", sample)?;
      make_asymmetric(&ctx, "arrayOf", obj)
    },
  )?;
  let close_to_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, value: f64, digits: Opt<u8>| -> rquickjs::Result<Object<'js>> {
      let obj = Object::new(ctx.clone())?;
      obj.set("value", value)?;
      obj.set("digits", digits.0.unwrap_or(2))?;
      make_asymmetric(&ctx, "closeTo", obj)
    },
  )?;

  // expect.not.<asym>(...) — wraps an asymmetric matcher in a NOT
  // tag. Mirrors Jest's `expect.not.objectContaining` etc. The wrappers
  // resolve `expect.<name>` from globals at call time (see the no-capture
  // rule below).
  let not_obj = Object::new(ctx.clone())?;
  install_not_asym(ctx, &not_obj, "any")?;
  install_not_asym(ctx, &not_obj, "anything")?;
  install_not_asym(ctx, &not_obj, "arrayContaining")?;
  install_not_asym(ctx, &not_obj, "objectContaining")?;
  install_not_asym(ctx, &not_obj, "stringContaining")?;
  install_not_asym(ctx, &not_obj, "stringMatching")?;
  install_not_asym(ctx, &not_obj, "closeTo")?;
  install_not_asym(ctx, &not_obj, "arrayOf")?;

  // Attach the helpers to expect()'s own properties.
  let expect_obj = expect_fn.as_object().ok_or_else(|| {
    rquickjs::Error::new_from_js_message("expect", "install", "expect Function has no object representation")
  })?;
  expect_obj.set("poll", poll_fn)?;
  expect_obj.set("any", any_fn)?;
  expect_obj.set("anything", anything_fn)?;
  expect_obj.set("arrayContaining", array_containing_fn)?;
  expect_obj.set("objectContaining", object_containing_fn)?;
  expect_obj.set("stringContaining", string_containing_fn)?;
  expect_obj.set("stringMatching", string_matching_fn)?;
  expect_obj.set("closeTo", close_to_fn)?;
  expect_obj.set("arrayOf", array_of_fn)?;
  expect_obj.set("not", not_obj.clone())?;
  // Playwright publishes every custom matcher as an asymmetric one too
  // (`buildCustomAsymmetricMatcher`), so `expect.toBeX()` can stand in
  // for a value inside `toEqual`.
  for name in table_names(ctx, table)? {
    install_custom_asym(ctx, expect_obj, &not_obj, &name)?;
  }
  install_expect_meta(ctx, expect_obj, meta, table)?;
  Ok(expect_fn)
}

/// `extend` / `configure` / `getState` / `soft`, plus the hidden state
/// `mergeExpects` reads back off another expect.
fn install_expect_meta<'js>(
  ctx: &Ctx<'js>,
  expect_obj: &Object<'js>,
  meta: &ferridriver_expect::ExpectMeta<()>,
  table: u32,
) -> rquickjs::Result<()> {
  define_hidden(ctx, expect_obj, EXPECT_TABLE, table.into_js(ctx)?)?;

  let extend_meta = meta.clone();
  let extend = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, matchers: Value<'js>| -> rquickjs::Result<Function<'js>> {
      let Some(additions) = matchers.as_object() else {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "TypeError",
          ferridriver_expect::not_a_matcher_message("matchers", &type_name(&matchers)),
        ));
      };
      extend_expect(&ctx, &extend_meta, table, additions)
    },
  )?;
  expect_obj.set("extend", extend)?;

  let configure_meta = meta.clone();
  let configure = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, config: Opt<Value<'js>>| -> rquickjs::Result<Function<'js>> {
      let cfg = parse_configure(&ctx, config.0.as_ref())?;
      create_expect(&ctx, &configure_meta.configure(&cfg), table)
    },
  )?;
  expect_obj.set("configure", configure)?;

  // Jest compatibility: Playwright answers an empty state object.
  let get_state = Function::new(ctx.clone(), |ctx: Ctx<'js>| -> rquickjs::Result<Object<'js>> {
    Object::new(ctx.clone())
  })?;
  expect_obj.set("getState", get_state)?;

  // `expect.soft` is a getter returning an Expect, so `expect.soft(x)`
  // and `expect.soft.poll(...)` are both the soft expect. An already
  // soft expect answers itself.
  let soft_meta = meta.clone();
  let soft_getter = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, this: rquickjs::function::This<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      if soft_meta.is_soft {
        return Ok(this.0);
      }
      Ok(create_expect(&ctx, &soft_meta.softened(), table)?.into_value())
    },
  )?;
  define_accessor(ctx, expect_obj.as_value(), "soft", soft_getter)?;
  Ok(())
}

fn type_name(value: &Value<'_>) -> String {
  JsLive(value.clone()).js_type().name().to_string()
}

/// `expect.configure({ message?, timeout?, soft? })` — presence is what
/// decides, so a key given as `undefined` CLEARS rather than keeps.
fn parse_configure<'js>(
  ctx: &Ctx<'js>,
  config: Option<&Value<'js>>,
) -> rquickjs::Result<ferridriver_expect::ExpectConfigure> {
  let mut out = ferridriver_expect::ExpectConfigure::default();
  let Some(obj) = config.and_then(Value::as_object) else {
    return Ok(out);
  };
  let _ = ctx;
  if obj.contains_key("message").unwrap_or(false) {
    let v: Value<'js> = obj.get("message")?;
    out.message = ferridriver_expect::Setting::from_optional(true, v.as_string().and_then(|s| s.to_string().ok()));
  }
  if obj.contains_key("timeout").unwrap_or(false) {
    let v: Value<'js> = obj.get("timeout")?;
    out.timeout =
      ferridriver_expect::Setting::from_optional(true, v.as_number().map(|ms| Duration::from_millis(ms as u64)));
  }
  if obj.contains_key("soft").unwrap_or(false) {
    let v: Value<'js> = obj.get("soft")?;
    out.soft = Some(v.as_bool().unwrap_or(false));
  }
  Ok(out)
}

/// The body of `expect.extend`, both halves of it.
fn extend_expect<'js>(
  ctx: &Ctx<'js>,
  meta: &ferridriver_expect::ExpectMeta<()>,
  table: u32,
  additions: &Object<'js>,
) -> rquickjs::Result<Function<'js>> {
  let mut names = Vec::new();
  for entry in additions.props::<String, Value<'js>>() {
    let (name, value) = entry?;
    if !value.is_function() {
      return Err(crate::bindings::convert::throw_named(
        ctx,
        "TypeError",
        ferridriver_expect::not_a_matcher_message(&name, &type_name(&value)),
      ));
    }
    names.push((name, value));
  }

  // Playwright's legacy half: a NON-builtin name also lands on the
  // expect `extend` was called on, so `expect.extend({...})` works
  // without capturing the return value. A built-in name is only
  // shadowed on the expect this returns.
  let current = matcher_table(ctx, table)?;
  for (name, value) in &names {
    if !ferridriver_expect::is_builtin_matcher(name) {
      current.set(name.as_str(), value.clone())?;
    }
  }

  // The returned expect gets its own table carrying everything.
  let next = Object::new(ctx.clone())?;
  for entry in current.props::<String, Value<'js>>() {
    let (name, value) = entry?;
    next.set(name, value)?;
  }
  for (name, value) in &names {
    next.set(name.as_str(), value.clone())?;
  }
  let index = push_onto(ctx, &matcher_tables(ctx)?, next.clone().into_value())?;
  let mut published: Vec<(String, ())> = Vec::new();
  for entry in next.props::<String, Value<'js>>() {
    published.push((entry?.0, ()));
  }
  create_expect(ctx, &meta.extended(published), index)
}

/// `mergeExpects(...expects)` — fold every argument's matchers into one
/// expect, left to right. A foreign expect (no table of ours) is
/// skipped, as upstream.
pub fn merge_expects<'js>(ctx: &Ctx<'js>, expects: Vec<Value<'js>>) -> rquickjs::Result<Function<'js>> {
  let merged = Object::new(ctx.clone())?;
  for e in expects {
    let Some(obj) = e.as_object() else { continue };
    let Some(index) = obj.get::<_, Value<'js>>(EXPECT_TABLE).ok().and_then(|v| v.as_number()) else {
      continue;
    };
    let table = matcher_table(ctx, index as u32)?;
    for entry in table.props::<String, Value<'js>>() {
      let (name, value) = entry?;
      merged.set(name, value)?;
    }
  }
  let index = push_onto(ctx, &matcher_tables(ctx)?, merged.clone().into_value())?;
  let mut published: Vec<(String, ())> = Vec::new();
  for entry in merged.props::<String, Value<'js>>() {
    published.push((entry?.0, ()));
  }
  let meta = ferridriver_expect::ExpectMeta::default().extended(published);
  create_expect(ctx, &meta, index)
}

// A native closure must NEVER capture a live JS value (`Function`,
// `Object`, `Value`, or a `Persistent` of one): the value owns a `Ctx`
// (a JSContext refcount), so a JS object holding such a closure forms a
// cross-language reference cycle QuickJS's GC cannot trace. The cycle
// survives until `JS_FreeRuntime`, which then aborts on its
// `gc_obj_list` assertion when the session VM is dropped. Closures here
// re-resolve what they need at call time instead (globals lookup /
// `This`).

/// `expect.<name>(...args)` and `expect.not.<name>(...args)` for a
/// matcher registered through `expect.extend`. The tag carries the name
/// and the arguments; the body runs later, through the evaluator the
/// structural matchers install.
fn install_custom_asym<'js>(
  ctx: &Ctx<'js>,
  expect_obj: &Object<'js>,
  not_obj: &Object<'js>,
  name: &str,
) -> rquickjs::Result<()> {
  let owned = name.to_string();
  let positive = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: rquickjs::function::Rest<Value<'js>>| -> rquickjs::Result<Object<'js>> {
      custom_asym_tag(&ctx, &owned, args.0)
    },
  )?;
  expect_obj.set(name, positive)?;
  let owned = name.to_string();
  let inverse = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: rquickjs::function::Rest<Value<'js>>| -> rquickjs::Result<Object<'js>> {
      let inner = custom_asym_tag(&ctx, &owned, args.0)?;
      let wrapper = Object::new(ctx.clone())?;
      wrapper.set("inner", inner)?;
      make_asymmetric(&ctx, "not", wrapper)
    },
  )?;
  not_obj.set(name, inverse)?;
  Ok(())
}

fn custom_asym_tag<'js>(ctx: &Ctx<'js>, name: &str, args: Vec<Value<'js>>) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("name", name)?;
  let arr = Array::new(ctx.clone())?;
  for (i, arg) in args.into_iter().enumerate() {
    arr.set(i, arg)?;
  }
  obj.set("args", arr)?;
  make_asymmetric(ctx, "custom", obj)
}

/// Runs a custom matcher as an asymmetric one, mid-comparison. The
/// values reaching it are the structural view (JSON), which is what the
/// comparison itself is working with.
struct TableEvaluator<'js> {
  ctx: Ctx<'js>,
  table: u32,
  timeout: Duration,
}

impl ferridriver_expect::CustomAsymmetric for TableEvaluator<'_> {
  fn matches(&self, name: &str, args: &[JsonValue], actual: &JsonValue) -> bool {
    let run = || -> rquickjs::Result<bool> {
      let body: Function<'_> = matcher_table(&self.ctx, self.table)?.get(name)?;
      let cx = ferridriver_expect::MatcherContext {
        is_not: false,
        is_soft: false,
        promise: None,
        timeout: self.timeout,
        custom_message: None,
      };
      let mut call_args = vec![json_to_js(&self.ctx, actual)?];
      for arg in args {
        call_args.push(json_to_js(&self.ctx, arg)?);
      }
      let outcome: Value<'_> = body.call((
        rquickjs::function::This(matcher_context(&self.ctx, &cx)?),
        rquickjs::function::Rest(call_args),
      ))?;
      if outcome.as_promise().is_some() {
        // An asymmetric matcher is used mid-comparison, where there is
        // nothing to await into — jest's `asymmetricMatch` is sync too.
        return Err(crate::bindings::convert::throw_named(
          &self.ctx,
          "TypeError",
          format!("expect.{name}() is asynchronous and cannot be used as an asymmetric matcher"),
        ));
      }
      Ok(parse_matcher_result(&self.ctx, name, &outcome)?.pass)
    };
    run().unwrap_or_else(|_| {
      // The comparison has no way to carry an error; the pending
      // exception must still be cleared or it leaks into the next call.
      let _ = self.ctx.catch();
      false
    })
  }
}

impl<'js> ExpectJs {
  /// This assertion's custom matchers, ready to answer as asymmetric
  /// ones inside a structural comparison.
  fn evaluator(&self, ctx: &Ctx<'js>) -> Option<TableEvaluator<'js>> {
    (!self.matcher_names.is_empty()).then(|| TableEvaluator {
      ctx: ctx.clone(),
      table: self.matcher_table,
      timeout: self.timeout,
    })
  }
}

fn install_not_asym<'js>(ctx: &Ctx<'js>, not_obj: &Object<'js>, name: &'static str) -> rquickjs::Result<()> {
  let wrapped = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: rquickjs::function::Rest<Value<'js>>| -> rquickjs::Result<Object<'js>> {
      let expect_obj: Object<'js> = ctx.globals().get("expect")?;
      let inner: Function<'js> = expect_obj.get(name)?;
      let inner_obj: Object<'js> = inner.call((rquickjs::function::Rest(args.0),))?;
      let wrapper = Object::new(ctx.clone())?;
      wrapper.set("inner", inner_obj)?;
      make_asymmetric(&ctx, "not", wrapper)
    },
  )?;
  not_obj.set(name, wrapped)?;
  Ok(())
}

/// Install a `.not` getter directly on the class instance via
/// `Object.defineProperty` — avoids a JS `Proxy` wrapper (which would
/// break the `#[qjs] fn (&self, ...)` receiver translation when the
/// matcher is called) and matches Jest's `.not.toBe(...)` chain shape.
/// The getter reads the instance from `this` rather than capturing it.
fn install_not_getter<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let getter = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, this: rquickjs::function::This<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let class = Class::<ExpectJs>::from_value(&this.0)?;
      let inverted = class.borrow().not_inner();
      let new_class = Class::instance(ctx.clone(), inverted)?;
      let new_val = new_class.into_value();
      install_not_getter(&ctx, &new_val)?;
      install_user_matchers(&ctx, &new_val)?;
      Ok(new_val)
    },
  )?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter)?;
  descriptor.set("configurable", true)?;
  let _: rquickjs::Value<'js> = define_property.call((instance.clone(), "not", descriptor))?;
  Ok(())
}

// ── expect.extend ────────────────────────────────────────────────────
//
// Every rule here — what `extend` composes, which names it may publish,
// how a matcher's result becomes a verdict, what the failure says — is
// `ferridriver_expect::extend`, so a Rust test registering a matcher and
// a JS suite calling `expect.extend` behave identically. This module
// only marshals JS values across that seam.
//
// The matcher FUNCTIONS live in JS objects hung off the Expect
// prototype, indexed by number. A native closure captures the index and
// the name — plain data — and looks the function up at call time, so no
// closure ever holds a JS value.

/// Where the per-expect matcher tables hang. Non-enumerable, and the
/// name does not start with `to`, so it never reads back as a matcher.
const MATCHER_TABLES: &str = "_matcherTables";
/// Hidden on each expect function, so `mergeExpects` can read another
/// expect's matchers without a registry.
const EXPECT_TABLE: &str = "_matcherTable";

fn matcher_tables<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Array<'js>> {
  let proto = Class::<ExpectJs>::prototype(ctx)?.ok_or_else(|| {
    rquickjs::Error::new_from_js_message("expect", "extend", "the Expect class has no prototype in this context")
  })?;
  let existing: Value<'js> = proto.get(MATCHER_TABLES)?;
  if let Some(arr) = existing.as_array() {
    return Ok(arr.clone());
  }
  let arr = Array::new(ctx.clone())?;
  // Index 0 is the empty table every un-extended expect points at.
  let empty = Object::new(ctx.clone())?;
  push_onto(ctx, &arr, empty.into_value())?;
  define_hidden(ctx, &proto, MATCHER_TABLES, arr.clone().into_value())?;
  Ok(arr)
}

fn push_onto<'js>(ctx: &Ctx<'js>, arr: &Array<'js>, value: Value<'js>) -> rquickjs::Result<u32> {
  let push: Function<'js> = arr.as_object().get("push")?;
  let len: f64 = push.call((rquickjs::function::This(arr.clone()), value))?;
  let _ = ctx;
  Ok(len as u32 - 1)
}

/// The matcher names currently in `table`.
fn table_names(ctx: &Ctx<'_>, index: u32) -> rquickjs::Result<Vec<String>> {
  let table = matcher_table(ctx, index)?;
  let mut names = Vec::new();
  for key in table.keys::<String>() {
    names.push(key?);
  }
  Ok(names)
}

fn matcher_table<'js>(ctx: &Ctx<'js>, index: u32) -> rquickjs::Result<Object<'js>> {
  let tables = matcher_tables(ctx)?;
  let entry: Value<'js> = tables.get(index as usize)?;
  entry
    .as_object()
    .cloned()
    .ok_or_else(|| rquickjs::Error::new_from_js_message("expect", "extend", "this expect's matcher table is missing"))
}

/// The `this` a matcher body reads — Playwright's `MatcherContext`.
fn matcher_context<'js>(ctx: &Ctx<'js>, cx: &ferridriver_expect::MatcherContext) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("isNot", cx.is_not)?;
  obj.set("isSoft", cx.is_soft)?;
  obj.set("promise", cx.promise.map_or("", PromiseMode::as_str))?;
  obj.set("timeout", cx.timeout.as_millis() as f64)?;
  obj.set("utils", matcher_utils(ctx)?)?;
  // Playwright hands custom matchers an `equals` that refuses to run:
  // a jest matcher relying on it is not compatible with this runner.
  let equals = Function::new(ctx.clone(), |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
    Err(crate::bindings::convert::throw_named(
      &ctx,
      "Error",
      "It looks like you are using custom expect matchers that are not compatible with ferridriver".to_string(),
    ))
  })?;
  obj.set("equals", equals)?;
  Ok(obj)
}

/// The subset of jest's matcher-utils ferridriver can honestly answer.
fn matcher_utils<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let utils = Object::new(ctx.clone())?;
  for name in ["printReceived", "printExpected", "stringify"] {
    let render = Function::new(
      ctx.clone(),
      |_ctx: Ctx<'js>, value: Value<'js>| -> rquickjs::Result<String> { Ok(JsLive(value).describe()) },
    )?;
    utils.set(name, render)?;
  }
  let hint = Function::new(
    ctx.clone(),
    |_ctx: Ctx<'js>, name: String, args: rquickjs::function::Rest<Value<'js>>| -> rquickjs::Result<String> {
      let _ = args;
      Ok(format!("expect(received).{name}(expected)"))
    },
  )?;
  utils.set("matcherHint", hint)?;
  Ok(utils)
}

/// Playwright: `toHaveProperty(path, value?)` takes `"a.b.c"` or an
/// array of keys and indexes.
fn parse_property_path<'js>(
  ctx: &Ctx<'js>,
  path: &Value<'js>,
) -> rquickjs::Result<Vec<ferridriver_expect::PropSegment>> {
  use ferridriver_expect::PropSegment;
  if let Some(s) = path.as_string() {
    return Ok(
      s.to_string()?
        .split('.')
        .map(|p| PropSegment::Key(p.to_string()))
        .collect(),
    );
  }
  if let Some(arr) = path.as_array() {
    let mut out = Vec::with_capacity(arr.len());
    for item in arr.iter::<Value<'js>>() {
      let item = item?;
      if let Some(s) = item.as_string() {
        out.push(PropSegment::Key(s.to_string()?));
      } else if let Some(n) = item.as_number() {
        if n < 0.0 || n.fract() != 0.0 {
          return Err(crate::bindings::convert::throw_named(
            ctx,
            "TypeError",
            "property path index must be a non-negative integer".to_string(),
          ));
        }
        out.push(PropSegment::Index(n as usize));
      } else {
        return Err(crate::bindings::convert::throw_named(
          ctx,
          "TypeError",
          "property path segment must be a string or an integer".to_string(),
        ));
      }
    }
    return Ok(out);
  }
  Err(crate::bindings::convert::throw_named(
    ctx,
    "TypeError",
    "property path must be a string or an array".to_string(),
  ))
}

/// Read a matcher's return value into core's [`ferridriver_expect::MatcherResult`].
fn parse_matcher_result<'js>(
  ctx: &Ctx<'js>,
  matcher: &str,
  raw: &Value<'js>,
) -> rquickjs::Result<ferridriver_expect::MatcherResult> {
  let invalid = || {
    crate::bindings::convert::throw_named(
      ctx,
      "Error",
      ferridriver_expect::invalid_result_message(&JsLive(raw.clone()).describe()),
    )
  };
  let Some(obj) = raw.as_object() else {
    return Err(invalid());
  };
  let Some(pass) = obj.get::<_, Value<'js>>("pass")?.as_bool() else {
    return Err(invalid());
  };
  let message = obj.get::<_, Value<'js>>("message")?;
  let message = if let Some(f) = message.as_function() {
    let produced: Value<'js> = f.call((rquickjs::function::This(raw.clone()),))?;
    produced.as_string().and_then(|s| s.to_string().ok())
  } else if message.is_undefined() || message.is_null() {
    None
  } else if let Some(s) = message.as_string() {
    Some(s.to_string()?)
  } else {
    return Err(invalid());
  };
  let mut out = ferridriver_expect::MatcherResult::new(pass);
  out.message = message;
  let expected = obj.get::<_, Value<'js>>("expected")?;
  let received = obj.get::<_, Value<'js>>("actual")?;
  if !expected.is_undefined() || !received.is_undefined() {
    out.expected = Some(JsLive(expected).describe());
    out.received = Some(JsLive(received).describe());
  }
  if let Some(log) = obj.get::<_, Value<'js>>("log")?.as_array() {
    out.log = log
      .iter::<Value<'js>>()
      .filter_map(std::result::Result::ok)
      .map(|v| JsLive(v).describe())
      .collect();
  }
  let _ = matcher;
  Ok(out)
}

/// Give an assertion instance one own method per custom matcher name.
fn install_user_matchers<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  let (table, names) = {
    let class = Class::<ExpectJs>::from_value(instance)?;
    let borrowed = class.borrow();
    (borrowed.matcher_table, borrowed.matcher_names.clone())
  };
  let Some(obj) = instance.as_object() else {
    return Ok(());
  };
  for name in names {
    let bound = name.clone();
    let f = Function::new(
      ctx.clone(),
      move |ctx: Ctx<'js>,
            this: rquickjs::function::This<Value<'js>>,
            args: rquickjs::function::Rest<Value<'js>>|
            -> rquickjs::Result<Value<'js>> { call_user_matcher(&ctx, &this.0, &bound, table, args.0) },
    )?;
    f.set_name(&name)?;
    obj.set(name, f)?;
  }
  Ok(())
}

fn call_user_matcher<'js>(
  ctx: &Ctx<'js>,
  this: &Value<'js>,
  name: &str,
  table: u32,
  args: Vec<Value<'js>>,
) -> rquickjs::Result<Value<'js>> {
  let class = Class::<ExpectJs>::from_value(this)?;
  let base = {
    let borrowed = class.borrow();
    borrowed.clone_with(|_| {})
  };
  let cx = ferridriver_expect::MatcherContext {
    is_not: base.is_not,
    is_soft: base.is_soft,
    promise: None,
    timeout: base.timeout,
    custom_message: base.message.clone(),
  };
  let body: Function<'js> = matcher_table(ctx, table)?.get(name)?;
  let receiver = base.live(ctx)?.0;
  let outcome: Value<'js> = body.call((
    rquickjs::function::This(matcher_context(ctx, &cx)?),
    receiver,
    rquickjs::function::Rest(args),
  ))?;

  // A synchronous matcher must fail synchronously: returning a promise
  // for it would let an unawaited assertion pass silently.
  if let Some(promise) = outcome.as_promise().cloned() {
    let name = name.to_string();
    let ctx_owned = ctx.clone();
    let fut: SettledFuture<'js> = Box::pin(async move {
      let settled: Value<'js> = promise.into_future().await?;
      let result = parse_matcher_result(&ctx_owned, &name, &settled)?;
      ferridriver_expect::finalize(&cx, &name, &result).or_else(|e| report(&ctx_owned, e))
    });
    return rquickjs::promise::Promised::from(fut).into_js(ctx);
  }
  let result = parse_matcher_result(ctx, name, &outcome)?;
  ferridriver_expect::finalize(&cx, name, &result).or_else(|e| report(ctx, e))?;
  Ok(Value::new_undefined(ctx.clone()))
}

// ── .resolves / .rejects ─────────────────────────────────────────────
//
// Playwright builds these the same way (`matchers/expect.ts:311-320`):
// one object per mode carrying every matcher name, plus a `not` twin.
// Here each name binds a native function that settles the subject and
// then delegates to the ordinary matcher on a fresh Expect built over
// the settled value — so a promise resolving to a Locator gets the
// Locator matchers, and there is no second copy of the matcher list.

/// A settled matcher's body. Boxed because a closure cannot name an
/// `impl Future` return type.
type SettledFuture<'js> = std::pin::Pin<Box<dyn std::future::Future<Output = rquickjs::Result<()>> + 'js>>;

/// Where the settled-matcher object keeps the assertion it came from.
/// Non-enumerable, and a plain property so QuickJS traces it — a native
/// closure must never capture a JS value.
const SETTLED_SOURCE: &str = "_expect";
const SETTLED_NEGATED: &str = "_not";

fn install_settled_getters<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  for mode in [PromiseMode::Resolves, PromiseMode::Rejects] {
    let getter = Function::new(
      ctx.clone(),
      move |ctx: Ctx<'js>, this: rquickjs::function::This<Value<'js>>| -> rquickjs::Result<Object<'js>> {
        build_settled(&ctx, &this.0, mode, true)
      },
    )?;
    define_accessor(ctx, instance, mode.as_str(), getter)?;
  }
  Ok(())
}

/// One matcher-name-keyed object for `mode`. `with_not` adds the `not`
/// twin — one level deep, as upstream (`resolves.not` exists,
/// `resolves.not.not` does not).
fn build_settled<'js>(
  ctx: &Ctx<'js>,
  source: &Value<'js>,
  mode: PromiseMode,
  with_not: bool,
) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  define_hidden(ctx, &obj, SETTLED_SOURCE, source.clone())?;
  define_hidden(ctx, &obj, SETTLED_NEGATED, (!with_not).into_js(ctx)?)?;
  let mut names = matcher_names(ctx)?;
  if let Ok(class) = Class::<ExpectJs>::from_value(source) {
    names.extend(class.borrow().matcher_names.iter().cloned());
  }
  for name in names {
    let bound = name.clone();
    let f = Function::new(
      ctx.clone(),
      move |ctx: Ctx<'js>,
            this: rquickjs::function::This<Value<'js>>,
            args: rquickjs::function::Rest<Value<'js>>|
            -> rquickjs::Result<rquickjs::promise::Promised<SettledFuture<'js>>> {
        let name = bound.clone();
        let this = this.0;
        let args = args.0;
        Ok(rquickjs::promise::Promised::from(
          Box::pin(async move { settled_call(ctx, this, name, mode, args).await }) as SettledFuture<'js>,
        ))
      },
    )?;
    f.set_name(&name)?;
    obj.set(name, f)?;
  }
  if with_not {
    let not = build_settled(ctx, source, mode, false)?;
    obj.set("not", not)?;
  }
  Ok(obj)
}

/// Every `to*` on the Expect prototype. Reading the prototype rather
/// than a hardcoded list is what keeps `.resolves` complete: a matcher
/// added to the class is reachable through it the same day.
fn matcher_names<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Vec<String>> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let own_names: Function<'js> = object_global.get("getOwnPropertyNames")?;
  let proto = Class::<ExpectJs>::prototype(ctx)?.ok_or_else(|| {
    rquickjs::Error::new_from_js_message(
      "expect",
      "resolves",
      "the Expect class has no prototype in this context",
    )
  })?;
  let names: Vec<String> = own_names.call((proto,))?;
  Ok(names.into_iter().filter(|n| n.starts_with("to")).collect())
}

/// Settle the subject, then run `name` against what it settled to.
async fn settled_call<'js>(
  ctx: Ctx<'js>,
  this: Value<'js>,
  name: String,
  mode: PromiseMode,
  args: Vec<Value<'js>>,
) -> rquickjs::Result<()> {
  let holder = this.as_object().ok_or_else(|| {
    rquickjs::Error::new_from_js_message(
      "expect",
      "resolves",
      "a settled matcher was called without its receiver",
    )
  })?;
  let source: Value<'js> = holder.get(SETTLED_SOURCE)?;
  let negated: bool = holder.get::<_, Value<'js>>(SETTLED_NEGATED)?.as_bool().unwrap_or(false);
  let class = Class::<ExpectJs>::from_value(&source)?;
  // Copy the assertion's state out and drop the borrow: nothing may hold
  // a class borrow across the await below.
  let base = {
    let borrowed = class.borrow();
    borrowed.clone_with(|e| {
      if negated {
        e.is_not = !e.is_not;
      }
    })
  };
  let fail = |mismatch, received: &str| {
    assertion_to_rq(
      &ctx,
      ferridriver_expect::promise_failure(mode, &name, base.is_not, base.message.as_deref(), mismatch, received),
    )
  };

  // Playwright accepts a promise OR a function returning one.
  let mut actual = base.live(&ctx)?.0;
  if let Some(f) = actual.as_function().cloned() {
    actual = f.call(())?;
  }
  let Some(promise) = actual.as_promise().cloned() else {
    return Err(fail(PromiseMismatch::NotAPromise, &JsLive(actual).describe()));
  };

  let settled = promise.into_future::<Value<'js>>().await;
  let receiver = match (mode, settled) {
    (PromiseMode::Resolves, Ok(v)) => v,
    (PromiseMode::Rejects, Ok(v)) => {
      return Err(fail(PromiseMismatch::ResolvedNotRejected, &JsLive(v).describe()));
    },
    (PromiseMode::Rejects, Err(rquickjs::Error::Exception)) => ctx.catch(),
    (PromiseMode::Resolves, Err(rquickjs::Error::Exception)) => {
      let reason = ctx.catch();
      return Err(fail(PromiseMismatch::RejectedNotResolved, &JsLive(reason).describe()));
    },
    // Not a JS rejection at all: a host-side error, which belongs to the
    // caller unchanged whichever way the chain was pointed.
    (_, Err(other)) => return Err(other),
  };

  let settled_expect = base.with_subject(&ctx, receiver.clone());
  // `toThrow` under a settled chain reads the value as the thrown error
  // instead of calling it (Playwright's `createThrowMatcher(_, true)`).
  if name == "toThrow" {
    let thrown = if receiver.is_error() {
      let (message, class_name) = extract_error(&receiver);
      Some(ThrownError { message, class_name })
    } else if let Some(f) = receiver.as_function().cloned() {
      match f.call::<_, Value<'js>>(()) {
        Ok(_) => None,
        Err(rquickjs::Error::Exception) => {
          let (message, class_name) = extract_error(&ctx.catch());
          Some(ThrownError { message, class_name })
        },
        Err(other) => Some(ThrownError {
          message: other.to_string(),
          class_name: None,
        }),
      }
    } else {
      None
    };
    return settled_expect.check_thrown(&ctx, thrown, Opt(args.into_iter().next()));
  }

  let instance = Class::instance(ctx.clone(), settled_expect)?.into_value();
  install_not_getter(&ctx, &instance)?;
  install_user_matchers(&ctx, &instance)?;
  let matcher: Function<'js> = instance
    .as_object()
    .and_then(|o| o.get(name.as_str()).ok())
    .ok_or_else(|| {
      rquickjs::Error::new_from_js_message("expect", "resolves", "no such matcher on the settled assertion")
    })?;
  let outcome: Value<'js> = matcher.call((
    rquickjs::function::This(instance.clone()),
    rquickjs::function::Rest(args),
  ))?;
  if let Some(p) = outcome.as_promise() {
    let _: Value<'js> = p.clone().into_future().await?;
  }
  Ok(())
}

/// `Object.defineProperty(target, key, { value, configurable: true })` —
/// an own property JS can read but not enumerate.
fn define_hidden<'js>(ctx: &Ctx<'js>, target: &Object<'js>, key: &str, value: Value<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("value", value)?;
  descriptor.set("configurable", true)?;
  let _: Value<'js> = define_property.call((target.clone(), key, descriptor))?;
  Ok(())
}

/// `Object.defineProperty(target, key, { get, configurable: true })`.
fn define_accessor<'js>(ctx: &Ctx<'js>, target: &Value<'js>, key: &str, getter: Function<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter)?;
  descriptor.set("configurable", true)?;
  let _: Value<'js> = define_property.call((target.clone(), key, descriptor))?;
  Ok(())
}

/// Playwright refuses `expect.poll(...).resolves` outright rather than
/// polling a promise (`matchers/expect.ts:433`), and says so.
fn install_poll_settled_refusal<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  for mode in [PromiseMode::Resolves, PromiseMode::Rejects] {
    let getter = Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
      Err(crate::bindings::convert::throw_named(
        &ctx,
        "Error",
        format!("`expect.poll()` does not support \"{}\" matcher.", mode.as_str()),
      ))
    })?;
    define_accessor(ctx, instance, mode.as_str(), getter)?;
  }
  Ok(())
}

fn install_poll_not_getter<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let getter = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, this: rquickjs::function::This<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let class = Class::<ExpectPollJs>::from_value(&this.0)?;
      let inverted = class.borrow().not_inner();
      let new_class = Class::instance(ctx.clone(), inverted)?;
      let new_val = new_class.into_value();
      install_poll_not_getter(&ctx, &new_val)?;
      Ok(new_val)
    },
  )?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter)?;
  descriptor.set("configurable", true)?;
  let _: rquickjs::Value<'js> = define_property.call((instance.clone(), "not", descriptor))?;
  Ok(())
}

// Accessor methods used by `build_expect` are defined in each binding
// module (`locator.rs::inner_ref`, `page.rs::page_arc`,
// `http_client.rs::inner_clone`) so they stay co-located with the
// private field they expose.
