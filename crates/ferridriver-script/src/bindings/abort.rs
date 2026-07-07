//! WHATWG `AbortController` / `AbortSignal` (spec subset, no external
//! deps). Enough of the standard for `fetch(..., { signal })`,
//! `AbortSignal.timeout(ms)`, `AbortSignal.any([...])`, `onabort`,
//! `addEventListener('abort', ...)`, `.aborted`, `.reason`,
//! `.throwIfAborted()`.
//!
//! JS-visible reason/listeners are stored natively on a `'js`-generic
//! class (no synthesized `__` properties). A separate `Send`/`Sync`
//! [`AbortInner`] channel lets `fetch` await an abort from the request
//! future and drop it (the spec's "abort the fetch").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, Function, Object, Value};

/// Registry keeping each `AbortSignal.any` combined signal reachable for
/// its source listeners WITHOUT capturing the live `Class` in the native
/// listener closure. A captured JS value is invisible to QuickJS's GC
/// (the closure is stored in another signal's traced `listeners` field),
/// which is the untraceable cross-language cycle that aborts
/// `JS_FreeRuntime` at teardown. Listeners capture only the `u64` key
/// and restore the class from here at fire time — the same
/// `Persistent`-in-userdata discipline as `PageCallbacks`.
struct AbortAnyUd(
  std::cell::RefCell<rustc_hash::FxHashMap<u64, rquickjs::Persistent<Class<'static, AbortSignalJs<'static>>>>>,
);

// SAFETY: holds only `Persistent` values (already `'static`-projected),
// so re-stating the unused `'js` lifetime is sound — identical rationale
// to `PageCallbacksUd`.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for AbortAnyUd {
  type Changed<'to> = AbortAnyUd;
}

static NEXT_ANY_ID: AtomicU64 = AtomicU64::new(1);

fn with_any_registry<R>(
  ctx: &Ctx<'_>,
  f: impl FnOnce(&mut rustc_hash::FxHashMap<u64, rquickjs::Persistent<Class<'static, AbortSignalJs<'static>>>>) -> R,
) -> Option<R> {
  let ud = ctx.userdata::<AbortAnyUd>()?;
  Some(f(&mut ud.0.borrow_mut()))
}

/// Native, thread-safe side of a signal: lets a `fetch` request future
/// observe an abort that happens on the JS thread and cancel itself.
pub struct AbortInner {
  aborted: AtomicBool,
  notify: tokio::sync::Notify,
  /// Best-effort message for the native rejection (the JS `.reason`
  /// object stays on the class instance).
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

#[derive(Trace)]
#[rquickjs::class(rename = "AbortSignal")]
pub struct AbortSignalJs<'js> {
  #[qjs(skip_trace)]
  inner: Arc<AbortInner>,
  #[qjs(skip_trace)]
  aborted: bool,
  reason: Option<Value<'js>>,
  listeners: Vec<Function<'js>>,
  onabort: Option<Function<'js>>,
}

#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for AbortSignalJs<'js> {
  type Changed<'to> = AbortSignalJs<'to>;
}

impl<'js> AbortSignalJs<'js> {
  fn fresh() -> Self {
    Self {
      inner: AbortInner::new(),
      aborted: false,
      reason: None,
      listeners: Vec::new(),
      onabort: None,
    }
  }

  /// The native channel a `fetch` future awaits on.
  pub fn inner_of(signal: &Class<'js, AbortSignalJs<'js>>) -> Arc<AbortInner> {
    signal.borrow().inner.clone()
  }

  /// Default abort reason: a duck-typed `{ name, message }` (no
  /// `DOMException` class in this runtime). `name` is what abort-aware
  /// libraries check.
  fn default_reason(ctx: &Ctx<'js>, name: &str, message: &str) -> rquickjs::Result<Value<'js>> {
    let o = Object::new(ctx.clone())?;
    o.set("name", name)?;
    o.set("message", message)?;
    Ok(o.into_value())
  }

  fn reason_to_message(reason: Option<&Value<'js>>) -> Option<String> {
    let r = reason?;
    if let Some(s) = r.as_string().and_then(|s| s.to_string().ok()) {
      return Some(s);
    }
    r.as_object()
      .and_then(|o| o.get::<_, String>("message").ok())
      .or(Some("This operation was aborted".to_string()))
  }

  /// Flip to aborted, store the reason, wake the native channel, fire
  /// `onabort` then every `addEventListener('abort')` listener once.
  fn run_abort(this: &Class<'js, AbortSignalJs<'js>>, reason: Value<'js>) {
    {
      let mut b = this.borrow_mut();
      if b.aborted {
        return;
      }
      b.aborted = true;
      b.reason = Some(reason.clone());
      b.inner.mark(Self::reason_to_message(Some(&reason)));
    }
    let (onabort, listeners) = {
      let b = this.borrow();
      (b.onabort.clone(), b.listeners.clone())
    };
    if let Some(cb) = onabort {
      let _ = cb.call::<_, ()>((reason.clone(),));
    }
    for cb in listeners {
      let _ = cb.call::<_, ()>((reason.clone(),));
    }
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl<'js> AbortSignalJs<'js> {
  /// Spec: `AbortSignal` is not constructible (`new AbortSignal()`
  /// throws). It exists only so the name/statics/`instanceof` are
  /// present; instances come from `AbortController`, `AbortSignal.abort`,
  /// `.timeout`, `.any`.
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'js>) -> rquickjs::Result<Self> {
    Err(rquickjs::Exception::throw_type(&ctx, "Illegal constructor"))
  }

  #[qjs(get)]
  pub fn aborted(&self) -> bool {
    self.aborted
  }

  #[qjs(get)]
  pub fn reason(&self) -> Option<Value<'js>> {
    self.reason.clone()
  }

  #[qjs(rename = "throwIfAborted")]
  pub fn throw_if_aborted(&self, ctx: Ctx<'js>) -> rquickjs::Result<()> {
    if self.aborted {
      let r = self.reason.clone().unwrap_or_else(|| Value::new_undefined(ctx.clone()));
      return Err(ctx.throw(r));
    }
    Ok(())
  }

  #[qjs(get, rename = "onabort")]
  pub fn get_onabort(&self) -> Option<Function<'js>> {
    self.onabort.clone()
  }

