//! `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` /
//! `setImmediate` — native, `ctx.spawn`-backed (the timer future lives
//! on the session VM's executor, so callbacks fire between executes and
//! while a script is parked on a host await; dropping the
//! `AsyncRuntime` aborts every armed timer).
//!
//! The timer handle is a [`Timeout`] class instance (not a numeric id):
//! it survives REPL-style across `execute` calls via `globalThis` and
//! `clearTimeout(handle)` cancels through its `Notify`. Holding the JS
//! callback inside the spawned future is the sanctioned
//! executor-owned-future shape (same as `AbortSignal.timeout`) — the
//! future is dropped with the runtime, never stored in a traced JS
//! field.

use std::sync::Arc;
use std::time::Duration;

use rquickjs::function::{Func, Rest};
use rquickjs::{Class, Ctx, Function, JsLifetime, Value, class::Trace};
use tokio::sync::Notify;

/// Opaque timer handle returned by `setTimeout` / `setInterval`.
#[derive(Trace, JsLifetime)]
#[rquickjs::class]
pub struct Timeout {
  #[qjs(skip_trace)]
  abort: Arc<Notify>,
}

/// `clearTimeout(handle?)` / `clearInterval(handle?)`. Node ignores
/// `undefined`, `null`, numbers, foreign objects — anything that is not
/// a live timer handle — so the argument is taken as a raw `Value` and
/// only acted on when it is actually a [`Timeout`].
fn clear_timeout(value: Rest<Value<'_>>) {
  if let Some(v) = value.0.first() {
    if let Ok(timeout) = Class::<Timeout>::from_value(v) {
      timeout.borrow().abort.notify_one();
    }
  }
}

fn set_timeout_interval<'js>(
  ctx: Ctx<'js>,
  cb: Function<'js>,
  msec: Option<f64>,
  args: Vec<Value<'js>>,
  is_interval: bool,
) -> rquickjs::Result<Class<'js, Timeout>> {
  // 4ms floor, matching the HTML spec's nested-timeout clamp (and the
  // prior rquickjs-extra-timers behaviour). Node clamps NaN/negative
  // and >2^31-1 delays to 1ms — treat all of those as the floor.
  let msecs = match msec {
    Some(ms) if ms.is_finite() && ms >= 0.0 && ms < f64::from(i32::MAX) => ms as u64,
    _ => 0,
  };
  let duration = Duration::from_millis(msecs.max(4));

  let abort = Arc::new(Notify::new());
  let abort_ref = abort.clone();

  ctx.spawn(async move {
    loop {
      let mut interval = tokio::time::interval(duration);
      interval.tick().await; // Skip the immediate first tick.
      let aborted = tokio::select! {
        () = abort_ref.notified() => true,
        _ = interval.tick() => false,
      };
      if aborted {
        break;
      }
      // Node passes `setTimeout(cb, ms, ...args)` extras through to
      // every invocation.
      let mut call_args = rquickjs::function::Args::new(cb.ctx().clone(), args.len());
      let ok = call_args.push_args(args.iter().cloned()).is_ok();
      if !ok || {
        let res: rquickjs::Result<()> = cb.call_arg(call_args);
        res
          .inspect_err(|err| tracing::warn!(target: "ferridriver::script", "timer callback threw: {err}"))
          .is_err()
      } {
        break;
      }
      if !is_interval {
        break;
      }
    }
  });

  Class::instance(ctx, Timeout { abort })
}

fn set_timeout<'js>(ctx: Ctx<'js>, cb: Function<'js>, rest: Rest<Value<'js>>) -> rquickjs::Result<Class<'js, Timeout>> {
  let (msec, args) = split_delay_args(rest.0);
  set_timeout_interval(ctx, cb, msec, args, false)
}

fn set_interval<'js>(
  ctx: Ctx<'js>,
  cb: Function<'js>,
  rest: Rest<Value<'js>>,
) -> rquickjs::Result<Class<'js, Timeout>> {
  let (msec, args) = split_delay_args(rest.0);
  set_timeout_interval(ctx, cb, msec, args, true)
}

/// Split `(delay?, ...args)` off the rest parameters, coercing the
/// delay to a number the way JS timers do (`undefined`/non-numeric ⇒ 0).
fn split_delay_args(mut rest: Vec<Value<'_>>) -> (Option<f64>, Vec<Value<'_>>) {
  if rest.is_empty() {
    return (None, rest);
  }
  let delay = rest.remove(0);
  (delay.as_number(), rest)
}

/// `setImmediate(cb, ...args)` — deferred to the microtask-adjacent
/// job queue, args passed through like Node.
fn set_immediate<'js>(cb: Function<'js>, rest: Rest<Value<'js>>) -> rquickjs::Result<()> {
  let mut args = rquickjs::function::Args::new(cb.ctx().clone(), rest.0.len());
  args.push_args(rest.0)?;
  cb.defer_arg(args)
}

pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();
  globals.set("setTimeout", Func::from(set_timeout))?;
  globals.set("clearTimeout", Func::from(clear_timeout))?;
  globals.set("setInterval", Func::from(set_interval))?;
  globals.set("clearInterval", Func::from(clear_timeout))?;
  globals.set("setImmediate", Func::from(set_immediate))?;
  Ok(())
}
