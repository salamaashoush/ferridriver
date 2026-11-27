//! `.resolves` / `.rejects` promise matchers and step wrapping for all
//! built-in and custom matchers.

use ferridriver_expect::{LiveValue, PromiseMismatch, PromiseMode, ThrownError};
use rquickjs::{Class, Ctx, Function, IntoJs, Object, Value, function::Opt};
use serde_json::Value as JsonValue;

use crate::bindings::convert::type_error;

use super::extend::install_user_matchers;
use super::live::JsLive;
use super::matchers::extract_error;
use super::poll::ExpectPollJs;
use super::{ExpectJs, SubjectKind, assertion_to_rq, capture_stack};

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
pub(super) type SettledFuture<'js> = std::pin::Pin<Box<dyn std::future::Future<Output = rquickjs::Result<()>> + 'js>>;

/// Where the settled-matcher object keeps the assertion it came from.
/// Non-enumerable, and a plain property so `QuickJS` traces it — a native
/// closure must never capture a JS value.
const SETTLED_SOURCE: &str = "_expect";
const SETTLED_NEGATED: &str = "_not";

pub(super) fn install_settled_getters<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
  for mode in [PromiseMode::Resolves, PromiseMode::Rejects] {
    let getter = Function::new(
      ctx.clone(),
      move |ctx: Ctx<'js>, this: rquickjs::function::This<Value<'js>>| -> rquickjs::Result<Object<'js>> {
        if let Ok(class) = Class::<ExpectJs>::from_value(&this.0) {
          let mut borrowed = class.borrow_mut();
          if borrowed.stack.is_none() {
            borrowed.stack = capture_stack(&ctx);
          }
        }
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
    type_error(
      ctx,
      "expect.resolves: the Expect class has no prototype in this context",
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
    type_error(
      &ctx,
      "expect.resolves: a settled matcher was called without its receiver",
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
      base.stack.as_deref(),
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

  let has_user_matchers = !settled_expect.matcher_names.is_empty();
  let instance = Class::instance(ctx.clone(), settled_expect)?.into_value();
  if has_user_matchers {
    install_user_matchers(&ctx, &instance)?;
  }
  let matcher: Function<'js> = instance
    .as_object()
    .and_then(|o| o.get(name.as_str()).ok())
    .ok_or_else(|| type_error(&ctx, "expect.resolves: no such matcher on the settled assertion"))?;
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
pub(super) fn define_hidden<'js>(
  ctx: &Ctx<'js>,
  target: &Object<'js>,
  key: &str,
  value: Value<'js>,
) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("value", value)?;
  descriptor.set("configurable", true)?;
  let _: Value<'js> = define_property.call((target.clone(), key, descriptor))?;
  Ok(())
}

/// `Object.defineProperty(target, key, { get, configurable: true })`.
pub(super) fn define_accessor<'js>(
  ctx: &Ctx<'js>,
  target: &Value<'js>,
  key: &str,
  getter: Function<'js>,
) -> rquickjs::Result<()> {
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
pub(super) fn install_poll_settled_refusal<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
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

/// Upstream's `expect.poll` takes every generic matcher: it polls the
/// value and runs `expect(value)[matcher](...args)` on it until that
/// passes, and on timeout throws the last failure with its call log. The
/// matchers `ExpectPollJs` implements itself stay native; the proxy adds
/// the rest.
///
/// The poll is one step; the matcher it runs on each value is not a step
/// of its own, so it is called through the unwrapped matcher.
const GENERIC_POLL_MATCHERS: &str = r"(target, expect, step) => new Proxy(target, {
  get(t, prop) {
    if (typeof prop !== 'string' || prop in t) {
      const value = Reflect.get(t, prop, t);
      return typeof value === 'function' ? value.bind(t) : value;
    }
    if (!prop.startsWith('to')) return undefined;
    const poll = step(prop, async (...args) => {
      const { generator, timeout, intervals, isNot } = t._pollInfo();
      const deadline = timeout ? Date.now() + timeout : Infinity;
      let index = 0;
      let last;
      for (;;) {
        const value = await generator();
        try {
          const subject = isNot ? expect(value).not : expect(value);
          const matcher = subject[prop];
          await (matcher._rawMatcher ?? matcher).apply(subject, args);
          return;
        } catch (error) {
          last = error;
        }
        const left = deadline - Date.now();
        if (left <= 0) break;
        const step = intervals[Math.min(index++, intervals.length - 1)];
        await new Promise(resolve => setTimeout(resolve, Math.min(step, left)));
      }
      const note = `Timeout ${timeout}ms exceeded while waiting on the predicate`;
      if (last !== null && typeof last === 'object' && 'message' in last) {
        last.message = `${last.message}\n\nCall Log:\n- ${note}`;
        throw last;
      }
      throw new Error(note);
    }, true);
    return (...args) => poll.apply(t, args);
  },
})";

