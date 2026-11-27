//! Matcher methods on [`ExpectJs`]: generic JS value matchers and
//! Playwright web-first / snapshot / `APIResponse` matchers.

use std::time::Duration;

use ferridriver_expect::{AssertionFailure, POLL_INTERVALS, StringOrRegex, ThrowMatcher, ThrownError};
use rquickjs::{Ctx, Persistent, Value, function::Opt};
use serde_json::Value as JsonValue;

use crate::bindings::convert::{serde_from_js, type_error};

use super::extend::parse_property_path;
use super::settled::mask_selectors;
use super::{
  ExpectJs, JsLive, SubjectKind, bool_field, expect_defaults, f64_field, is_string_or_regex, opts_obj,
  parse_string_or_regex, parse_string_or_regex_array, snapshot_failure, snapshot_name_and_options, text_match_options,
  u64_array_field, u64_field, unsupported_snapshot_subject,
};

#[rquickjs::methods]
#[expect(
  clippy::needless_pass_by_value,
  reason = "rquickjs hands methods their Ctx and JS arguments by value"
)]
impl ExpectJs {
  // ── modifiers ────────────────────────────────────────────────────

  /// `.not` getter — returns a new `ExpectJs` with the negation flag
  /// toggled. Implemented as a method so `expect(x).not.toBe(y)` reads
  /// naturally; the JS-side `Object.defineProperty` shim in
  /// `install_expect` adapts it into a getter on the class prototype.
  #[qjs(rename = "_notInner")]
  #[must_use]
  pub fn not_inner(&self) -> ExpectJs {
    self.clone_with(|e| e.is_not = !e.is_not)
  }

  /// `.soft` modifier.
  #[qjs(rename = "soft")]
  #[must_use]
  pub fn soft(&self) -> ExpectJs {
    self.clone_with(|e| e.is_soft = true)
  }

  /// Override the timeout for web-first matchers on this assertion
  /// (milliseconds).
  #[qjs(rename = "withTimeout")]
  #[must_use]
  pub fn with_timeout(&self, timeout_ms: u32) -> ExpectJs {
    self.clone_with(|e| e.timeout = Duration::from_millis(u64::from(timeout_ms)))
  }

  /// Attach a custom failure-message prefix.
  #[qjs(rename = "withMessage")]
  #[must_use]
  pub fn with_message(&self, msg: String) -> ExpectJs {
    self.clone_with(|e| e.message = Some(msg))
  }

  // ── value matchers ───────────────────────────────────────────────

