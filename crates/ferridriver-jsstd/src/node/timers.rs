//! `node:timers` and `node:timers/promises`.
//!
//! The callback forms are the runtime's own timer globals, re-exported —
//! there is one scheduler, and it is the host's. The promise forms are
//! built on those same globals.

use rquickjs::function::{Func, Opt};
use rquickjs::{Ctx, Function, Object, Promise, Result, Value};

pub const TIMERS_MEMBERS: &[&str] = &[
  "clearImmediate",
  "clearInterval",
  "clearTimeout",
  "setImmediate",
  "setInterval",
  "setTimeout",
];

pub const TIMERS_PROMISES_MEMBERS: &[&str] = &["setImmediate", "setTimeout"];

/// Re-export of the host's timer globals.
///
/// # Errors
///
/// Propagates the global reads and property writes.
pub fn timers_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  let timers = Object::new(ctx.clone())?;
  for name in TIMERS_MEMBERS {
    if let Ok(value) = ctx.globals().get::<_, Value<'js>>(*name) {
      if !value.is_undefined() {
        timers.set(*name, value)?;
      }
    }
  }
  Ok(timers)
}

/// `timers/promises`' `setTimeout(delay, value)`: resolve after the host
/// timer fires, with the value the caller passed.
fn timeout_promise<'js>(ctx: Ctx<'js>, delay: Opt<f64>, value: Opt<Value<'js>>) -> Result<Promise<'js>> {
  let (promise, resolve, _reject) = ctx.promise()?;
  let set_timeout: Function<'js> = ctx.globals().get("setTimeout")?;
  let carried = value.0.unwrap_or_else(|| Value::new_undefined(ctx.clone()));
  // The host's `setTimeout` forwards trailing arguments to the callback,
  // as Node's does, which is what carries the resolution value.
  set_timeout.call::<_, Value<'js>>((resolve, delay.0.unwrap_or(0.0), carried))?;
  Ok(promise)
}

/// `timers/promises`' `setImmediate(value)`.
fn immediate_promise<'js>(ctx: Ctx<'js>, value: Opt<Value<'js>>) -> Result<Promise<'js>> {
  let (promise, resolve, _reject) = ctx.promise()?;
  let set_immediate: Function<'js> = ctx.globals().get("setImmediate")?;
  let carried = value.0.unwrap_or_else(|| Value::new_undefined(ctx.clone()));
  set_immediate.call::<_, Value<'js>>((resolve, carried))?;
  Ok(promise)
}

/// The promise-returning timer surface.
///
/// # Errors
///
/// Propagates the property writes.
pub fn timers_promises_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  let timers = Object::new(ctx.clone())?;
  timers.set("setTimeout", Func::from(timeout_promise))?;
  timers.set("setImmediate", Func::from(immediate_promise))?;
  Ok(timers)
}
