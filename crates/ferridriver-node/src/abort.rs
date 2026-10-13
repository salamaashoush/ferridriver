//! Playwright's `signal` option on every method that takes an option bag.
//!
//! The JS `AbortSignal` cannot cross into the tokio task a method runs
//! on, so each class prototype's methods are wrapped once at module
//! load: a call whose option bag carries a signal swaps it for a native
//! handle over an [`ferridriver::abort::AbortSignal`], fires the handle
//! when the JS signal does, and drops its listener when the call
//! settles. The option bag's `signal` field reads the handle back, and
//! the method scopes its core call with it. The `AbortError` the call
//! may reject with gets the signal's reason as its `cause`, which only
//! the JS side can hand back unchanged.

use std::future::Future;

use ferridriver::abort::AbortSignal;
use napi::bindgen_prelude::*;

/// The `signal` field of an option bag: the handle the wrapped method
/// put in place of the JS `AbortSignal`.
#[derive(Debug, Clone)]
pub struct Signal(AbortSignal);

impl TypeName for Signal {
  fn type_name() -> &'static str {
    "AbortSignal"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::External
  }
}

impl ValidateNapiValue for Signal {}

impl FromNapiValue for Signal {
  unsafe fn from_napi_value(env: sys::napi_env, value: sys::napi_value) -> Result<Self> {
    let handle = unsafe { External::<AbortSignal>::from_napi_ref(env, value) }
      .map_err(|_| Error::from_reason("signal: expected an AbortSignal"))?;
    Ok(Self((**handle).clone()))
  }
}

/// An option bag that carries `signal`.
pub trait Signalled {
  fn take_signal(&mut self) -> Option<AbortSignal>;
}

impl Signalled for Option<Signal> {
  fn take_signal(&mut self) -> Option<AbortSignal> {
    self.take().map(|signal| signal.0)
  }
}

impl<B: Signalled> Signalled for Option<B> {
  fn take_signal(&mut self) -> Option<AbortSignal> {
    self.as_mut().and_then(Signalled::take_signal)
  }
}

macro_rules! signalled {
  ($($bag:ty),* $(,)?) => {
    $(impl $crate::abort::Signalled for $bag {
      fn take_signal(&mut self) -> Option<ferridriver::abort::AbortSignal> {
        $crate::abort::Signalled::take_signal(&mut self.signal)
      }
    })*
  };
}
pub(crate) use signalled;

/// Take the signal out of an option bag whose other fields are lowered
/// by hand.
pub fn take<B: Signalled>(options: &mut Option<B>) -> Option<AbortSignal> {
  options.take_signal()
}

/// Lowers a JS option bag onto a core action, its `signal` included.
pub trait Bag<O>: Sized {
  fn bag<N>(self, options: Option<N>) -> Result<Self>
  where
    N: Signalled + TryInto<O>,
    N::Error: Into<Error>;
}

impl<O, T> Bag<O> for ferridriver::action::Action<'_, O, T> {
  fn bag<N>(self, options: Option<N>) -> Result<Self>
  where
    N: Signalled + TryInto<O>,
    N::Error: Into<Error>,
  {
    let Some(mut options) = options else {
      return Ok(self);
    };
    let signal = options.take_signal();
    Ok(
      self
        .options(options.try_into().map_err(Into::into)?)
        .maybe_signal(signal),
    )
  }
}

/// Run a core call that is not an action under the bag's signal.
pub fn run<'a, T: 'a>(
  signal: Option<AbortSignal>,
  call: impl Future<Output = ferridriver::error::Result<T>> + Send + 'a,
) -> impl Future<Output = Result<T>> + Send + 'a {
  let call = Box::pin(call);
  async move {
    ferridriver::abort::scope(signal, ferridriver::abort::guard(call))
      .await
      .map_err(crate::error::to_napi)
  }
}

/// Run a wait the binding drives itself under the bag's signal.
pub async fn race<T>(signal: Option<AbortSignal>, wait: impl Future<Output = Result<T>>) -> Result<T> {
  let Some(signal) = signal else {
    return wait.await;
  };
  if signal.was_refused() {
    return Err(crate::error::to_napi(signal.error()));
  }
  tokio::select! {
    biased;
    result = wait => result,
    () = signal.aborted() => Err(crate::error::to_napi(signal.error())),
  }
}

/// The module-load hook that wraps the classes. napi registers it outside
/// test builds only.
#[cfg(not(test))]
mod load {
  use ferridriver::abort::AbortSignal;
  use napi::bindgen_prelude::*;
  use napi_derive::napi;

  type Arm<'env> = Function<'env, FnArgs<(bool, String)>, External<AbortSignal>>;
  type Fire<'env> = Function<'env, FnArgs<(Unknown<'env>, String)>, ()>;

  const WRAP_CLASSES: &str = r"((arm, fire, exports) => {
  const text = reason =>
    reason !== null && typeof reason === 'object' && typeof reason.message === 'string' ? reason.message : String(reason);
  const wrap = proto => {
    for (const key of Object.getOwnPropertyNames(proto)) {
      if (key === 'constructor') continue;
      const desc = Object.getOwnPropertyDescriptor(proto, key);
      if (!desc || typeof desc.value !== 'function') continue;
      const original = desc.value;
      Object.defineProperty(proto, key, { ...desc, value: function (...args) {
        let at = -1;
        for (let i = args.length - 1; i >= 0; i--) {
          const arg = args[i];
          if (arg !== null && typeof arg === 'object' && arg.signal instanceof AbortSignal) {
            at = i;
            break;
          }
        }
        if (at < 0) return original.apply(this, args);
        const signal = args[at].signal;
        const handle = arm(signal.aborted, signal.aborted ? text(signal.reason) : '');
        args[at] = { ...args[at], signal: handle };
        const result = original.apply(this, args);
        if (!(result instanceof Promise)) return result;
        const onAbort = () => fire(handle, text(signal.reason));
        signal.addEventListener('abort', onAbort, { once: true });
        return result.then(
          value => {
            signal.removeEventListener('abort', onAbort);
            return value;
          },
          error => {
            signal.removeEventListener('abort', onAbort);
            if (error instanceof Error && error.message.startsWith('AbortError: ')) error.cause = signal.reason;
            throw error;
          },
        );
      } });
    }
  };
  for (const value of Object.values(exports)) {
    if (typeof value === 'function' && value.prototype) wrap(value.prototype);
  }
})";

  #[napi(module_exports)]
  pub fn wrap_classes(env: Env, exports: Object<'_>) -> Result<()> {
    let arm = env.create_function_from_closure::<FnArgs<(bool, String)>, External<AbortSignal>, _>("arm", |ctx| {
      let reason: String = ctx.get(1)?;
      Ok(External::new(if ctx.get::<bool>(0)? {
        AbortSignal::refused(reason)
      } else {
        AbortSignal::new()
      }))
    })?;
    let fire = env.create_function_from_closure::<FnArgs<(Unknown<'_>, String)>, (), _>("fire", |ctx| {
      let handle: &External<AbortSignal> = ctx.get(0)?;
      handle.abort(ctx.get::<String>(1)?);
      Ok(())
    })?;
    let wrap: Function<'_, FnArgs<(Arm<'_>, Fire<'_>, Object<'_>)>, ()> = env.run_script(WRAP_CLASSES)?;
    wrap.call((arm, fire, exports).into())
  }
}
