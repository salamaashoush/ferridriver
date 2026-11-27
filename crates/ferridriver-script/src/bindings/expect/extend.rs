//! `expect.extend`, `mergeExpects`, custom asymmetric matchers, and
//! `MatcherContext` / `MatcherUtils` marshaling.

use std::time::Duration;

use ferridriver_expect::{LiveValue, PromiseMode};
use rquickjs::{Array, Class, Ctx, Function, IntoJs, Object, Value};
use serde_json::Value as JsonValue;

use crate::bindings::convert::{json_to_js, type_error};

use super::live::JsLive;
use super::settled::{SettledFuture, define_hidden, matcher_step_wrapper};
use super::{ExpectJs, create_expect, make_asymmetric, type_name};

/// Per-realm latch flipped once `expect.extend` populates any custom matcher.
/// Avoids JS prototype/table lookups on every `expect(x)` call in suites that
/// never register custom matchers.
#[derive(Default, rquickjs::JsLifetime)]
pub(super) struct CustomMatchersActive(std::cell::Cell<bool>);

pub(super) fn has_custom_matchers(ctx: &Ctx<'_>) -> bool {
  ctx.userdata::<CustomMatchersActive>().is_none_or(|flag| flag.0.get())
}

pub(super) fn mark_custom_matchers_active(ctx: &Ctx<'_>) {
  if let Some(flag) = ctx.userdata::<CustomMatchersActive>() {
    flag.0.set(true);
  }
}

/// The body of `expect.extend`, both halves of it.
pub(super) fn extend_expect<'js>(
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
      mark_custom_matchers_active(ctx);
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
///
/// # Errors
///
/// When reading an argument's matcher table or building the merged
/// `expect` fails.
pub fn merge_expects<'js>(ctx: &Ctx<'js>, expects: Vec<Value<'js>>) -> rquickjs::Result<Function<'js>> {
  let merged = Object::new(ctx.clone())?;
  for e in expects {
    let Some(obj) = e.as_object() else { continue };
    let Some(index) = obj.get::<_, Value<'js>>(EXPECT_TABLE).ok().and_then(|v| v.as_number()) else {
      continue;
    };
    let table = matcher_table(ctx, crate::bindings::convert::f64_to_u32(index))?;
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
pub(super) fn install_custom_asym<'js>(
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
pub(super) struct TableEvaluator<'js> {
  pub(super) ctx: Ctx<'js>,
  pub(super) table: u32,
  pub(super) timeout: Duration,
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
  pub(super) fn evaluator(&self, ctx: &Ctx<'js>) -> Option<TableEvaluator<'js>> {
    (!self.matcher_names.is_empty()).then(|| TableEvaluator {
      ctx: ctx.clone(),
      table: self.matcher_table,
      timeout: self.timeout,
    })
  }
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
pub(super) const EXPECT_TABLE: &str = "_matcherTable";

fn matcher_tables<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Array<'js>> {
  let proto = Class::<ExpectJs>::prototype(ctx)?
    .ok_or_else(|| type_error(ctx, "expect.extend: the Expect class has no prototype in this context"))?;
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
  Ok(crate::bindings::convert::f64_to_u32(len).saturating_sub(1))
}

/// The matcher names currently in `table`.
pub(super) fn table_names(ctx: &Ctx<'_>, index: u32) -> rquickjs::Result<Vec<String>> {
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
    .ok_or_else(|| type_error(ctx, "expect.extend: this expect's matcher table is missing"))
}

/// The `this` a matcher body reads — Playwright's `MatcherContext`.
fn matcher_context<'js>(ctx: &Ctx<'js>, cx: &ferridriver_expect::MatcherContext) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("isNot", cx.is_not)?;
  obj.set("isSoft", cx.is_soft)?;
  obj.set("promise", cx.promise.map_or("", PromiseMode::as_str))?;
  obj.set("timeout", crate::bindings::convert::duration_ms_to_f64(cx.timeout))?;
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
pub(super) fn parse_property_path<'js>(
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
        out.push(PropSegment::Index(crate::bindings::convert::f64_to_usize(n)));
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
pub(super) fn install_user_matchers<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
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
    // A custom matcher runs the test's own code, so what it calls nests
    // under its step.
    let stepped: Value<'js> = matcher_step_wrapper(ctx)?.call((name.as_str(), f, true))?;
    obj.set(name, stepped)?;
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
      ferridriver_expect::finalize(&cx, &result).or_else(|e| base.fail(&ctx_owned, e))
    });
    return rquickjs::promise::Promised::from(fut).into_js(ctx);
  }
  let result = parse_matcher_result(ctx, name, &outcome)?;
  ferridriver_expect::finalize(&cx, &result).or_else(|e| base.fail(ctx, e))?;
  Ok(Value::new_undefined(ctx.clone()))
}
