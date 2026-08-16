//! `node:assert`.
//!
//! Written here rather than vendored: upstream `llrt_assert` is a single
//! `ok`. Structural comparisons run through
//! [`deep_equal`](super::deep_equal), the same function `util.isDeepStrictEqual`
//! uses, and failure messages render through the one
//! [`Inspector`](super::inspect::Inspector).

use rquickjs::function::{Async, Func, Opt, Rest};
use rquickjs::{Ctx, Function, Object, Promise, Result, Value};

use super::deep_equal::{Mode, deep_equal, loose_equal, strict_equal};
use super::inspect::Inspector;

fn render(value: &Value<'_>) -> String {
  let mut out = String::new();
  if Inspector::new(false).quoted().value(&mut out, value, 0).is_err() {
    out.push_str("<unrenderable>");
  }
  out
}

/// Throw an `AssertionError` carrying Node's diagnostic fields.
fn fail_with<'js>(
  ctx: &Ctx<'js>,
  message: Opt<Value<'js>>,
  generated: String,
  actual: Value<'js>,
  expected: Value<'js>,
  operator: &str,
) -> rquickjs::Error {
  // A message that is itself an Error is thrown as-is, as Node does.
  if let Some(value) = message.0.clone() {
    if value.is_error() {
      return ctx.throw(value);
    }
  }
  let text = match message.0.as_ref().and_then(rquickjs::Value::as_string) {
    Some(s) => s.to_string().unwrap_or(generated),
    None => generated,
  };

  let build = |ctx: &Ctx<'js>| -> Result<Value<'js>> {
    let error_ctor: rquickjs::function::Constructor<'js> = ctx.globals().get("Error")?;
    let error: Object<'js> = error_ctor.construct((text.clone(),))?;
    error.set("name", "AssertionError")?;
    error.set("code", "ERR_ASSERTION")?;
    error.set("actual", actual.clone())?;
    error.set("expected", expected.clone())?;
    error.set("operator", operator)?;
    error.set("generatedMessage", message.0.is_none())?;
    Ok(error.into_value())
  };
  match build(ctx) {
    Ok(error) => ctx.throw(error),
    Err(e) => e,
  }
}

fn truthy(value: &Value<'_>) -> bool {
  match value.type_of() {
    rquickjs::Type::Undefined | rquickjs::Type::Null | rquickjs::Type::Uninitialized => false,
    rquickjs::Type::Bool => value.as_bool().unwrap_or(false),
    rquickjs::Type::Int | rquickjs::Type::Float => value.as_number().is_some_and(|n| n != 0.0 && !n.is_nan()),
    rquickjs::Type::String => value
      .as_string()
      .and_then(|s| s.to_string().ok())
      .is_some_and(|s| !s.is_empty()),
    _ => true,
  }
}

fn ok<'js>(ctx: Ctx<'js>, value: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  if truthy(&value) {
    return Ok(());
  }
  let rendered = render(&value);
  Err(fail_with(
    &ctx,
    message,
    format!("The expression evaluated to a falsy value: {rendered}"),
    value,
    Value::new_bool(ctx.clone(), true),
    "==",
  ))
}

/// One comparison entry point: `passed` decides, `operator` and the
/// wording come from the caller.
fn compare<'js>(
  ctx: &Ctx<'js>,
  actual: Value<'js>,
  expected: Value<'js>,
  message: Opt<Value<'js>>,
  passed: bool,
  operator: &str,
  wording: &str,
) -> Result<()> {
  if passed {
    return Ok(());
  }
  let generated = format!("{wording}\n\n{} {operator} {}\n", render(&actual), render(&expected));
  Err(fail_with(ctx, message, generated, actual, expected, operator))
}

fn equal<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = loose_equal(&actual, &expected);
  compare(&ctx, actual, expected, message, passed, "==", "Expected values to be loosely equal:")
}

fn not_equal<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = !loose_equal(&actual, &expected);
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "!=",
    "Expected values not to be loosely equal:",
  )
}

fn strict_eq<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = strict_equal(&actual, &expected);
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "strictEqual",
    "Expected values to be strictly equal:",
  )
}