  /// Playwright: `Object.is` equality (jest `expectLibrary.ts:623`) —
  /// two structurally equal objects are NOT `toBe`-equal, and a
  /// reference IS `toBe`-equal to itself.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBe")]
  pub fn to_be<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    let expected = JsLive(expected);
    self
      .live_expect(&actual)
      .to_be(&expected)
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toEqual`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
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
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toStrictEqual`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
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
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeNull`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeNull")]
  pub fn to_be_null(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_null()
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeUndefined`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeUndefined")]
  pub fn to_be_undefined(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_undefined()
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeDefined`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeDefined")]
  pub fn to_be_defined(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_defined()
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeTruthy`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeTruthy")]
  pub fn to_be_truthy(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_truthy()
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeFalsy`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeFalsy")]
  pub fn to_be_falsy(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_falsy()
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeNaN`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeNaN")]
  pub fn to_be_nan(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_be_nan()
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toBeCloseTo`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` when the
  /// subject cannot be serialized.
  #[qjs(rename = "toBeCloseTo")]
  pub fn to_be_close_to(&self, ctx: Ctx<'_>, expected: f64, digits: Opt<u8>) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_close_to(expected, digits.0)
      .or_else(|e| self.fail(&ctx, e))
  }

  /// Playwright: `toBeGreaterThan`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` when the
  /// subject cannot be serialized.
  #[qjs(rename = "toBeGreaterThan")]
  pub fn to_be_greater_than(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_greater_than(expected)
      .or_else(|e| self.fail(&ctx, e))
  }

  /// Playwright: `toBeGreaterThanOrEqual`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` when the
  /// subject cannot be serialized.
  #[qjs(rename = "toBeGreaterThanOrEqual")]
  pub fn to_be_greater_than_or_equal(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_greater_than_or_equal(expected)
      .or_else(|e| self.fail(&ctx, e))
  }

  /// Playwright: `toBeLessThan`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` when the
  /// subject cannot be serialized.
  #[qjs(rename = "toBeLessThan")]
  pub fn to_be_less_than(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_less_than(expected)
      .or_else(|e| self.fail(&ctx, e))
  }

  /// Playwright: `toBeLessThanOrEqual`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` when the
  /// subject cannot be serialized.
  #[qjs(rename = "toBeLessThanOrEqual")]
  pub fn to_be_less_than_or_equal(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    self
      .build_value_expect(&ctx)?
      .to_be_less_than_or_equal(expected)
      .or_else(|e| self.fail(&ctx, e))
  }

  /// Playwright: substring for a string receiver, otherwise
  /// `[...received].indexOf(expected)` — strict equality over the live
  /// items, so a structurally equal copy is NOT contained.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toContain")]
  pub fn to_contain<'js>(&self, ctx: Ctx<'js>, expected: Value<'js>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    let expected = JsLive(expected);
    self
      .live_expect(&actual)
      .to_contain(&expected)
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toContainEqual`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
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
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright reads the receiver's own `.length`, so a function's
  /// arity and a typed array's length answer here too.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toHaveLength")]
  pub fn to_have_length(&self, ctx: Ctx<'_>, expected: f64) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    self
      .live_expect(&actual)
      .to_have_length(expected)
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toHaveProperty`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a
  /// malformed property path or a value the matcher cannot compare.
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
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toMatch`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` when the
  /// subject cannot be serialized or `pattern` is not a string or `RegExp`.
  #[qjs(rename = "toMatch")]
  pub fn to_match<'js>(&self, ctx: Ctx<'js>, pattern: Value<'js>) -> rquickjs::Result<()> {
    let pat = parse_string_or_regex(&ctx, &pattern)?;
    self
      .build_value_expect(&ctx)?
      .to_match(&pat)
      .or_else(|e| self.fail(&ctx, e))
  }

  /// Playwright: `toMatchObject`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
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
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: the real `instanceof` operator (jest
  /// `expectLibrary.ts:789`) — a subclass instance IS an instance of
  /// its base, which a constructor-name comparison cannot see.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the assertion fails (a soft assertion
  /// records the failure and returns instead), or a `TypeError` for a value
  /// the matcher cannot compare.
  #[qjs(rename = "toBeInstanceOf")]
  pub fn to_be_instance_of<'js>(&self, ctx: Ctx<'js>, ctor: Value<'js>) -> rquickjs::Result<()> {
    let actual = self.live(&ctx)?;
    let ctor = JsLive(ctor);
    self
      .live_expect(&actual)
      .to_be_instance_of(&ctor)
      .or_else(|e| self.live_fail(&ctx, e))
  }

  /// Playwright: `toThrow`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the function's outcome does not
  /// match, or a `TypeError` when the subject is not a function or `matcher`
  /// is an invalid `RegExp` or an unserializable object.
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
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
        me.build_locator_expect(&ctx, "toBeVisible")?
          .to_be_visible_with(want_visible)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeHidden(options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeEnabled(options?: { enabled?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
        me.build_locator_expect(&ctx, "toBeEnabled")?
          .to_be_enabled_with(want_enabled)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeDisabled(options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeChecked(options?: { checked?, indeterminate?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
        let options = ferridriver_expect::CheckedOptions {
          checked: bool_field(o.as_ref(), "checked"),
          indeterminate: bool_field(o.as_ref(), "indeterminate"),
        };
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toBeChecked")?
          .to_be_checked_with(options)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeEditable(options?: { editable?, timeout? })`.
  /// `editable: false` maps to the readonly assertion, matching
  /// Playwright's `to.be.readonly` lowering (not a plain negation).
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
        me.build_locator_expect(&ctx, "toBeEditable")?
          .to_be_editable_with(want_editable)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeAttached(options?: { attached?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
        me.build_locator_expect(&ctx, "toBeAttached")?
          .to_be_attached_with(want_attached)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeEmpty(options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeFocused(options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toBeInViewport(options?: { ratio?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveText(expected: string | RegExp | (string | RegExp)[],
  /// options?: { ignoreCase?, timeout?, useInnerText? })`. The array
  /// form asserts over EVERY element the locator resolves to.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
            .or_else(|e| self.fail(&ctx, e));
        }
        let exp = parse_string_or_regex(&ctx, &expected)?;
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveText")?
          .to_have_text_with(exp, text_match_options(o.as_ref()))
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toContainText(expected: string | RegExp | (string | RegExp)[],
  /// options?: { ignoreCase?, timeout?, useInnerText? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
            .or_else(|e| self.fail(&ctx, e));
        }
        let exp = parse_string_or_regex(&ctx, &expected)?;
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toContainText")?
          .to_contain_text_with(exp, text_match_options(o.as_ref()))
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveValue(value: string | RegExp, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveValues(values: Array<string | RegExp>,
  /// options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
        let values = expected
          .iter()
          .map(|v| parse_string_or_regex(&ctx, v))
          .collect::<rquickjs::Result<Vec<_>>>()?;
        let o = opts_obj(&options);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveValues")?
          .to_have_values(&values)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveCount(count: number, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright overloads: `toHaveAttribute(name, value: string | RegExp,
  /// options?: { ignoreCase?, timeout? })` and
  /// `toHaveAttribute(name, options?: { timeout? })` (presence check).
  /// A second argument that is a string/RegExp is the expected value;
  /// any other object is the options bag.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
        .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveClass(expected: string | RegExp | (string | RegExp)[],
  /// options?: { timeout? })`. The array form asserts one class list per
  /// element.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
        let o = opts_obj(&options);
        let me = self.for_call(o.as_ref());
        let e = me.build_locator_expect(&ctx, "toHaveClass")?;
        match parse_string_or_regex_array(&ctx, &expected)? {
          Some(list) => e.to_have_class_array(&list).await,
          None => e.to_have_class(parse_string_or_regex(&ctx, &expected)?).await,
        }
        .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toContainClass(expected: string | string[], options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
  #[qjs(rename = "toContainClass")]
  pub async fn to_contain_class<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let o = opts_obj(&options);
        let me = self.for_call(o.as_ref());
        let e = me.build_locator_expect(&ctx, "toContainClass")?;
        if let Some(list) = expected.as_array() {
          let mut classes = Vec::with_capacity(list.len());
          for item in list.iter::<Value<'js>>() {
            match item?.as_string() {
              Some(s) => classes.push(s.to_string()?),
              None => {
                return Err(type_error(
                  &ctx,
                  "\"expected\" argument in toContainClass cannot contain RegExp values",
                ));
              },
            }
          }
          return e.to_contain_class_array(&classes).await.or_else(|e| self.fail(&ctx, e));
        }
        let Some(class) = expected.as_string() else {
          return Err(type_error(
            &ctx,
            "\"expected\" argument in toContainClass cannot be a RegExp value",
          ));
        };
        e.to_contain_class(&class.to_string()?)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveCSS(name: string, value: string | RegExp,
  /// options?: { pseudo?: 'before' | 'after', timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
        let pseudo = match o
          .as_ref()
          .map(|o| o.get::<_, Option<String>>("pseudo"))
          .transpose()?
          .flatten()
        {
          Some(p) => Some(
            p.parse::<ferridriver_expect::CssPseudo>()
              .map_err(|e| type_error(&ctx, e))?,
          ),
          None => None,
        };
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveCSS")?
          .to_have_css_with(&name, exp, ferridriver_expect::HaveCssOptions { pseudo })
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveId(id: string | RegExp, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveRole(role: string, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// or an `Error` when the subject is not a `Locator`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveJSProperty(name: string, value: any,
  /// options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveAccessibleName(name: string | RegExp,
  /// options?: { ignoreCase?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
        let ignore_case = bool_field(o.as_ref(), "ignoreCase").unwrap_or(false);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveAccessibleName")?
          .to_have_accessible_name_with(exp, ignore_case)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveAccessibleDescription(description: string | RegExp,
  /// options?: { ignoreCase?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
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
        let ignore_case = bool_field(o.as_ref(), "ignoreCase").unwrap_or(false);
        self
          .for_call(o.as_ref())
          .build_locator_expect(&ctx, "toHaveAccessibleDescription")?
          .to_have_accessible_description_with(exp, ignore_case)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveAccessibleErrorMessage(errorMessage: string | RegExp,
  /// options?: { ignoreCase?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Locator`, or a `TypeError` for an
  /// expected value of the wrong shape.
  #[qjs(rename = "toHaveAccessibleErrorMessage")]
  pub async fn to_have_accessible_error_message<'js>(
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
          .build_locator_expect(&ctx, "toHaveAccessibleErrorMessage")?
          .to_have_accessible_error_message_with(exp, ignore_case)
          .await
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  // ── Page web-first matchers (delegated) ───────────────────────────

  /// Playwright: `toHaveTitle(title: string | RegExp, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Page`, or a `TypeError` when the
  /// expected value is not a string or `RegExp`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  /// Playwright: `toHaveURL(url: string | RegExp, options?: { ignoreCase?, timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the assertion does not hold within
  /// the timeout (a soft assertion records the failure and resolves instead),
  /// an `Error` when the subject is not a `Page`, or a `TypeError` when the
  /// expected value is not a string or `RegExp`.
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
          .or_else(|e| self.fail(&ctx, e))
      })
      .await
  }

  // ── Retrying function matcher ──────────────────────────────────────

  /// Playwright: `expect(callback).toPass(options?: { intervals?, timeout? })`.
  /// Retries the callback until it stops throwing. Timeout defaults to 0
  /// (unbounded, Playwright parity); intervals default to
  /// `[100, 250, 500, 1000]`. With `.not`, passes as soon as the callback
  /// throws.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when the callback is still throwing
  /// (or, with `.not`, still passing) at the timeout, or a `TypeError` when the
  /// subject is not a function.
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
        .or_else(|e| self.fail(&ctx, e))
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

  /// Playwright: `toMatchSnapshot(name?: string | string[], options?)` /
  /// `toMatchSnapshot(options?)` on a `string` or a `Buffer` —
  /// synchronous, as upstream; a `Buffer` holding an image is compared as
  /// one. A locator compares its text content (a ferridriver extension,
  /// so that form alone returns a promise).
  ///
  /// # Errors
  ///
  /// Throws an `Error` outside a running test, an `AssertionError` on a
  /// mismatch (a soft assertion records it instead), or a `TypeError` for an
  /// unresolved promise, an unsupported subject or a malformed `options`; for a
  /// `Locator` the returned promise rejects with those instead.
  #[qjs(rename = "toMatchSnapshot")]
  pub fn to_match_snapshot<'js>(
    &self,
    ctx: Ctx<'js>,
    name_or_options: Opt<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let bridge = crate::bindings::test::current_bridge(&ctx, "expect(...).toMatchSnapshot()")?;
    let (name, options) = snapshot_name_and_options(&ctx, name_or_options.0, options.0)?;
    let (is_not, is_soft) = (self.is_not, self.is_soft);
    match &self.subject.kind {
      SubjectKind::Locator(locator) => {
        let locator = locator.clone();
        let stack = self.stack.clone();
        let (promise, resolve, reject) = rquickjs::Promise::new(&ctx)?;
        let task_ctx = ctx.clone();
        ctx.spawn(async move {
          let settled = match locator.text_content().await {
            Ok(text) => bridge
              .match_value_snapshot(
                ferridriver_test::snapshot::SnapshotValue::Text(text.unwrap_or_default()),
                name,
                options,
                is_not,
              )
              .or_else(|m| snapshot_failure(&task_ctx, is_soft, m, stack.as_deref())),
            Err(e) => Err(type_error(
              &task_ctx,
              format!("toMatchSnapshot: reading text content: {e}"),
            )),
          };
          let _ = match settled {
            Ok(()) => resolve.call::<_, ()>(()),
            Err(_) => reject.call::<_, ()>((task_ctx.catch(),)),
          };
        });
        Ok(promise.into_value())
      },
      SubjectKind::Page(_) | SubjectKind::ApiResponse(_) => Err(unsupported_snapshot_subject(&ctx, "toMatchSnapshot")),
      SubjectKind::Value => {
        let received = self.subject.live.clone().restore(&ctx)?;
        if received.as_promise().is_some() {
          return Err(type_error(
            &ctx,
            "An unresolved Promise was passed to toMatchSnapshot(), make sure to resolve it by adding await to it.",
          ));
        }
        let received = match received.as_string() {
          Some(text) => ferridriver_test::snapshot::SnapshotValue::Text(text.to_string()?),
          None => ferridriver_test::snapshot::SnapshotValue::Bytes(
            ferrijs::std::node::bytes::value_to_bytes(&ctx, &received, None)
              .map_err(|_| unsupported_snapshot_subject(&ctx, "toMatchSnapshot"))?,
          ),
        };
        bridge
          .match_value_snapshot(received, name, options, is_not)
          .or_else(|m| snapshot_failure(&ctx, is_soft, m, self.stack.as_deref()))?;
        Ok(Value::new_undefined(ctx))
      },
    }
  }

  /// Playwright: `toHaveScreenshot(name?: string | string[], options?)` /
  /// `toHaveScreenshot(options?)` on a locator or page, `.not` included.
  ///
  /// # Errors
  ///
  /// Rejects with an `Error` outside a running test, a `TypeError` for an
  /// unsupported subject or a malformed `options`, or an `AssertionError` on a
  /// mismatch (a soft assertion records it and resolves instead).
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
        let is_name = |v: &Value<'js>| v.as_string().is_some() || v.as_array().is_some();
        let opts_val = match &name_or_options.0 {
          Some(v) if is_name(v) => options.0.clone(),
          Some(v) if v.as_object().is_some() => Some(v.clone()),
          _ => options.0.clone(),
        };
        let (name, mut opts_json) = snapshot_name_and_options(&ctx, name_or_options.0, options.0)?;
        // `mask` is `Locator[]`, and a Locator is a native class: a plain
        // serde walk of the bag turns each entry into an empty object and
        // the mask silently disappears. Read the instances off the object
        // and put their selectors back where the runner reads them. A
        // bare selector string is accepted too, because that is what a
        // caller with no Locator to hand can write.
        if let Some(obj) = opts_val.as_ref().and_then(rquickjs::Value::as_object)
          && let Some(mask) = mask_selectors(obj)?
        {
          opts_json["mask"] = serde_json::Value::Array(mask);
        }
        let target = self.snapshot_target(&ctx, "toHaveScreenshot")?;
        bridge
          .match_screenshot(target, name, opts_json, self.is_not)
          .await
          .or_else(|m| self.fail(&ctx, AssertionFailure::new(m, None).with_soft(self.is_soft)))
      })
      .await
  }

  /// Playwright: `toMatchAriaSnapshot(expected: string, options?: { timeout? })`
  /// on a locator or page.
  ///
  /// # Errors
  ///
  /// Rejects with an `Error` outside a running test, a `TypeError` for an
  /// unsupported subject, or an `AssertionError` when the snapshot does not
  /// match within the timeout (a soft assertion records it and resolves
  /// instead).
  #[qjs(rename = "toMatchAriaSnapshot")]
  pub async fn to_match_aria_snapshot<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    expected: Opt<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let bridge = crate::bindings::test::current_bridge(&ctx, "expect(...).toMatchAriaSnapshot()")?;
        let o = opts_obj(&options);
        let mut timeout_ms = u64_field(o.as_ref(), "timeout");
        // `toMatchAriaSnapshot(template, options?)` or, reading the
        // template from a snapshot file, `toMatchAriaSnapshot({ name?, timeout? })`.
        let expected = match expected.0 {
          Some(v) if v.as_string().is_some() => ferridriver_test::host::AriaExpectation::Template(
            v.as_string()
              .map(rquickjs::String::to_string)
              .transpose()?
              .unwrap_or_default(),
          ),
          Some(v) if v.as_object().is_some() => {
            let bag = v.as_object();
            timeout_ms = u64_field(bag, "timeout").or(timeout_ms);
            let name = match bag.map(|b| b.get::<_, Value<'js>>("name")).transpose()? {
              Some(n) if n.as_string().is_some() => ferridriver_test::snapshot_path::SnapshotName::One(
                n.as_string()
                  .map(rquickjs::String::to_string)
                  .transpose()?
                  .unwrap_or_default(),
              ),
              Some(n) if n.as_array().is_some() => ferridriver_test::snapshot_path::SnapshotName::Segments(
                n.as_array()
                  .map(|a| a.iter::<String>().collect::<rquickjs::Result<Vec<_>>>())
                  .transpose()?
                  .unwrap_or_default(),
              ),
              _ => ferridriver_test::snapshot_path::SnapshotName::Anonymous,
            };
            ferridriver_test::host::AriaExpectation::File(name)
          },
          _ => ferridriver_test::host::AriaExpectation::File(ferridriver_test::snapshot_path::SnapshotName::Anonymous),
        };
        let target = self.snapshot_target(&ctx, "toMatchAriaSnapshot")?;
        bridge
          .match_aria_snapshot(target, expected, self.is_not, timeout_ms)
          .await
          .or_else(|m| self.fail(&ctx, AssertionFailure::new(m, None).with_soft(self.is_soft)))
      })
      .await
  }

  // ── APIResponse matcher (delegated) ──────────────────────────────

  /// Playwright: `toBeOK`.
  ///
  /// # Errors
  ///
  /// Throws an `AssertionError` when the status is outside 200-299 (inside
  /// it under `.not`; a soft assertion records the failure and returns
  /// instead), or an `Error` when the subject is not an `APIResponse`.
  #[qjs(rename = "toBeOK")]
  pub fn to_be_ok(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self
      .build_api_response_expect(&ctx, "toBeOK")?
      .to_be_ok()
      .or_else(|e| self.fail(&ctx, e))
  }
}

pub(super) fn parse_throw_matcher<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ThrowMatcher> {
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
      let re = StringOrRegex::regex(pat, flags)
        .compile()
        .transpose()
        .map_err(|e| type_error(ctx, format!("expect: invalid RegExp: {e}")))?
        .ok_or_else(|| type_error(ctx, "expect: invalid RegExp"))?;
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

pub(super) fn extract_error<'js>(v: &Value<'js>) -> (String, Option<String>) {
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
