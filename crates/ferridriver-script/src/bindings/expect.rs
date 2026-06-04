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
  AssertionFailure, DEFAULT_EXPECT_TIMEOUT, ExpectValue, POLL_INTERVALS, StringOrRegex, ThrowMatcher, ThrownError,
  deep_equal, expect_fn, expect_value,
};
use rquickjs::{Array, Class, Ctx, Function, JsLifetime, Object, Persistent, Value, class::Trace, function::Opt};
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
  target: ExpectTarget,
  is_not: bool,
  is_soft: bool,
  #[qjs(skip_trace)]
  timeout: Duration,
  message: Option<String>,
}

#[derive(Clone)]
enum ExpectTarget {
  Value {
    value: JsonValue,
    /// `value.constructor.name` captured at `expect(...)` call time,
    /// used by `toBeInstanceOf` to compare custom constructors.
    ctor_name: Option<String>,
  },
  Locator(Locator),
  Page(Arc<Page>),
  ApiResponse(HttpResponse),
  /// Persistent JS function — kept across `expect(fn).toThrow(...)`
  /// because `toThrow` invokes it lazily.
  Fn(Persistent<Function<'static>>),
}

impl ExpectJs {
  fn new(target: ExpectTarget) -> Self {
    Self {
      target,
      is_not: false,
      is_soft: false,
      timeout: DEFAULT_EXPECT_TIMEOUT,
      message: None,
    }
  }

  fn clone_with<F: FnOnce(&mut Self)>(&self, mutate: F) -> Self {
    let mut out = Self {
      target: self.target.clone(),
      is_not: self.is_not,
      is_soft: self.is_soft,
      timeout: self.timeout,
      message: self.message.clone(),
    };
    mutate(&mut out);
    out
  }

  fn value_target(&self) -> Result<(&JsonValue, Option<&str>), rquickjs::Error> {
    match &self.target {
      ExpectTarget::Value { value, ctor_name } => Ok((value, ctor_name.as_deref())),
      _ => Err(rquickjs::Error::new_from_js_message(
        "expect",
        "matcher",
        "this matcher requires a value subject (got Locator/Page/Response/Function)",
      )),
    }
  }

  fn locator_target(&self) -> Result<&Locator, rquickjs::Error> {
    match &self.target {
      ExpectTarget::Locator(loc) => Ok(loc),
      _ => Err(rquickjs::Error::new_from_js_message(
        "expect",
        "matcher",
        "this matcher requires a Locator subject",
      )),
    }
  }

  fn page_target(&self) -> Result<&Arc<Page>, rquickjs::Error> {
    match &self.target {
      ExpectTarget::Page(p) => Ok(p),
      _ => Err(rquickjs::Error::new_from_js_message(
        "expect",
        "matcher",
        "this matcher requires a Page subject",
      )),
    }
  }

  fn api_response_target(&self) -> Result<&HttpResponse, rquickjs::Error> {
    match &self.target {
      ExpectTarget::ApiResponse(r) => Ok(r),
      _ => Err(rquickjs::Error::new_from_js_message(
        "expect",
        "matcher",
        "this matcher requires an APIResponse subject",
      )),
    }
  }

  fn fn_target(&self) -> Result<&Persistent<Function<'static>>, rquickjs::Error> {
    match &self.target {
      ExpectTarget::Fn(f) => Ok(f),
      _ => Err(rquickjs::Error::new_from_js_message(
        "expect",
        "matcher",
        "this matcher requires a function subject",
      )),
    }
  }

  fn build_value_expect(&self) -> Result<ExpectValue, rquickjs::Error> {
    let (val, _) = self.value_target()?;
    let mut ev = expect_value(val.clone());
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
  fn build_locator_expect(&self) -> Result<ferridriver_expect::Expect<'_, Locator>, rquickjs::Error> {
    let loc = self.locator_target()?;
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

  fn build_page_expect(&self) -> Result<ferridriver_expect::Expect<'_, std::sync::Arc<Page>>, rquickjs::Error> {
    let p = self.page_target()?;
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

  fn build_api_response_expect(&self) -> Result<ferridriver_expect::Expect<'_, HttpResponse>, rquickjs::Error> {
    let r = self.api_response_target()?;
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
}

fn assertion_to_rq(err: AssertionFailure) -> rquickjs::Error {
  // Concatenate title + body for the JS-thrown message so the JS stack
  // shows the full failure on one Error. The JS stack itself comes from
  // QuickJS and is added to the Error automatically.
  let full = match err.diff.as_deref() {
    Some(body) if !body.is_empty() => format!("{}\n\n{body}", err.message),
    _ => err.message,
  };
  rquickjs::Error::new_from_js_message("expect", "AssertionError", full)
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

  #[qjs(rename = "toBe")]
  pub fn to_be<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp: JsonValue = serde_from_js(&ctx, expected)?;
    self.build_value_expect()?.to_be(&exp).map_err(assertion_to_rq)
  }