fn not_strict_eq<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = !strict_equal(&actual, &expected);
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "notStrictEqual",
    "Expected values not to be strictly equal:",
  )
}

fn deep_eq<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = deep_equal(&actual, &expected, Mode::Loose)?;
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "deepEqual",
    "Expected values to be loosely deep-equal:",
  )
}

fn not_deep_eq<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = !deep_equal(&actual, &expected, Mode::Loose)?;
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "notDeepEqual",
    "Expected values not to be loosely deep-equal:",
  )
}

fn deep_strict_eq<'js>(ctx: Ctx<'js>, actual: Value<'js>, expected: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let passed = deep_equal(&actual, &expected, Mode::Strict)?;
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "deepStrictEqual",
    "Expected values to be strictly deep-equal:",
  )
}

fn not_deep_strict_eq<'js>(
  ctx: Ctx<'js>,
  actual: Value<'js>,
  expected: Value<'js>,
  message: Opt<Value<'js>>,
) -> Result<()> {
  let passed = !deep_equal(&actual, &expected, Mode::Strict)?;
  compare(
    &ctx,
    actual,
    expected,
    message,
    passed,
    "notDeepStrictEqual",
    "Expected values not to be strictly deep-equal:",
  )
}

fn regexp_test<'js>(regexp: &Object<'js>, subject: &Value<'js>) -> Result<bool> {
  let test: Function<'js> = regexp.get("test")?;
  test.call((rquickjs::function::This(regexp.clone()), subject.clone()))
}

fn matches<'js>(ctx: Ctx<'js>, subject: Value<'js>, regexp: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let Some(re) = regexp.as_object() else {
    return Err(rquickjs::Exception::throw_type(
      &ctx,
      "The \"regexp\" argument must be an instance of RegExp",
    ));
  };
  let passed = regexp_test(re, &subject)?;
  compare(
    &ctx,
    subject,
    regexp.clone(),
    message,
    passed,
    "match",
    "The input did not match the regular expression:",
  )
}

fn does_not_match<'js>(ctx: Ctx<'js>, subject: Value<'js>, regexp: Value<'js>, message: Opt<Value<'js>>) -> Result<()> {
  let Some(re) = regexp.as_object() else {
    return Err(rquickjs::Exception::throw_type(
      &ctx,
      "The \"regexp\" argument must be an instance of RegExp",
    ));
  };
  let passed = !regexp_test(re, &subject)?;
  compare(
    &ctx,
    subject,
    regexp.clone(),
    message,
    passed,
    "doesNotMatch",
    "The input was expected to not match the regular expression:",
  )
}

/// Does a thrown value satisfy the expectation Node accepts: a RegExp
/// against its message, a constructor via `instanceof`, or an object whose
/// listed properties must deep-strict-match.
fn thrown_matches<'js>(ctx: &Ctx<'js>, error: &Value<'js>, expected: &Value<'js>) -> Result<bool> {
  let Some(expected_obj) = expected.as_object() else {
    return Ok(true);
  };

  if expected.is_function() {
    let instance_of: Function<'js> = ctx.eval("(e, C) => e instanceof C")?;
    return instance_of.call((error.clone(), expected.clone()));
  }

  if regexp_source_present(expected_obj)? {
    let message: Value<'js> = error
      .as_object()
      .map_or_else(|| Ok(error.clone()), |o| o.get::<_, Value<'js>>("message"))?;
    return regexp_test(expected_obj, &message);
  }

  // A plain object: every listed property must deep-strict-match.
  let Some(error_obj) = error.as_object() else {
    return Ok(false);
  };
  for key in expected_obj.keys::<String>() {
    let key = key?;
    let want: Value<'js> = expected_obj.get(key.as_str())?;
    let got: Value<'js> = error_obj.get(key.as_str())?;
    if !deep_equal(&got, &want, Mode::Strict)? {
      return Ok(false);
    }
  }
  Ok(true)
}

fn regexp_source_present(object: &Object<'_>) -> Result<bool> {
  Ok(object.get::<_, Value<'_>>("source").is_ok_and(|v| v.is_string()) && object.get::<_, Value<'_>>("test").is_ok())
}

fn throws<'js>(ctx: Ctx<'js>, body: Function<'js>, rest: Rest<Value<'js>>) -> Result<()> {
  let (expected, message) = split_expectation(&rest.0);
  match body.call::<_, Value<'js>>(()) {
    Err(_) => {
      let caught = ctx.catch();
      if let Some(expected) = expected {
        if !thrown_matches(&ctx, &caught, &expected)? {
          return Err(fail_with(
            &ctx,
            message,
            format!("The error did not match the expectation: {}", render(&caught)),
            caught,
            expected,
            "throws",
          ));
        }
      }
      Ok(())
    },
    Ok(_) => Err(fail_with(
      &ctx,
      message,
      "Missing expected exception.".to_string(),
      Value::new_undefined(ctx.clone()),
      expected.unwrap_or_else(|| Value::new_undefined(ctx.clone())),
      "throws",
    )),
  }
}

