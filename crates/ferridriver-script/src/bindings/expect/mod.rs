//! `QuickJS` bindings for the `expect` API — Jest-style value matchers,
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
  AssertionFailure, ExpectLive, ExpectValue, LiveError, LiveValue, StringOrRegex, ThrownError, expect_fn, expect_live,
  expect_value,
};
use rquickjs::{
  Array, Class, Ctx, Function, IntoJs, JsLifetime, Object, Persistent, Value, class::Trace, function::Opt,
};
use serde_json::Value as JsonValue;

use crate::bindings::convert::{serde_from_js, type_error};
use crate::bindings::http_client::HttpResponseJs;
use crate::bindings::locator::LocatorJs;
use crate::bindings::page::PageJs;

mod extend;
mod live;
mod matchers;
mod poll;
mod settled;

pub use extend::merge_expects;
pub use poll::ExpectPollJs;

use extend::{
  CustomMatchersActive, EXPECT_TABLE, extend_expect, has_custom_matchers, install_custom_asym, install_user_matchers,
  table_names,
};
use live::JsLive;
use matchers::parse_throw_matcher;
use poll::make_poll_fn;
use settled::{define_accessor, define_hidden, install_matcher_steps, install_settled_getters};

// ── ExpectJs ─────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Expect")]
pub struct ExpectJs {
  #[qjs(skip_trace)]
  pub(super) subject: ExpectSubject,
  pub(super) is_not: bool,
  pub(super) is_soft: bool,
  #[qjs(skip_trace)]
  pub(super) timeout: Duration,
  pub(super) message: Option<String>,
  /// Index of this assertion's custom-matcher table, and the names in
  /// it. Both are plain data: the matcher FUNCTIONS live in a JS object
  /// the engine traces, never in a native closure.
  #[qjs(skip_trace)]
  pub(super) matcher_table: u32,
  #[qjs(skip_trace)]
  pub(super) matcher_names: Vec<String>,
  /// The JS stack where `expect(...)` was called. A web-first matcher
  /// fails after an `await`, when no JS frame is left to name the line
  /// that asserted; its error carries this one instead.
  pub(super) stack: Option<String>,
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
pub(super) struct ExpectSubject {
  pub(super) live: Persistent<Value<'static>>,
  pub(super) kind: SubjectKind,
}

#[derive(Clone)]
pub(super) enum SubjectKind {
  Locator(Locator),
  Page(Arc<Page>),
  ApiResponse(HttpResponse),
  Value,
}

impl SubjectKind {
  /// The `_apiName` Playwright reports for this receiver in the
  /// wrong-receiver message.
  pub(super) fn api_name(&self) -> &'static str {
    match self {
      Self::Locator(_) => "Locator",
      Self::Page(_) => "Page",
      Self::ApiResponse(_) => "APIResponse",
      Self::Value => "value",
    }
  }
}

/// The runner mirrors it in through `set_current_test`, so a bare
/// `expect(x)` in a spec starts from `[test.expect]` — or from the
/// project's own `expect` block, which REPLACES the config's whole
/// object rather than merging into it.
#[derive(rquickjs::JsLifetime)]
pub(crate) struct ExpectDefaultsUd(pub std::cell::RefCell<Arc<ferridriver_test::config::ExpectConfig>>);

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
pub(super) fn expect_defaults(ctx: &Ctx<'_>) -> Arc<ferridriver_test::config::ExpectConfig> {
  ctx
    .userdata::<ExpectDefaultsUd>()
    .map_or_else(Arc::default, |ud| Arc::clone(&ud.0.borrow()))
}

/// What an assertion built right now starts from: the running test's
/// resolved `expect.timeout`, else the process-wide default the runner
/// set from the config.
pub(super) fn default_timeout(ctx: &Ctx<'_>) -> Duration {
  ctx
    .userdata::<ExpectDefaultsUd>()
    .map_or_else(ferridriver_expect::default_expect_timeout, |ud| {
      Duration::from_millis(ud.0.borrow().timeout_ms())
    })
}

