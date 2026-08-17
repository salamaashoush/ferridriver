//! `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` /
//! `setImmediate` / `queueMicrotask` — native, `ctx.spawn`-backed (the
//! timer future lives on the host's VM executor, so callbacks fire
//! between executes and while a script is parked on a host await;
//! dropping the runtime aborts every armed timer).
//!
//! The timer handle is a [`Timeout`] class instance (not a numeric id):
//! it survives REPL-style across evaluations via `globalThis` and
//! `clearTimeout(handle)` cancels through its `Notify`. Holding the JS
//! callback inside the spawned future is the sanctioned
//! executor-owned-future shape (same as `AbortSignal.timeout`) — the
//! future is dropped with the runtime, never stored in a traced JS field.
//!
//! A host with ambient per-callback state (a capability grant, a request
//! scope) supplies it as a [`CallbackPolicy`]: it is captured when the
//! timer is armed and re-entered when the callback fires, so a callback
//! registered under a restriction keeps it instead of falling back to
//! whatever the resting state happens to be. Hosts without such state
//! install [`NoPolicy`].

use std::sync::Arc;
use std::time::Duration;

use rquickjs::function::{Func, Rest};
use rquickjs::{Class, Ctx, Function, JsLifetime, Value, class::Trace};
use tokio::sync::Notify;

/// Ambient host state that a scheduled callback must run under.
pub trait CallbackPolicy: Clone + 'static {
  /// The state in force right now, if any.
  fn capture(ctx: &Ctx<'_>) -> Option<Self>
  where
    Self: Sized;

  /// Run `f` with `policy` in force, restoring the caller's state after.
  fn enter<R>(ctx: &Ctx<'_>, policy: Option<&Self>, f: impl FnOnce() -> R) -> R
  where
    Self: Sized;
}

/// For hosts with no ambient callback state: callbacks run as they are.
#[derive(Clone, Copy)]
pub struct NoPolicy;

impl CallbackPolicy for NoPolicy {
  fn capture(_ctx: &Ctx<'_>) -> Option<Self> {
    None
  }

  fn enter<R>(_ctx: &Ctx<'_>, _policy: Option<&Self>, f: impl FnOnce() -> R) -> R {
    f()
  }
}

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

fn set_timeout_interval<'js, P: CallbackPolicy>(
  ctx: Ctx<'js>,
  cb: Function<'js>,
  msec: Option<f64>,
  args: Vec<Value<'js>>,
  is_interval: bool,
) -> rquickjs::Result<Class<'js, Timeout>> {
  // 4ms floor, matching the HTML spec's nested-timeout clamp. Node
  // clamps NaN/negative and >2^31-1 delays to 1ms — treat all of those
  // as the floor.
  let msecs = match msec {
    Some(ms) if ms.is_finite() && ms >= 0.0 && ms < f64::from(i32::MAX) => ms as u64,
    _ => 0,
  };
  let duration = Duration::from_millis(msecs.max(4));

  let abort = Arc::new(Notify::new());
  let abort_ref = abort.clone();
  let policy = P::capture(&ctx);

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
        let res: rquickjs::Result<()> = P::enter(cb.ctx(), policy.as_ref(), || cb.call_arg(call_args));
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

fn set_timeout<'js, P: CallbackPolicy>(
  ctx: Ctx<'js>,
  cb: Function<'js>,
  rest: Rest<Value<'js>>,
) -> rquickjs::Result<Class<'js, Timeout>> {
  let (msec, args) = split_delay_args(rest.0);
  set_timeout_interval::<P>(ctx, cb, msec, args, false)
}

fn set_interval<'js, P: CallbackPolicy>(
  ctx: Ctx<'js>,
  cb: Function<'js>,
  rest: Rest<Value<'js>>,
) -> rquickjs::Result<Class<'js, Timeout>> {
  let (msec, args) = split_delay_args(rest.0);
  set_timeout_interval::<P>(ctx, cb, msec, args, true)
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

/// `setImmediate(cb, ...args)` — deferred to the microtask-adjacent job
/// queue, args passed through like Node. With a captured policy the
/// callback is wrapped in a native bracket so the deferred job runs
/// under it (same rule as `setTimeout`).
fn set_immediate<'js, P: CallbackPolicy>(
  ctx: Ctx<'js>,
  cb: Function<'js>,
  rest: Rest<Value<'js>>,
) -> rquickjs::Result<()> {
  match P::capture(&ctx) {
    None => {
      let mut args = rquickjs::function::Args::new(ctx, rest.0.len());
      args.push_args(rest.0)?;
      cb.defer_arg(args)
    },
    Some(policy) => {
      // The wrapper captures only the policy (plain data); the real
      // callback rides the deferred args (a native closure must never
      // capture a JS value or a `Persistent` — untraceable GC cycle at
      // teardown). A `Rest`-only signature keeps every JS value on one
      // `'js`.
      let policy = Some(policy);
      let wrapper = Function::new(ctx.clone(), move |args: Rest<Value<'_>>| {
        deferred_call::<P>(policy.as_ref(), &args.0)
      })?;
      let mut args = rquickjs::function::Args::new(ctx, rest.0.len() + 1);
      args.push_arg(cb)?;
      args.push_args(rest.0)?;
      wrapper.defer_arg(args)
    },
  }
}

/// Call the deferred callback (args[0]) with the rest of the args, under
/// `policy`.
fn deferred_call<P: CallbackPolicy>(policy: Option<&P>, args: &[Value<'_>]) -> rquickjs::Result<()> {
  let inner = args.first().and_then(|v| v.as_function().cloned()).ok_or_else(|| {
    rquickjs::Error::new_from_js_message("setImmediate", "Error", "deferred callback missing".to_string())
  })?;
  let ctx = inner.ctx().clone();
  let mut call_args = rquickjs::function::Args::new(ctx.clone(), args.len().saturating_sub(1));
  call_args.push_args(args.iter().skip(1).cloned())?;
  P::enter(&ctx, policy, || inner.call_arg(call_args))
}

/// WHATWG `queueMicrotask(cb)`. A named generic fn so `Ctx`, the
/// callback, and the wrapper share one `'js` (an inline closure would
/// give each its own lifetime).
fn queue_microtask<'js, P: CallbackPolicy>(ctx: Ctx<'js>, cb: Function<'js>) -> rquickjs::Result<()> {
  match P::capture(&ctx) {
    None => cb.defer::<()>(()),
    Some(policy) => {
      let policy = Some(policy);
      let wrapper = Function::new(ctx.clone(), move |args: Rest<Value<'_>>| {
        deferred_call::<P>(policy.as_ref(), &args.0)
      })?;
      wrapper.defer((cb,))
    },
  }
}

/// Install the timer globals, carrying `P` from registration to callback.
///
/// # Errors
///
/// Propagates the global writes.
pub fn install<P: CallbackPolicy>(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();
  globals.set("setTimeout", Func::from(set_timeout::<P>))?;
  globals.set("clearTimeout", Func::from(clear_timeout))?;
  globals.set("setInterval", Func::from(set_interval::<P>))?;
  globals.set("clearInterval", Func::from(clear_timeout))?;
  globals.set("setImmediate", Func::from(set_immediate::<P>))?;
  // The job queue drains outside whatever bracket the registrar ran in,
  // so a microtask it queued must carry the policy with it (same rule as
  // `setTimeout` / `setImmediate`).
  globals.set("queueMicrotask", Func::from(queue_microtask::<P>))?;
  Ok(())
}
