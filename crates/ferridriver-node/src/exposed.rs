//! The JS callback behind `exposeFunction` / `exposeBinding`, as the
//! [`ferridriver::ExposedBinding`] core dispatches page-side calls to.

use std::sync::Arc;

use napi::Env;
use napi::bindgen_prelude::{FnArgs, Function, Promise, Unknown};

use crate::context::BindingSourceJs;

/// Playwright spreads the page-side arguments into the callback, passes the
/// binding source first for `exposeBinding` only, and awaits whatever the
/// callback returns. A thread-safe function has a fixed arity and cannot
/// await, so this adapter does all three on the JS side, and the promise it
/// always returns carries a synchronous throw as a rejection. The result
/// crosses as JSON, which has no `undefined`.
const ADAPTER: &str = "(f, withSource) => {
  const json = v => v === undefined ? null : v;
  return withSource
    ? (source, args) => Promise.resolve().then(() => f(source, ...args)).then(json)
    : (source, args) => Promise.resolve().then(() => f(...args)).then(json);
}";

type Adapted<'a> = Function<'a, FnArgs<(BindingSourceJs, serde_json::Value)>, Promise<serde_json::Value>>;

pub(crate) fn binding(env: Env, callback: Unknown<'_>, with_source: bool) -> napi::Result<ferridriver::ExposedBinding> {
  let make_adapted: Function<'_, FnArgs<(Unknown<'_>, bool)>, Adapted<'_>> = env.run_script(ADAPTER)?;
  let adapted = make_adapted.call((callback, with_source).into())?;
  let tsfn = Arc::new(
    adapted
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?,
  );
  Ok(Arc::new(move |source, args| {
    let tsfn = Arc::clone(&tsfn);
    Box::pin(async move {
      let call = FnArgs::from((
        BindingSourceJs {
          context: source.context,
          page: source.page,
          frame: source.frame,
        },
        serde_json::Value::Array(args),
      ));
      let returned = tsfn.call_async(call).await.map_err(|e| e.reason.clone())?;
      returned.await.map_err(|e| e.reason.clone())
    })
  }))
}