pub(super) fn with_generic_poll_matchers<'js>(ctx: &Ctx<'js>, instance: Value<'js>) -> rquickjs::Result<Value<'js>> {
  let wrap: Function<'js> = ctx.eval(GENERIC_POLL_MATCHERS)?;
  let expect: Value<'js> = ctx.globals().get("expect")?;
  wrap.call((instance, expect, matcher_step_wrapper(ctx)?))
}

pub(super) fn install_poll_not_getter<'js>(ctx: &Ctx<'js>, instance: &Value<'js>) -> rquickjs::Result<()> {
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
      with_generic_poll_matchers(&ctx, new_val)
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

/// The `mask` entries of a `toHaveScreenshot` option bag, as selectors.
///
/// Returns `None` when the bag carries no `mask` at all, so an absent
/// key stays absent rather than becoming an empty list.
pub(super) fn mask_selectors<'js>(obj: &rquickjs::Object<'js>) -> rquickjs::Result<Option<Vec<serde_json::Value>>> {
  let v: rquickjs::Value<'js> = obj.get("mask")?;
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  let arr = v.into_array().ok_or_else(|| {
    type_error(
      obj.ctx(),
      "toHaveScreenshot options: mask: expected an array of Locator",
    )
  })?;
  let mut out = Vec::with_capacity(arr.len());
  for item in arr.iter::<rquickjs::Value<'js>>() {
    let item = item?;
    if let Some(s) = item.as_string() {
      out.push(serde_json::Value::String(s.to_string()?));
    } else if let Ok(class) = rquickjs::Class::<crate::bindings::locator::LocatorJs>::from_value(&item) {
      out.push(serde_json::Value::String(
        class.borrow().inner_ref().selector().to_string(),
      ));
    } else {
      return Err(type_error(
        obj.ctx(),
        "toHaveScreenshot options: mask: each mask entry must be a Locator or a selector string",
      ));
    }
  }
  Ok(Some(out))
}

// ── Matchers as steps ────────────────────────────────────────────────
//
// Playwright's `createMatchers` wraps every matcher, built-in and custom,
// in `callMatcherAsStep` (`matchers/expect.ts`): an `expect` step opens
// when the matcher is called and closes when it settles. The wrapper here
// is that, installed once over each matcher class's prototype; what the
// step is called and where it belongs are `ferridriver_test`'s rules.

/// Where the wrapper is kept, so a custom matcher installed per assertion
/// is wrapped the same way. Hidden, and not starting with `to`.
const MATCHER_STEP: &str = "_matcherStep";

/// `step(name, raw, nests)` returns `raw` run as a step. The raw matcher
/// stays reachable as `_rawMatcher`, for a caller whose matcher call is
/// part of a step already (`expect.poll` re-running it on each value).
const MATCHER_STEPS: &str = r"(begin, end) => {
  const message = error => (error !== null && typeof error === 'object' && 'message' in error)
    ? String(error.message) : String(error);
  return (name, raw, nests) => {
    const stepped = function (...args) {
      const step = begin(this, name, nests, args[0]);
      if (step === undefined) return raw.apply(this, args);
      let result;
      try {
        result = raw.apply(this, args);
      } catch (error) {
        end(step, message(error));
        throw error;
      }
      if (result !== null && typeof result === 'object' && typeof result.then === 'function') {
        return result.then(
          value => { end(step); return value; },
          error => { end(step, message(error)); throw error; });
      }
      end(step);
      return result;
    };
    Object.defineProperty(stepped, 'name', { value: name });
    Object.defineProperty(stepped, '_rawMatcher', { value: raw });
    return stepped;
  };
}";

/// Soft failures recorded before each open soft step, by step id: a soft
/// assertion that fails returns normally, and the step learns it failed
/// from the failure it left behind.
#[derive(Default, rquickjs::JsLifetime)]
struct SoftBaselines(std::cell::RefCell<rustc_hash::FxHashMap<String, usize>>);

pub(super) fn install_matcher_steps<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
  // Once per realm: wrapping a prototype twice would report every
  // assertion twice.
  if let Some(proto) = Class::<ExpectJs>::prototype(ctx)?
    && proto.contains_key(MATCHER_STEP)?
  {
    return Ok(());
  }
  let _ = ctx.store_userdata(SoftBaselines::default());
  let begin = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, this: Value<'js>, name: String, nests: bool, expected: Opt<Value<'js>>| {
      begin_matcher_step(&ctx, &this, name, nests, expected.0)
    },
  )?;
  let end = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, step_id: String, error: Opt<Option<String>>| {
      end_matcher_step(&ctx, &step_id, error.0.flatten());
    },
  )?;
  let factory: Function<'js> = ctx.eval(MATCHER_STEPS)?;
  let step: Function<'js> = factory.call((begin, end))?;
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let own_names: Function<'js> = object_global.get("getOwnPropertyNames")?;
  for (proto, polled) in [
    (Class::<ExpectJs>::prototype(ctx)?, false),
    (Class::<ExpectPollJs>::prototype(ctx)?, true),
  ] {
    let Some(proto) = proto else { continue };
    let names: Vec<String> = own_names.call((proto.clone(),))?;
    for name in names.into_iter().filter(|n| n.starts_with("to")) {
      let raw: Value<'js> = proto.get(name.as_str())?;
      if !raw.is_function() {
        continue;
      }
      // What runs inside `toPass` and `expect.poll` is the test's own code.
      let nests = polled || name == "toPass";
      let stepped: Value<'js> = step.call((name.as_str(), raw, nests))?;
      proto.set(name.as_str(), stepped)?;
    }
    if !polled {
      define_hidden(ctx, &proto, MATCHER_STEP, step.clone().into_value())?;
    }
  }
  Ok(())
}