fn does_not_throw<'js>(ctx: Ctx<'js>, body: Function<'js>, rest: Rest<Value<'js>>) -> Result<()> {
  let (_, message) = split_expectation(&rest.0);
  match body.call::<_, Value<'js>>(()) {
    Ok(_) => Ok(()),
    Err(_) => {
      let caught = ctx.catch();
      Err(fail_with(
        &ctx,
        message,
        format!("Got unwanted exception: {}", render(&caught)),
        caught,
        Value::new_undefined(ctx.clone()),
        "doesNotThrow",
      ))
    },
  }
}

/// `(expected?, message?)`: a string in the first slot is the message,
/// anything else is the expectation.
fn split_expectation<'js>(rest: &[Value<'js>]) -> (Option<Value<'js>>, Opt<Value<'js>>) {
  match rest {
    [] => (None, Opt(None)),
    [only] if only.is_string() => (None, Opt(Some(only.clone()))),
    [only] => (Some(only.clone()), Opt(None)),
    [first, second, ..] => (Some(first.clone()), Opt(Some(second.clone()))),
  }
}

async fn rejects<'js>(ctx: Ctx<'js>, subject: Value<'js>, rest: Rest<Value<'js>>) -> Result<()> {
  let (expected, message) = split_expectation(&rest.0);
  match await_subject(&ctx, subject).await {
    Err(_) => {
      let caught = ctx.catch();
      if let Some(expected) = expected {
        if !thrown_matches(&ctx, &caught, &expected)? {
          return Err(fail_with(
            &ctx,
            message,
            format!("The rejection did not match the expectation: {}", render(&caught)),
            caught,
            expected,
            "rejects",
          ));
        }
      }
      Ok(())
    },
    Ok(()) => Err(fail_with(
      &ctx,
      message,
      "Missing expected rejection.".to_string(),
      Value::new_undefined(ctx.clone()),
      expected.unwrap_or_else(|| Value::new_undefined(ctx.clone())),
      "rejects",
    )),
  }
}

async fn does_not_reject<'js>(ctx: Ctx<'js>, subject: Value<'js>, rest: Rest<Value<'js>>) -> Result<()> {
  let (_, message) = split_expectation(&rest.0);
  match await_subject(&ctx, subject).await {
    Ok(()) => Ok(()),
    Err(_) => {
      let caught = ctx.catch();
      Err(fail_with(
        &ctx,
        message,
        format!("Got unwanted rejection: {}", render(&caught)),
        caught,
        Value::new_undefined(ctx.clone()),
        "doesNotReject",
      ))
    },
  }
}

/// Both async assertions accept a promise or a function returning one.
async fn await_subject<'js>(ctx: &Ctx<'js>, subject: Value<'js>) -> Result<()> {
  let promise: Value<'js> = if let Some(f) = subject.as_function() {
    f.call(())?
  } else {
    subject
  };
  match promise.into_promise() {
    Some(p) => p.into_future::<Value<'js>>().await.map(|_| ()),
    None => Err(rquickjs::Exception::throw_type(
      ctx,
      "The \"promiseFn\" argument must be a function or a Promise",
    )),
  }
}