impl ExpectJs {
  pub(super) fn new(subject: ExpectSubject, timeout: Duration) -> Self {
    Self {
      subject,
      is_not: false,
      is_soft: false,
      timeout,
      message: None,
      matcher_table: 0,
      matcher_names: Vec::new(),
      stack: None,
    }
  }

  pub(super) fn clone_with<F: FnOnce(&mut Self)>(&self, mutate: F) -> Self {
    let mut out = Self {
      subject: self.subject.clone(),
      is_not: self.is_not,
      is_soft: self.is_soft,
      timeout: self.timeout,
      message: self.message.clone(),
      matcher_table: self.matcher_table,
      matcher_names: self.matcher_names.clone(),
      stack: self.stack.clone(),
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
  pub(super) fn with_subject<'js>(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Self {
    let mut out = build_expect(ctx, value);
    out.is_not = self.is_not;
    out.is_soft = self.is_soft;
    out.timeout = self.timeout;
    out.message.clone_from(&self.message);
    out.matcher_table = self.matcher_table;
    out.matcher_names.clone_from(&self.matcher_names);
    if out.stack.is_none() {
      out.stack.clone_from(&self.stack);
    }
    out
  }

  /// The subject's structural snapshot, taken now rather than at
  /// `expect(...)` time — a subject that cannot be serialized only
  /// fails the matchers that actually need a snapshot.
  pub(super) fn snapshot(&self, ctx: &Ctx<'_>) -> Result<JsonValue, rquickjs::Error> {
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

  pub(super) fn locator_target(&self, ctx: &Ctx<'_>, matcher: &'static str) -> Result<&Locator, rquickjs::Error> {
    match &self.subject.kind {
      SubjectKind::Locator(loc) => Ok(loc),
      _ => Err(self.wrong_receiver(ctx, matcher, "Locator")),
    }
  }

  pub(super) fn page_target(&self, ctx: &Ctx<'_>, matcher: &'static str) -> Result<&Arc<Page>, rquickjs::Error> {
    match &self.subject.kind {
      SubjectKind::Page(p) => Ok(p),
      _ => Err(self.wrong_receiver(ctx, matcher, "Page")),
    }
  }

  pub(super) fn api_response_target(
    &self,
    ctx: &Ctx<'_>,
    matcher: &'static str,
  ) -> Result<&HttpResponse, rquickjs::Error> {
    match &self.subject.kind {
      SubjectKind::ApiResponse(r) => Ok(r),
      _ => Err(self.wrong_receiver(ctx, matcher, "APIResponse")),
    }
  }

  /// Playwright's `expectTypes` message (`matcherHint.ts:65`), verbatim
  /// down to the receiver rendering.
  pub(super) fn wrong_receiver(&self, ctx: &Ctx<'_>, matcher: &'static str, wanted: &str) -> rquickjs::Error {
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
  pub(super) fn function_target<'js>(&self, ctx: &Ctx<'js>, matcher: &'static str) -> rquickjs::Result<Function<'js>> {
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
  pub(super) fn check_thrown<'js>(
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
    ef.to_throw(matcher.as_ref())
      .map_err(|e| assertion_to_rq(ctx, e, self.stack.as_deref()))
  }

  pub(super) fn build_value_expect(&self, ctx: &Ctx<'_>) -> Result<ExpectValue, rquickjs::Error> {
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
  pub(super) fn snapshot_target(
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
        Err(_) => Err(unsupported_snapshot_subject(ctx, matcher)),
      },
      SubjectKind::ApiResponse(_) => Err(unsupported_snapshot_subject(ctx, matcher)),
    }
  }

  pub(super) fn build_locator_expect(
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

  pub(super) fn build_page_expect(
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

  pub(super) fn build_api_response_expect(
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

  /// [`report`] under this assertion's call-site stack.
  pub(super) fn fail(&self, ctx: &Ctx<'_>, err: AssertionFailure) -> rquickjs::Result<()> {
    report(ctx, err, self.stack.as_deref())
  }

  /// [`live_report`] under this assertion's call-site stack.
  pub(super) fn live_fail(&self, ctx: &Ctx<'_>, err: LiveError<rquickjs::Error>) -> rquickjs::Result<()> {
    live_report(ctx, err, self.stack.as_deref())
  }

  /// Per-call copy with the inline `{ timeout }` matcher option applied
  /// (Playwright: every web-first matcher accepts `{ timeout? }` in its
  /// trailing options bag, overriding the assertion default).
  pub(super) fn for_call(&self, obj: Option<&Object<'_>>) -> Self {
    let timeout = u64_field(obj, "timeout");
    self.clone_with(|e| {
      if let Some(ms) = timeout {
        e.timeout = Duration::from_millis(ms);
      }
    })
  }
}

pub(super) fn unsupported_snapshot_subject(ctx: &Ctx<'_>, matcher: &'static str) -> rquickjs::Error {
  type_error(
    ctx,
    format!("expect.{matcher}: snapshot matchers apply to a locator, a page, or a serializable value"),
  )
}

/// A snapshot-matcher failure (message produced runner-side, already
/// fully formatted), resolved by the same soft rule as every other
/// assertion.
/// Playwright's `nameOrOptions, optOptions` pair: a name (`string` or
/// path segments) with the options after it, or one bag carrying `name`.
pub(super) fn snapshot_name_and_options<'js>(
  ctx: &Ctx<'js>,
  name_or_options: Option<Value<'js>>,
  options: Option<Value<'js>>,
) -> rquickjs::Result<(ferridriver_test::snapshot_path::SnapshotName, JsonValue)> {
  use ferridriver_test::snapshot_path::SnapshotName;
  let as_name = |value: &Value<'js>| -> rquickjs::Result<Option<SnapshotName>> {
    if let Some(s) = value.as_string() {
      return Ok(Some(SnapshotName::One(s.to_string()?)));
    }
    if let Some(array) = value.as_array() {
      let parts = array.iter::<String>().collect::<rquickjs::Result<Vec<_>>>()?;
      return Ok(Some(SnapshotName::Segments(parts)));
    }
    Ok(None)
  };
  let to_json = |value: Option<Value<'js>>| -> rquickjs::Result<JsonValue> {
    match value {
      Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(ctx, v),
      _ => Ok(JsonValue::Object(serde_json::Map::new())),
    }
  };
  match name_or_options {
    Some(value) if value.as_string().is_some() || value.as_array().is_some() => {
      let name = as_name(&value)?.unwrap_or(SnapshotName::Anonymous);
      Ok((name, to_json(options)?))
    },
    Some(value) if value.as_object().is_some() => {
      let name = match value.as_object().map(|o| o.get::<_, Value<'js>>("name")).transpose()? {
        Some(n) if !n.is_undefined() => as_name(&n)?.unwrap_or(SnapshotName::Anonymous),
        _ => SnapshotName::Anonymous,
      };
      let mut bag = to_json(Some(value))?;
      if let Some(map) = bag.as_object_mut() {
        map.remove("name");
      }
      Ok((name, bag))
    },
    _ => Ok((SnapshotName::Anonymous, to_json(options)?)),
  }
}

pub(super) fn snapshot_failure(
  ctx: &Ctx<'_>,
  is_soft: bool,
  message: String,
  stack: Option<&str>,
) -> rquickjs::Result<()> {
  report(ctx, AssertionFailure::new(message, None).with_soft(is_soft), stack)
}

/// A failed assertion, resolved the way its softness says: a soft
/// failure is recorded against the running test and the caller carries
/// on; anything else is thrown.
///
/// Outside a test there is nothing to record into, so a soft failure
/// throws — the same rule `ferridriver_expect::soft` applies on the Rust
/// side, where an absent sink means the failure surfaces.
pub(super) fn report(ctx: &Ctx<'_>, err: AssertionFailure, stack: Option<&str>) -> rquickjs::Result<()> {
  if err.soft
    && let Some(bridge) = crate::bindings::test::optional_bridge(ctx)
  {
    bridge.record_soft_error(assertion_text(&err), err.diff, err.error_context);
    return Ok(());
  }
  Err(assertion_to_rq(ctx, err, stack))
}

/// The same one-string rendering the thrown error carries.
fn assertion_text(err: &AssertionFailure) -> String {
  match err.diff.as_deref() {
    Some(body) if !body.is_empty() => format!("{}\n\n{body}", err.message),
    _ => err.message.clone(),
  }
}

pub(super) fn assertion_to_rq(ctx: &Ctx<'_>, err: AssertionFailure, stack: Option<&str>) -> rquickjs::Error {
  // Concatenate title + body for the JS-thrown message so the JS stack
  // shows the full failure on one Error. Thrown as a real `Error` with
  // `name = "AssertionError"` so `e.name` checks match.
  let full = assertion_text(&err);
  let built: rquickjs::Result<Value<'_>> = (|| {
    let ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("Error")?;
    let error: Object<'_> = ctor.construct((full.as_str(),))?;
    error.set("name", "AssertionError")?;
    let own_stack: Option<String> = error.get("stack")?;
    if let Some(stack) = stack
      && own_stack.is_none_or(|s| s.trim().is_empty())
    {
      error.set("stack", stack)?;
    }
    // Upstream's `ExpectError.matcherResult`: the runner reads the
    // snapshot back off whatever error fails the test, so it survives a
    // `toPass` retry or a `test.step` rethrow.
    if let Some(aria_snapshot) = err.error_context {
      let matcher_result = Object::new(ctx.clone())?;
      matcher_result.set("message", full.as_str())?;
      matcher_result.set("ariaSnapshot", aria_snapshot)?;
      error.set("matcherResult", matcher_result)?;
    }
    Ok(error.into_value())
  })();
  match built {
    Ok(error) => ctx.throw(error),
    Err(_) => crate::bindings::convert::throw_named(ctx, "AssertionError", full),
  }
}