/// The `step(name, raw, nests)` wrapper [`install_matcher_steps`] built.
pub(super) fn matcher_step_wrapper<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  let proto = Class::<ExpectJs>::prototype(ctx)?
    .ok_or_else(|| type_error(ctx, "expect: the Expect class has no prototype in this context"))?;
  proto.get(MATCHER_STEP)
}

/// Fast-path `JSON.parse(JSON.stringify(value))` for primitives in Rust before
/// falling back to `QuickJS` `JSON.stringify` for objects/arrays/RegExp.
fn quick_expected_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Option<JsonValue> {
  use rquickjs::Type;
  match value.type_of() {
    Type::Undefined | Type::Uninitialized | Type::Function | Type::Symbol => None,
    Type::Null => Some(JsonValue::Null),
    Type::Bool => value.as_bool().map(JsonValue::Bool),
    Type::Int => value.as_int().map(|n| JsonValue::Number(serde_json::Number::from(n))),
    Type::Float => value
      .as_float()
      .map(|n| serde_json::Number::from_f64(n).map_or(JsonValue::Null, JsonValue::Number)),
    Type::String => value
      .as_string()
      .and_then(|s| s.to_string().ok())
      .map(JsonValue::String),
    _ => ctx
      .json_stringify(value)
      .unwrap_or_else(|_| {
        // A cyclic value throws; the assertion itself is not affected.
        let _ = ctx.catch();
        None
      })
      .and_then(|json| json.to_string().ok())
      .and_then(|json| serde_json::from_str(&json).ok()),
  }
}

/// Open the step for a matcher call on `this`. `None` outside a test, so
/// a script's assertions cost one call and report nothing.
fn begin_matcher_step<'js>(
  ctx: &Ctx<'js>,
  this: &Value<'js>,
  matcher: String,
  nests: bool,
  expected: Option<Value<'js>>,
) -> Option<String> {
  let bridge = crate::bindings::test::optional_bridge(ctx)?;
  let expected = expected.and_then(|value| quick_expected_json(ctx, value));
  let mut step = ferridriver_test::expect::ExpectStep {
    matcher,
    expected,
    ..Default::default()
  };
  if let Ok(class) = Class::<ExpectJs>::from_value(this) {
    let assertion = class.borrow();
    step.is_not = assertion.is_not;
    step.is_soft = assertion.is_soft;
    step.message.clone_from(&assertion.message);
    if let SubjectKind::Locator(locator) = &assertion.subject.kind {
      step.selector = Some(locator.selector().to_string());
    }
  } else if let Ok(class) = Class::<ExpectPollJs>::from_value(this) {
    let assertion = class.borrow();
    step.is_poll = true;
    step.is_not = assertion.is_not;
    step.message.clone_from(&assertion.message);
  } else {
    return None;
  }
  let mut spec = step.spec();
  if let Some((_, line, column)) = crate::bindings::call_site::capture_frame(ctx) {
    spec
      .frames
      .push(ferridriver_test::step::StepFrame::Host { line, column });
  }
  let step_id = bridge.open_step_now(spec, nests)?;
  if step.is_soft
    && let Some(baselines) = ctx.userdata::<SoftBaselines>()
  {
    baselines.0.borrow_mut().insert(step_id.clone(), bridge.errors().len());
  }
  Some(step_id)
}

fn end_matcher_step(ctx: &Ctx<'_>, step_id: &str, error: Option<String>) {
  let Some(bridge) = crate::bindings::test::optional_bridge(ctx) else {
    return;
  };
  let baseline = ctx
    .userdata::<SoftBaselines>()
    .and_then(|baselines| baselines.0.borrow_mut().remove(step_id));
  let error = error.or_else(|| {
    let errors = bridge.errors();
    baseline.filter(|before| errors.len() > *before)?;
    errors.last().cloned()
  });
  let status = if error.is_some() {
    ferridriver_test::model::StepStatus::Failed
  } else {
    ferridriver_test::model::StepStatus::Passed
  };
  bridge.close_step_now(
    step_id,
    ferridriver_test::step::StepOutcome {
      status,
      error,
      annotations: Vec::new(),
    },
  );
}
