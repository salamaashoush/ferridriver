//! Playwright's `signal` option on every binding method that takes an
//! option bag.
//!
//! Each class prototype's methods are wrapped once: a call whose option
//! bag carries an `AbortSignal` hands it to Rust as an
//! [`ferridriver::abort::AbortSignal`], armed for the
//! [`crate::bindings::CallSite`] that the method's own parameter
//! conversion takes on the same stack, and passes the bag on without it.
//! The `AbortError` the call may reject with gets the signal's reason as
//! its `cause`, which only the JS side can hand back unchanged.

use std::cell::RefCell;

use rquickjs::class::{Class, JsClass};
use rquickjs::function::Func;
use rquickjs::{Ctx, Function, JsLifetime, Object, Value};

const WRAP_METHODS: &str = r"(proto, arm, disarm) => {
  for (const key of Object.getOwnPropertyNames(proto)) {
    if (key === 'constructor') continue;
    const desc = Object.getOwnPropertyDescriptor(proto, key);
    if (!desc || typeof desc.value !== 'function') continue;
    const original = desc.value;
    Object.defineProperty(proto, key, { ...desc, value: function (...args) {
      let signal;
      for (let i = args.length - 1; i >= 0; i--) {
        const arg = args[i];
        if (arg !== null && typeof arg === 'object' && arg.signal instanceof AbortSignal) {
          const { signal: found, ...rest } = arg;
          signal = found;
          args[i] = rest;
          break;
        }
      }
      if (signal === undefined) return original.apply(this, args);
      arm(signal);
      let result;
      try {
        result = original.apply(this, args);
      } finally {
        disarm();
      }
      if (!(result instanceof Promise)) return result;
      return result.catch(error => {
        if (error !== null && typeof error === 'object' && error.name === 'AbortError') error.cause = signal.reason;
        throw error;
      });
    } });
  }
}";

/// The signal armed for the next binding call on this stack.
struct Pending(RefCell<Option<ferridriver::abort::AbortSignal>>);

// SAFETY: holds only `'static` data.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for Pending {
  type Changed<'to> = Pending;
}

/// Take the signal a wrapped method armed for this call.
pub(crate) fn take_pending(ctx: &Ctx<'_>) -> Option<ferridriver::abort::AbortSignal> {
  ctx
    .userdata::<Pending>()
    .and_then(|pending| pending.0.borrow_mut().take())
}

/// What an in-flight call fails with: an `Error` reason's message, any
/// other reason as a string.
fn reason_text(reason: &Value<'_>) -> String {
  if let Some(error) = reason.as_object()
    && let Ok(Some(message)) = error.get::<_, Option<String>>("message")
  {
    return message;
  }
  reason
    .clone()
    .get::<rquickjs::convert::Coerced<String>>()
    .map_or_else(|_| ferridriver::abort::ABORTED.to_string(), |text| text.0)
}

fn arm<'js>(ctx: Ctx<'js>, signal: Object<'js>) -> rquickjs::Result<()> {
  let core = if signal.get::<_, bool>("aborted")? {
    ferridriver::abort::AbortSignal::refused(reason_text(&signal.get("reason")?))
  } else {
    let core = ferridriver::abort::AbortSignal::new();
    let on_abort = core.clone();
    let watched = rquickjs::Persistent::save(&ctx, signal.clone());
    let listener = Function::new(ctx.clone(), move |ctx: Ctx<'_>| -> rquickjs::Result<()> {
      let signal = watched.clone().restore(&ctx)?;
      on_abort.abort(reason_text(&signal.get("reason")?));
      Ok(())
    })?;
    let options = Object::new(ctx.clone())?;
    options.set("once", true)?;
    let add: Function<'js> = signal.get("addEventListener")?;
    add.call::<_, ()>((rquickjs::function::This(signal), "abort", listener, options))?;
    core
  };
  if let Some(pending) = ctx.userdata::<Pending>() {
    *pending.0.borrow_mut() = Some(core);
  }
  Ok(())
}

fn wrap<'js, C: JsClass<'js>>(ctx: &Ctx<'js>, patch: &Function<'js>) -> rquickjs::Result<()> {
  let Some(proto) = Class::<C>::prototype(ctx)? else {
    return Ok(());
  };
  let arm = Func::from(arm);
  let disarm = Func::from(|ctx: Ctx<'_>| {
    take_pending(&ctx);
  });
  patch.call::<_, ()>((proto, arm, disarm))
}

/// Wrap the methods of every class whose calls take an option bag.
pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let _ = ctx.store_userdata(Pending(RefCell::new(None)));
  let patch: Function<'_> = ctx.eval(WRAP_METHODS)?;
  wrap::<crate::bindings::PageJs>(ctx, &patch)?;
  wrap::<crate::bindings::FrameJs>(ctx, &patch)?;
  wrap::<crate::bindings::LocatorJs>(ctx, &patch)?;
  wrap::<crate::bindings::BrowserContextJs>(ctx, &patch)?;
  wrap::<crate::bindings::BrowserJs>(ctx, &patch)?;
  wrap::<crate::bindings::KeyboardJs>(ctx, &patch)?;
  wrap::<crate::bindings::MouseJs>(ctx, &patch)?;
  wrap::<crate::bindings::page::TouchscreenJs>(ctx, &patch)?;
  wrap::<crate::bindings::JSHandleJs>(ctx, &patch)?;
  wrap::<crate::bindings::ElementHandleJs>(ctx, &patch)?;
  wrap::<crate::bindings::RouteJs>(ctx, &patch)?;
  wrap::<crate::bindings::WebSocketJs>(ctx, &patch)?;
  wrap::<crate::bindings::DownloadJs>(ctx, &patch)?;
  wrap::<crate::bindings::expect::ExpectJs>(ctx, &patch)?;
  Ok(())
}