/// The options bag of a web-first matcher call, if one was passed.
pub(super) fn opts_obj<'js>(options: &Opt<Value<'js>>) -> Option<Object<'js>> {
  options.0.as_ref().and_then(Value::as_object).cloned()
}

pub(super) fn u64_field(obj: Option<&Object<'_>>, key: &str) -> Option<u64> {
  let v = obj?.get::<_, rquickjs::Value<'_>>(key).ok()?;
  v.as_int().and_then(|i| u64::try_from(i).ok()).or_else(|| {
    v.as_number()
      .filter(|n| *n >= 0.0)
      .map(crate::bindings::convert::f64_to_u64)
  })
}

pub(super) fn bool_field(obj: Option<&Object<'_>>, key: &str) -> Option<bool> {
  obj?.get::<_, rquickjs::Value<'_>>(key).ok()?.as_bool()
}

pub(super) fn f64_field(obj: Option<&Object<'_>>, key: &str) -> Option<f64> {
  let v = obj?.get::<_, rquickjs::Value<'_>>(key).ok()?;
  v.as_number().or_else(|| v.as_int().map(f64::from))
}

/// Parse a `number[]` option field (e.g. `intervals`), dropping
/// non-numeric entries; `None` when absent or empty.
pub(super) fn u64_array_field(obj: Option<&Object<'_>>, key: &str) -> Option<Vec<u64>> {
  let arr = obj?.get::<_, rquickjs::Value<'_>>(key).ok()?.into_array()?;
  let out: Vec<u64> = arr
    .iter::<rquickjs::Value<'_>>()
    .filter_map(std::result::Result::ok)
    .filter_map(|v| {
      v.as_int().and_then(|i| u64::try_from(i).ok()).or_else(|| {
        v.as_number()
          .filter(|n| *n >= 0.0)
          .map(crate::bindings::convert::f64_to_u64)
      })
    })
    .collect();
  (!out.is_empty()).then_some(out)
}

