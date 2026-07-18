//! Ferridriver-side glue over the vendored WHATWG `AbortController` /
//! `AbortSignal` ([`ferridriver_jsstd::abort`]).
//!
//! The JS-visible classes come from the vendored implementation, so a
//! signal produced by `new AbortController()` is the same class
//! `stream.pipeTo(dest, { signal })` accepts and its default reason is a
//! real `DOMException` with `name === "AbortError"`.
//!
//! What lives here is the bridge a `fetch` request future needs: a
//! `Send`/`Sync` [`AbortInner`] channel it can await from outside the JS
//! thread, fed by a native `abort` listener that captures only the
//! `Arc<AbortInner>` — never a JS value, so no untraceable cross-language
//! cycle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferridriver_jsstd::abort::AbortSignal;
use ferridriver_jsstd::events::Emitter;
use ferridriver_jsstd::exceptions::{DOMException, DOMExceptionName};
use rquickjs::function::{Opt, This};
use rquickjs::{Class, Ctx, Function, Value};

/// Native, thread-safe side of a signal: lets a `fetch` request future
/// observe an abort that happens on the JS thread and cancel itself.
pub struct AbortInner {
  aborted: AtomicBool,
  notify: tokio::sync::Notify,
  /// Best-effort message for the native rejection (the JS `.reason`
  /// object stays on the signal instance).
  message: std::sync::Mutex<Option<String>>,
}

impl AbortInner {
  fn new() -> Arc<Self> {
    Arc::new(Self {
      aborted: AtomicBool::new(false),
      notify: tokio::sync::Notify::new(),
      message: std::sync::Mutex::new(None),
    })
  }

  pub fn is_aborted(&self) -> bool {
    self.aborted.load(Ordering::Acquire)
  }

  /// Reason message for the `fetch` rejection ("This operation was
  /// aborted" by default).
  pub fn reason_message(&self) -> String {
    self
      .message
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
      .unwrap_or_else(|| "This operation was aborted".to_string())
  }

  fn mark(&self, message: Option<String>) {
    *self.message.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = message;
    self.aborted.store(true, Ordering::Release);
    self.notify.notify_waiters();
  }

  /// Resolves the next time the signal aborts. (`Notify` only wakes
  /// waiters registered before `notify_waiters`; callers must check
  /// [`Self::is_aborted`] first to avoid the pre-abort race.)
  pub async fn aborted(&self) {
    self.notify.notified().await;
  }
}

fn reason_message(reason: Option<&Value<'_>>) -> Option<String> {
  let r = reason?;
  if let Some(s) = r.as_string().and_then(|s| s.to_string().ok()) {
    return Some(s);
  }
  r.as_object()
    .and_then(|o| o.get::<_, String>("message").ok())
    .or(Some("This operation was aborted".to_string()))
}

/// Attach a native abort channel to `signal`.
///
/// The returned [`AbortInner`] is already marked if the signal has
/// aborted; otherwise a one-shot `abort` listener marks it later. The
/// listener closure captures only the `Arc`, so the signal stays
/// collectable.
pub fn native_channel<'js>(ctx: &Ctx<'js>, signal: &Class<'js, AbortSignal<'js>>) -> rquickjs::Result<Arc<AbortInner>> {
  let inner = AbortInner::new();
  {
    let b = signal.borrow();
    if b.aborted {
      inner.mark(reason_message(b.reason().as_ref()));
      return Ok(inner);
    }
  }
  let sink = inner.clone();
  // `this` is the signal the listener fired on, so the reason is read at
  // call time instead of captured — a captured `Class` would be
  // invisible to the QuickJS GC.
  let cb = Function::new(ctx.clone(), move |this: This<Value<'js>>| {
    let reason = Class::<AbortSignal<'js>>::from_value(&this.0)
      .ok()
      .and_then(|s| s.borrow().reason());
    sink.mark(reason_message(reason.as_ref()));
  })?;
  AbortSignal::add_event_listener_str(signal.clone(), ctx, "abort", cb, false, true)?;
  Ok(inner)
}

/// A fresh, not-yet-aborted signal for native callers (the
/// extension-tool dispatch hands one to every handler as `ctx.signal`).
pub fn fresh_instance<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Class<'js, AbortSignal<'js>>> {
  Class::instance(ctx.clone(), AbortSignal::new())
}

/// Abort `signal` from native code with a `DOMException` reason — the
/// exact effect of `AbortController.abort` (state flip, listener
/// dispatch, `.reason`).
pub fn abort_native<'js>(
  signal: &Class<'js, AbortSignal<'js>>,
  ctx: &Ctx<'js>,
  name: DOMExceptionName,
  message: &str,
) -> rquickjs::Result<()> {
  let reason = Class::instance(
    ctx.clone(),
    DOMException::new_with_name(ctx, name, message.to_string())?,
  )?;
  signal.borrow_mut().set_reason(Opt(Some(reason.into_value())));
  AbortSignal::send_aborted(This(signal.clone()), ctx.clone())
}