  #[qjs(rename = "toEqual")]
  pub fn to_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp: JsonValue = serde_from_js(&ctx, expected)?;
    self.build_value_expect()?.to_equal(&exp).map_err(assertion_to_rq)
  }

  #[qjs(rename = "toStrictEqual")]
  pub fn to_strict_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp: JsonValue = serde_from_js(&ctx, expected)?;
    self
      .build_value_expect()?
      .to_strict_equal(&exp)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeNull")]
  pub fn to_be_null(&self) -> rquickjs::Result<()> {
    self.build_value_expect()?.to_be_null().map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeUndefined")]
  pub fn to_be_undefined(&self) -> rquickjs::Result<()> {
    self.build_value_expect()?.to_be_undefined().map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeDefined")]
  pub fn to_be_defined(&self) -> rquickjs::Result<()> {
    self.build_value_expect()?.to_be_defined().map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeTruthy")]
  pub fn to_be_truthy(&self) -> rquickjs::Result<()> {
    self.build_value_expect()?.to_be_truthy().map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeFalsy")]
  pub fn to_be_falsy(&self) -> rquickjs::Result<()> {
    self.build_value_expect()?.to_be_falsy().map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeNaN")]
  pub fn to_be_nan(&self) -> rquickjs::Result<()> {
    self.build_value_expect()?.to_be_nan().map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeCloseTo")]
  pub fn to_be_close_to(&self, expected: f64, digits: Opt<u8>) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_be_close_to(expected, digits.0)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeGreaterThan")]
  pub fn to_be_greater_than(&self, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_be_greater_than(expected)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeGreaterThanOrEqual")]
  pub fn to_be_greater_than_or_equal(&self, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_be_greater_than_or_equal(expected)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeLessThan")]
  pub fn to_be_less_than(&self, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_be_less_than(expected)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeLessThanOrEqual")]
  pub fn to_be_less_than_or_equal(&self, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_be_less_than_or_equal(expected)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toContain")]
  pub fn to_contain<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp: JsonValue = serde_from_js(&ctx, expected)?;
    self.build_value_expect()?.to_contain(&exp).map_err(assertion_to_rq)
  }

  #[qjs(rename = "toContainEqual")]
  pub fn to_contain_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_contain_equal(&serde_from_js(&ctx, expected)?)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveLength")]
  pub fn to_have_length(&self, expected: u32) -> rquickjs::Result<()> {
    self
      .build_value_expect()?
      .to_have_length(expected as usize)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveProperty")]
  pub fn to_have_property<'js>(
    &self,
    ctx: Ctx<'js>,
    path: Value<'js>,
    expected: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let path_v: JsonValue = serde_from_js(&ctx, path)?;
    let exp = match expected.0 {
      Some(v) if !v.is_undefined() => Some(serde_from_js::<JsonValue>(&ctx, v)?),
      _ => None,
    };
    self
      .build_value_expect()?
      .to_have_property(&path_v, exp.as_ref())
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toMatch")]
  pub fn to_match<'js>(&self, ctx: Ctx<'js>, pattern: Value<'js>) -> rquickjs::Result<()> {
    let pat = parse_string_or_regex(&ctx, &pattern)?;
    self.build_value_expect()?.to_match(&pat).map_err(assertion_to_rq)
  }

  #[qjs(rename = "toMatchObject")]
  pub fn to_match_object<'js>(&self, ctx: Ctx<'js>, subset: Value<'js>) -> rquickjs::Result<()> {
    let sub: JsonValue = serde_from_js(&ctx, subset)?;
    self
      .build_value_expect()?
      .to_match_object(&sub)
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeInstanceOf")]
  pub fn to_be_instance_of<'js>(&self, _ctx: Ctx<'js>, ctor: Value<'js>) -> rquickjs::Result<()> {
    let ctor_name = ctor
      .as_function()
      .and_then(|f| f.get::<_, rquickjs::Value<'js>>("name").ok())
      .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()))
      .unwrap_or_else(|| "(unknown)".into());
    let (val, target_ctor) = self.value_target()?;
    let mut ev = expect_value(val.clone());
    if self.is_not {
      ev = ev.not();
    }
    ev.to_be_instance_of(&ctor_name, target_ctor).map_err(assertion_to_rq)
  }

  #[qjs(rename = "toThrow")]
  pub async fn to_throw<'js>(&self, ctx: Ctx<'js>, matcher: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let f = self.fn_target()?.clone().restore(&ctx)?;
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
    let matcher = match matcher.0 {
      Some(v) if !v.is_undefined() => Some(parse_throw_matcher(&ctx, v)?),
      _ => None,
    };
    let mut ef = expect_fn(caught);
    if self.is_not {
      ef = ef.not();
    }
    if let Some(m) = &self.message {
      ef = ef.with_message(m.clone());
    }
    ef.to_throw(matcher.as_ref()).map_err(assertion_to_rq)
  }

  // ── Locator web-first matchers (delegated to ferridriver-expect) ──

  #[qjs(rename = "toBeVisible")]
  pub async fn to_be_visible(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_visible()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeHidden")]
  pub async fn to_be_hidden(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_hidden()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeEnabled")]
  pub async fn to_be_enabled(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_enabled()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeDisabled")]
  pub async fn to_be_disabled(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_disabled()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeChecked")]
  pub async fn to_be_checked(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_checked()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeEditable")]
  pub async fn to_be_editable(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_editable()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeAttached")]
  pub async fn to_be_attached(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_attached()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toBeEmpty")]
  pub async fn to_be_empty(&self) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_be_empty()
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveText")]
  pub async fn to_have_text<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp = parse_string_or_regex(&ctx, &expected)?;
    self
      .build_locator_expect()?
      .to_have_text(exp)
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toContainText")]
  pub async fn to_contain_text(&self, expected: String) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_contain_text(StringOrRegex::String(expected))
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveValue")]
  pub async fn to_have_value<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp = parse_string_or_regex(&ctx, &expected)?;
    self
      .build_locator_expect()?
      .to_have_value(exp)
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveCount")]
  pub async fn to_have_count(&self, expected: u32) -> rquickjs::Result<()> {
    self
      .build_locator_expect()?
      .to_have_count(expected as usize)
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveAttribute")]
  pub async fn to_have_attribute<'js>(
    &self,
    ctx: Ctx<'js>,
    name: String,
    value: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let e = self.build_locator_expect()?;
    match value.0 {
      Some(v) if !v.is_undefined() => {
        let exp = parse_string_or_regex(&ctx, &v)?;
        e.to_have_attribute(&name, exp).await
      },
      _ => e.to_have_attribute_exists(&name).await,
    }
    .map_err(assertion_to_rq)
  }

  // ── Page web-first matchers (delegated) ───────────────────────────

  #[qjs(rename = "toHaveTitle")]
  pub async fn to_have_title<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp = parse_string_or_regex(&ctx, &expected)?;
    self
      .build_page_expect()?
      .to_have_title(exp)
      .await
      .map_err(assertion_to_rq)
  }

  #[qjs(rename = "toHaveURL")]
  pub async fn to_have_url<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp = parse_string_or_regex(&ctx, &expected)?;
    self
      .build_page_expect()?
      .to_have_url(exp)
      .await
      .map_err(assertion_to_rq)
  }

  // ── APIResponse matcher (delegated) ──────────────────────────────

  #[qjs(rename = "toBeOK")]
  pub fn to_be_ok(&self) -> rquickjs::Result<()> {
    self.build_api_response_expect()?.to_be_ok().map_err(assertion_to_rq)
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
  is_not: bool,
}

#[rquickjs::methods]
impl ExpectPollJs {
  #[qjs(rename = "withTimeout")]
  pub fn with_timeout(&self, timeout_ms: u32) -> ExpectPollJs {
    ExpectPollJs {
      generator: self.generator.clone(),
      timeout: Duration::from_millis(u64::from(timeout_ms)),
      is_not: self.is_not,
    }
  }

  #[qjs(rename = "_notInner")]
  pub fn not_inner(&self) -> ExpectPollJs {
    ExpectPollJs {
      generator: self.generator.clone(),
      timeout: self.timeout,
      is_not: !self.is_not,
    }
  }

  #[qjs(rename = "toBe")]
  pub async fn to_be<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp: JsonValue = serde_from_js(&ctx, expected)?;
    self.poll_value(&ctx, "toBe", &exp).await
  }

  #[qjs(rename = "toEqual")]
  pub async fn to_equal<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let exp: JsonValue = serde_from_js(&ctx, expected)?;
    self.poll_value(&ctx, "toEqual", &exp).await
  }

  #[qjs(rename = "toSatisfy")]
  pub async fn to_satisfy<'js>(&self, ctx: Ctx<'js>, predicate: Function<'js>) -> rquickjs::Result<()> {
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
      let interval_ms = POLL_INTERVALS
        .get(interval_idx)
        .copied()
        .unwrap_or_else(|| POLL_INTERVALS.last().copied().unwrap_or(1000));
      interval_idx += 1;
      let sleep_dur = Duration::from_millis(interval_ms);
      if tokio::time::Instant::now() + sleep_dur > deadline {
        break dbg;
      }
      tokio::time::sleep(sleep_dur).await;
    };
    let last = final_dbg.as_str();
    Err(assertion_to_rq(AssertionFailure::new(
      format!(
        "expect.poll().toSatisfy() timed out after {}ms; last value was {last}",
        self.timeout.as_millis()
      ),
      None,
    )))
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
      let interval_ms = POLL_INTERVALS
        .get(interval_idx)
        .copied()
        .unwrap_or_else(|| POLL_INTERVALS.last().copied().unwrap_or(1000));
      interval_idx += 1;
      let sleep_dur = Duration::from_millis(interval_ms);
      if tokio::time::Instant::now() + sleep_dur > deadline {
        break actual;
      }
      tokio::time::sleep(sleep_dur).await;
    };
    Err(assertion_to_rq(AssertionFailure::new(
      format!(
        "expect.poll().{method}() timed out after {}ms\n\nExpected: {}\nReceived: {}",
        self.timeout.as_millis(),
        ferridriver_expect::asymmetric::json_short(expected),
        ferridriver_expect::asymmetric::json_short(&last)
      ),
      None,
    )))
  }
}

async fn call_generator<'js>(
  ctx: &Ctx<'js>,
  generator_fn: &Persistent<Function<'static>>,
) -> rquickjs::Result<JsonValue> {
  let f = generator_fn.clone().restore(ctx)?;
  let result: rquickjs::Value<'js> = f.call(())?;
  // Await the result if it's a thenable.
  let result = if let Some(promise) = result.as_promise() {
    promise.clone().into_future::<rquickjs::Value<'js>>().await?
  } else {
    result
  };
  serde_from_js(ctx, result)
}

// ── factory + asymmetric helpers ─────────────────────────────────────

/// Construct an [`ExpectJs`] from any JS value, dispatching on the
/// runtime type to the appropriate target.
fn build_expect<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ExpectJs> {
  if let Ok(class) = Class::<LocatorJs>::from_value(&value) {
    let loc = class.borrow().inner_ref().clone();
    return Ok(ExpectJs::new(ExpectTarget::Locator(loc)));
  }
  if let Ok(class) = Class::<PageJs>::from_value(&value) {
    return Ok(ExpectJs::new(ExpectTarget::Page(class.borrow().page_arc())));
  }
  if let Ok(class) = Class::<HttpResponseJs>::from_value(&value) {
    return Ok(ExpectJs::new(ExpectTarget::ApiResponse(class.borrow().inner_clone())));
  }
  if value.is_function()
    && let Some(func) = value.as_function()
  {
    let saved = Persistent::save(ctx, func.clone());
    return Ok(ExpectJs::new(ExpectTarget::Fn(saved)));
  }
  let ctor_name = value
    .as_object()
    .and_then(|o| o.get::<_, rquickjs::Value<'js>>("constructor").ok())
    .and_then(|c| {
      c.as_object()
        .and_then(|o| o.get::<_, rquickjs::Value<'js>>("name").ok())
    })
    .and_then(|n| n.as_string().and_then(|s| s.to_string().ok()))
    .filter(|s| !s.is_empty());
  let json: JsonValue = serde_from_js(ctx, value)?;
  Ok(ExpectJs::new(ExpectTarget::Value { value: json, ctor_name }))
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

  let expect_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, value: Value<'js>| -> rquickjs::Result<Value<'js>> {
      let inst = build_expect(&ctx, value)?;
      let class = Class::instance(ctx.clone(), inst)?;
      // Wrap in the JS proxy that translates `.not` (a getter) to
      // `_notInner()` (the method-bound clone).
      {
        let val = class.into_value();
        install_not_getter(&ctx, &val)?;
        Ok(val)
      }
    },
  )?;
  expect_fn.set_name("expect")?;

  // expect.poll(fn, opts?)
  let poll_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, generator: Function<'js>, opts: Opt<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let timeout_ms = opts
        .0
        .as_ref()
        .and_then(|v| {
          v.as_object()
            .and_then(|o| o.get::<_, rquickjs::Value<'js>>("timeout").ok())
        })
        .and_then(|v| {
          v.as_int()
            .map(|i| u64::try_from(i).unwrap_or(0))
            .or_else(|| v.as_number().map(|n| n as u64))
        })
        .unwrap_or_else(|| DEFAULT_EXPECT_TIMEOUT.as_millis() as u64);
      let saved = Persistent::save(&ctx, generator);
      let inst = ExpectPollJs {
        generator: saved,
        timeout: Duration::from_millis(timeout_ms),
        is_not: false,
      };
      let class = Class::instance(ctx.clone(), inst)?;
      {
        let val = class.into_value();
        install_poll_not_getter(&ctx, &val)?;
        Ok(val)
      }
    },
  )?;

  // expect.soft(target) – marks the resulting Expect as soft.
  let soft_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, value: Value<'js>| -> rquickjs::Result<Value<'js>> {
      let mut inst = build_expect(&ctx, value)?;
      inst.is_soft = true;
      let class = Class::instance(ctx.clone(), inst)?;
      {
        let val = class.into_value();
        install_not_getter(&ctx, &val)?;
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
  // tag. Mirrors Jest's `expect.not.objectContaining` etc.
  let not_obj = Object::new(ctx.clone())?;
  let any_fn_n = any_fn.clone();
  let anything_fn_n = anything_fn.clone();
  let array_containing_fn_n = array_containing_fn.clone();
  let object_containing_fn_n = object_containing_fn.clone();
  let string_containing_fn_n = string_containing_fn.clone();
  let string_matching_fn_n = string_matching_fn.clone();
  let close_to_fn_n = close_to_fn.clone();
  install_not_asym(ctx, &not_obj, "any", any_fn_n)?;
  install_not_asym(ctx, &not_obj, "anything", anything_fn_n)?;
  install_not_asym(ctx, &not_obj, "arrayContaining", array_containing_fn_n)?;
  install_not_asym(ctx, &not_obj, "objectContaining", object_containing_fn_n)?;
  install_not_asym(ctx, &not_obj, "stringContaining", string_containing_fn_n)?;
  install_not_asym(ctx, &not_obj, "stringMatching", string_matching_fn_n)?;
  install_not_asym(ctx, &not_obj, "closeTo", close_to_fn_n)?;

  // Attach the helpers to expect()'s own properties.
  let expect_obj = expect_fn.as_object().ok_or_else(|| {
    rquickjs::Error::new_from_js_message("expect", "install", "expect Function has no object representation")
  })?;
  expect_obj.set("poll", poll_fn)?;
  expect_obj.set("soft", soft_fn)?;
  expect_obj.set("any", any_fn)?;
  expect_obj.set("anything", anything_fn)?;
  expect_obj.set("arrayContaining", array_containing_fn)?;
  expect_obj.set("objectContaining", object_containing_fn)?;
  expect_obj.set("stringContaining", string_containing_fn)?;
  expect_obj.set("stringMatching", string_matching_fn)?;
  expect_obj.set("closeTo", close_to_fn)?;
  expect_obj.set("not", not_obj)?;

  ctx.globals().set("expect", expect_fn)?;
  crate::bindings::runtime::mirror_global(ctx, "expect")?;
  Ok(())
}

fn install_not_asym<'js>(
  ctx: &Ctx<'js>,
  not_obj: &Object<'js>,
  name: &str,
  inner: Function<'js>,
) -> rquickjs::Result<()> {
  let wrapped = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: rquickjs::function::Rest<Value<'js>>| -> rquickjs::Result<Object<'js>> {
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
fn install_not_getter<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let inst_clone = instance.clone();
  let getter = Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
    let class = Class::<ExpectJs>::from_value(&inst_clone)?;
    let inverted = class.borrow().not_inner();
    let new_class = Class::instance(ctx.clone(), inverted)?;
    let new_val = new_class.into_value();
    install_not_getter(&ctx, &new_val)?;
    Ok(new_val)
  })?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter)?;
  descriptor.set("configurable", true)?;
  let _: rquickjs::Value<'js> = define_property.call((instance.clone(), "not", descriptor))?;
  Ok(())
}

fn install_poll_not_getter<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let inst_clone = instance.clone();
  let getter = Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
    let class = Class::<ExpectPollJs>::from_value(&inst_clone)?;
    let inverted = class.borrow().not_inner();
    let new_class = Class::instance(ctx.clone(), inverted)?;
    let new_val = new_class.into_value();
    install_poll_not_getter(&ctx, &new_val)?;
    Ok(new_val)
  })?;
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