/// True when the value is a string or a `RegExp` instance — the shapes
/// Playwright treats as an expected text value rather than an options
/// bag in overloads like `toHaveAttribute(name, value?, options?)`.
pub(super) fn is_string_or_regex(value: &Value<'_>) -> bool {
  if value.as_string().is_some() {
    return true;
  }
  value.as_object().is_some_and(|obj| {
    let src = obj.get::<_, rquickjs::Value<'_>>("source").ok();
    let flags = obj.get::<_, rquickjs::Value<'_>>("flags").ok();
    matches!((src, flags), (Some(s), Some(f)) if s.as_string().is_some() && f.as_string().is_some())
  })
}

pub(super) fn text_match_options(obj: Option<&Object<'_>>) -> ferridriver_expect::TextMatchOptions {
  ferridriver_expect::TextMatchOptions {
    ignore_case: bool_field(obj, "ignoreCase").unwrap_or(false),
    use_inner_text: bool_field(obj, "useInnerText").unwrap_or(false),
  }
}

pub(super) fn parse_string_or_regex<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<StringOrRegex> {
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
      return Ok(StringOrRegex::regex(s.to_string()?, f.to_string()?));
    }
  }
  Err(type_error(ctx, "expected a string or RegExp"))
}

/// The `(string | RegExp)[]` half of the text matchers' overload.
/// `None` when the argument is not an array, so the caller falls
/// through to the single-value form.
pub(super) fn parse_string_or_regex_array<'js>(
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

