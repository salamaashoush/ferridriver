//! `WebErrorJs` — QuickJS binding for
//! [`ferridriver::web_error::WebError`].
//!
//! Mirrors Playwright's client-side `WebError` class from
//! `/tmp/playwright/packages/playwright-core/src/client/webError.ts`
//! and the public-type contract in
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:21658` —
//! `error(): Error` returning a **native JS `Error`** instance (not a
//! plain object) so `instanceof Error` holds in script-land. `page()`
//! is omitted (symmetric with `DownloadJs` / `FileChooserJs` /
//! `ConsoleMessageJs`); script-land callers have no need for the
//! page back-reference.

use ferridriver::web_error::{ErrorDetails, WebError as CoreWebError};
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebError")]
pub struct WebErrorJs {
  #[qjs(skip_trace)]
  inner: CoreWebError,
}

impl WebErrorJs {
  #[must_use]
  pub fn new(inner: CoreWebError) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl WebErrorJs {
  /// Playwright: `webError.error(): Error`. Returns a **native JS
  /// `Error`** instance constructed via the global `Error`
  /// constructor so `instanceof Error === true` and the value is a
  /// throwable object with a real engine-captured `stack`.
  #[qjs(rename = "error")]
  pub fn error<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    build_native_error(&ctx, self.inner.error())
  }
}

/// Shared helper: construct a native JS `Error` from an [`ErrorDetails`]
/// snapshot. Reused by `WebErrorJs::error` (context-scoped) AND by
/// `PageJs::waitForEvent('pageerror')` (page-scoped, Playwright-parity
/// `Promise<Error>`).
///
/// Constructs the `Error` natively via the global `Error` constructor
/// (`rquickjs::function::Constructor::construct`, the same call-as-new
/// path `page.route` uses for `new URL(...)`), then sets `name` and a
/// non-empty `stack` on the instance. No `ctx.eval` of a JS factory.
pub fn build_native_error<'js>(
  ctx: &rquickjs::Ctx<'js>,
  details: &ErrorDetails,
) -> rquickjs::Result<rquickjs::Value<'js>> {
  let err_ctor: rquickjs::function::Constructor<'js> = ctx.globals().get("Error")?;
  let err: rquickjs::Object<'js> = err_ctor.construct((details.message.clone(),))?;
  err.set("name", details.name.clone())?;
  if !details.stack.is_empty() {
    err.set("stack", details.stack.clone())?;
  }
  Ok(err.into_value())
}