  #[qjs(set, rename = "onabort")]
  pub fn set_onabort(&mut self, cb: Opt<Function<'js>>) {
    self.onabort = cb.0;
  }

  #[qjs(rename = "addEventListener")]
  pub fn add_event_listener(&mut self, event: String, cb: Function<'js>) {
    if event == "abort" {
      self.listeners.push(cb);
    }
  }

  #[qjs(rename = "removeEventListener")]
  pub fn remove_event_listener(&mut self, event: String, cb: Function<'js>) {
    if event == "abort" {
      self.listeners.retain(|l| l != &cb);
    }
  }

  /// `AbortSignal.abort(reason?)` — an already-aborted signal.
  #[qjs(static)]
  pub fn abort(ctx: Ctx<'js>, reason: Opt<Value<'js>>) -> rquickjs::Result<Class<'js, AbortSignalJs<'js>>> {
    let inst = Class::instance(ctx.clone(), Self::fresh())?;
    let r = match reason.0 {
      Some(v) if !v.is_undefined() => v,
      _ => Self::default_reason(&ctx, "AbortError", "This operation was aborted")?,
    };
    Self::run_abort(&inst, r);
    Ok(inst)
  }

  /// `AbortSignal.timeout(ms)` — aborts with a `TimeoutError` after the
  /// delay, driven on the JS event loop (`Ctx::spawn`).
  #[qjs(static)]
  pub fn timeout(ctx: Ctx<'js>, ms: u64) -> rquickjs::Result<Class<'js, AbortSignalJs<'js>>> {
    let inst = Class::instance(ctx.clone(), Self::fresh())?;
    let inst2 = inst.clone();
    let ctx2 = ctx.clone();
    ctx.spawn(async move {
      tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
      if let Ok(reason) = AbortSignalJs::default_reason(&ctx2, "TimeoutError", "The operation timed out") {
        AbortSignalJs::run_abort(&inst2, reason);
      }
    });
    Ok(inst)
  }

  /// `AbortSignal.any([...])` — aborts when any input signal aborts
  /// (or immediately if one already has).
  #[qjs(static)]
  pub fn any(
    ctx: Ctx<'js>,
    signals: Vec<Class<'js, AbortSignalJs<'js>>>,
  ) -> rquickjs::Result<Class<'js, AbortSignalJs<'js>>> {
    let combined = Class::instance(ctx.clone(), Self::fresh())?;
    for s in &signals {
      let (is_aborted, reason) = {
        let b = s.borrow();
        (b.aborted, b.reason.clone())
      };
      if is_aborted {
        let r = reason.unwrap_or_else(|| {
          Self::default_reason(&ctx, "AbortError", "This operation was aborted")
            .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
        });
        Self::run_abort(&combined, r);
        return Ok(combined);
      }
    }
    if ctx.userdata::<AbortAnyUd>().is_none() {
      let _ = ctx.store_userdata(AbortAnyUd(std::cell::RefCell::new(rustc_hash::FxHashMap::default())));
    }
    let key = NEXT_ANY_ID.fetch_add(1, Ordering::Relaxed);
    let saved = rquickjs::Persistent::save(&ctx, combined.clone());
    with_any_registry(&ctx, |r| {
      r.insert(key, saved);
    });
    for s in &signals {
      // The first source to abort removes the entry (a combined signal
      // aborts at most once); the other sources' listeners then no-op.
      // If no source ever aborts, the entry lives until context
      // teardown — bounded, and GC-safe unlike a captured Class.
      let cb = Function::new(ctx.clone(), move |ctx: Ctx<'js>, reason: Value<'js>| {
        let Some(Some(saved)) = with_any_registry(&ctx, |r| r.remove(&key)) else {
          return;
        };
        if let Ok(combined) = saved.restore(&ctx) {
          AbortSignalJs::run_abort(&combined, reason);
        }
      })?;
      s.borrow_mut().listeners.push(cb);
    }
    Ok(combined)
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "AbortController")]
pub struct AbortControllerJs<'js> {
  signal: Class<'js, AbortSignalJs<'js>>,
}

#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for AbortControllerJs<'js> {
  type Changed<'to> = AbortControllerJs<'to>;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl<'js> AbortControllerJs<'js> {
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'js>) -> rquickjs::Result<Self> {
    Ok(Self {
      signal: Class::instance(ctx, AbortSignalJs::fresh())?,
    })
  }

  #[qjs(get)]
  pub fn signal(&self) -> Class<'js, AbortSignalJs<'js>> {
    self.signal.clone()
  }

  #[qjs(rename = "abort")]
  pub fn abort(&self, ctx: Ctx<'js>, reason: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let r = match reason.0 {
      Some(v) if !v.is_undefined() => v,
      _ => AbortSignalJs::default_reason(&ctx, "AbortError", "This operation was aborted")?,
    };
    AbortSignalJs::run_abort(&self.signal, r);
    Ok(())
  }
}