// ── factory + asymmetric helpers ─────────────────────────────────────

/// Construct an [`ExpectJs`] from any JS value.
///
/// The value is kept as-is; the typed handle it resolves to (if any)
/// only decides which web-first matchers apply. No snapshot is taken
/// here — `expect(x)` itself never fails, exactly as upstream.
pub(super) fn build_expect<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> ExpectJs {
  let is_obj = value.is_object();
  let is_fn = is_obj && value.is_function();
  let kind = if is_obj {
    if let Ok(class) = Class::<LocatorJs>::from_value(&value) {
      let loc = class.borrow().inner_ref().clone();
      SubjectKind::Locator(loc)
    } else if let Ok(class) = Class::<PageJs>::from_value(&value) {
      SubjectKind::Page(class.borrow().page_arc())
    } else if let Ok(class) = Class::<HttpResponseJs>::from_value(&value) {
      SubjectKind::ApiResponse(class.borrow().inner_clone())
    } else {
      SubjectKind::Value
    }
  } else {
    SubjectKind::Value
  };
  let needs_stack = is_fn || !matches!(kind, SubjectKind::Value);
  let mut expect = ExpectJs::new(
    ExpectSubject {
      live: Persistent::save(ctx, value),
      kind,
    },
    default_timeout(ctx),
  );
  if needs_stack {
    expect.stack = capture_stack(ctx);
  }
  expect
}

pub(super) fn capture_stack(ctx: &Ctx<'_>) -> Option<String> {
  let ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("Error").ok()?;
  let error: Object<'_> = ctor.construct(("",)).ok()?;
  error.get::<_, Option<String>>("stack").ok().flatten()
}