fn fail<'js>(ctx: Ctx<'js>, message: Opt<Value<'js>>) -> Result<()> {
  Err(fail_with(
    &ctx,
    message,
    "Failed".to_string(),
    Value::new_undefined(ctx.clone()),
    Value::new_undefined(ctx.clone()),
    "fail",
  ))
}

fn if_error<'js>(ctx: Ctx<'js>, value: Value<'js>) -> Result<()> {
  if value.is_null() || value.is_undefined() {
    return Ok(());
  }
  let rendered = render(&value);
  Err(fail_with(
    &ctx,
    Opt(None),
    format!("ifError got unwanted exception: {rendered}"),
    value,
    Value::new_null(ctx.clone()),
    "ifError",
  ))
}

/// The names [`assert_object`] sets, for a module's export list.
pub const ASSERT_MEMBERS: &[&str] = &[
  "deepEqual",
  "deepStrictEqual",
  "doesNotMatch",
  "doesNotReject",
  "doesNotThrow",
  "equal",
  "fail",
  "ifError",
  "match",
  "notDeepEqual",
  "notDeepStrictEqual",
  "notEqual",
  "notStrictEqual",
  "ok",
  "rejects",
  "strict",
  "strictEqual",
  "throws",
];

fn install_members<'js>(target: &Object<'js>, strict_mode: bool) -> Result<()> {
  target.set("ok", Func::from(ok))?;
  target.set("fail", Func::from(fail))?;
  target.set("ifError", Func::from(if_error))?;
  target.set("match", Func::from(matches))?;
  target.set("doesNotMatch", Func::from(does_not_match))?;
  target.set("throws", Func::from(throws))?;
  target.set("doesNotThrow", Func::from(does_not_throw))?;
  target.set("rejects", Func::from(Async(rejects)))?;
  target.set("doesNotReject", Func::from(Async(does_not_reject)))?;
  target.set("strictEqual", Func::from(strict_eq))?;
  target.set("notStrictEqual", Func::from(not_strict_eq))?;
  target.set("deepStrictEqual", Func::from(deep_strict_eq))?;
  target.set("notDeepStrictEqual", Func::from(not_deep_strict_eq))?;

  // In strict mode the loose entry points ARE the strict ones, which is
  // the whole difference between `assert` and `assert/strict`.
  if strict_mode {
    target.set("equal", Func::from(strict_eq))?;
    target.set("notEqual", Func::from(not_strict_eq))?;
    target.set("deepEqual", Func::from(deep_strict_eq))?;
    target.set("notDeepEqual", Func::from(not_deep_strict_eq))?;
  } else {
    target.set("equal", Func::from(equal))?;
    target.set("notEqual", Func::from(not_equal))?;
    target.set("deepEqual", Func::from(deep_eq))?;
    target.set("notDeepEqual", Func::from(not_deep_eq))?;
  }
  Ok(())
}

/// The `assert` module object: a callable that is `assert.ok`, carrying
/// every assertion as a property, plus `assert.strict`.
///
/// # Errors
///
/// Propagates the property writes it makes.
pub fn assert_object<'js>(ctx: &Ctx<'js>, strict_mode: bool) -> Result<Object<'js>> {
  let callable = Function::new(ctx.clone(), ok)?.with_name("assert")?;
  let object = callable
    .as_object()
    .cloned()
    .ok_or_else(|| rquickjs::Error::new_loading("assert"))?;
  install_members(&object, strict_mode)?;

  if strict_mode {
    object.set("strict", object.clone())?;
  } else {
    let strict = Function::new(ctx.clone(), ok)?.with_name("assert")?;
    let strict_object = strict
      .as_object()
      .cloned()
      .ok_or_else(|| rquickjs::Error::new_loading("assert"))?;
    install_members(&strict_object, true)?;
    strict_object.set("strict", strict_object.clone())?;
    object.set("strict", strict_object)?;
  }
  Ok(object)
}

/// A promise-returning helper is only reachable from JS, so the module
/// needs the async runtime marker type in scope.
type _AsyncMarker<'js> = Promise<'js>;