/// Playwright: `expect(actual, messageOrOptions?: string | { message?: string })`
/// (`types/test.d.ts:8934`) — the same trailing argument `expect.soft`
/// and `expect.poll` take.
pub(super) fn custom_message<'js>(value: Option<&Value<'js>>) -> rquickjs::Result<Option<String>> {
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
pub(super) fn live_report(ctx: &Ctx<'_>, err: LiveError<rquickjs::Error>, stack: Option<&str>) -> rquickjs::Result<()> {
  match err {
    LiveError::Failed(f) => report(ctx, f, stack),
    LiveError::BadInput(b) => Err(crate::bindings::convert::throw_named(ctx, "TypeError", b.to_string())),
    LiveError::Host(e) => Err(e),
  }
}

pub(super) fn make_asymmetric<'js>(ctx: &Ctx<'js>, tag: &str, payload: Object<'js>) -> rquickjs::Result<Object<'js>> {
  payload.set(ferridriver_expect::ASYM_TAG_KEY, tag)?;
  let _ = ctx;
  Ok(payload)
}

///
/// # Errors
///
/// When defining the classes, building `expect` or setting the globals
/// fails.
pub fn install_expect<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
  // Define the class prototype once so `expect(x)` can return
  // `ExpectJs` instances JS can call methods on.
  Class::<ExpectJs>::define(&ctx.globals())?;
  Class::<ExpectPollJs>::define(&ctx.globals())?;
  let _ = ctx.store_userdata(CustomMatchersActive::default());
  install_matcher_steps(ctx)?;
  if let Some(proto) = Class::<ExpectJs>::prototype(ctx)?
    && !proto.contains_key("not")?
  {
    let proto_val = proto.into_value();
    install_not_getter(ctx, &proto_val)?;
    install_settled_getters(ctx, &proto_val)?;
  }

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
pub(super) fn create_expect<'js>(
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
      if table != 0 || has_custom_matchers(&ctx) {
        inst.matcher_names = table_names(&ctx, table)?;
      }
      let has_user_matchers = !inst.matcher_names.is_empty();
      let val = Class::instance(ctx.clone(), inst)?.into_value();
      if has_user_matchers {
        install_user_matchers(&ctx, &val)?;
      }
      Ok(val)
    },
  )?;
  expect_fn.set_name("expect")?;

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
  let expect_obj = expect_fn
    .as_object()
    .ok_or_else(|| type_error(ctx, "expect.install: expect Function has no object representation"))?;
  expect_obj.set("poll", make_poll_fn(ctx, meta)?)?;
  install_asymmetric_factories(ctx, expect_obj)?;
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

/// The asymmetric matcher factories (`expect.any`, `expect.closeTo`, ...) on `target`.
fn install_asymmetric_factories<'js>(ctx: &Ctx<'js>, target: &Object<'js>) -> rquickjs::Result<()> {
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
          .unwrap_or_else(|_| Value::new_undefined(ctx.clone()));
        if let Some(s) = source.as_string() {
          obj.set("regex", s.to_string()?)?;
        }
        if let Some(f) = flags.as_string() {
          obj.set("flags", f.to_string()?)?;
        }
      } else {
        return Err(type_error(&ctx, "expect.stringMatching expects a string or RegExp"));
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

  target.set("any", any_fn)?;
  target.set("anything", anything_fn)?;
  target.set("arrayContaining", array_containing_fn)?;
  target.set("objectContaining", object_containing_fn)?;
  target.set("stringContaining", string_containing_fn)?;
  target.set("stringMatching", string_matching_fn)?;
  target.set("closeTo", close_to_fn)?;
  target.set("arrayOf", array_of_fn)?;
  Ok(())
}

/// `extend` / `configure` / `getState` / `soft`, plus the hidden state
/// `mergeExpects` reads back off another expect.
pub(super) fn install_expect_meta<'js>(
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

pub(super) fn type_name(value: &Value<'_>) -> String {
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
    out.timeout = ferridriver_expect::Setting::from_optional(
      true,
      v.as_number()
        .map(|ms| Duration::from_millis(crate::bindings::convert::f64_to_u64(ms))),
    );
  }
  if obj.contains_key("soft").unwrap_or(false) {
    let v: Value<'js> = obj.get("soft")?;
    out.soft = Some(v.as_bool().unwrap_or(false));
  }
  Ok(out)
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
pub(super) fn install_not_getter<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let getter = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, this: rquickjs::function::This<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let class = Class::<ExpectJs>::from_value(&this.0)?;
      let inverted = class.borrow().not_inner();
      let has_user_matchers = !inverted.matcher_names.is_empty();
      let new_val = Class::instance(ctx.clone(), inverted)?.into_value();
      if has_user_matchers {
        install_user_matchers(&ctx, &new_val)?;
      }
      Ok(new_val)
    },
  )?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter)?;
  descriptor.set("configurable", true)?;
  let _: rquickjs::Value<'js> = define_property.call((instance.clone(), "not", descriptor))?;
  Ok(())
}
