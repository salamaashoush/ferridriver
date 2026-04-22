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
/// Uses [`rquickjs::Ctx::eval`] once per call to define+invoke a tiny
/// factory `(n, m, s) => { const e = new Error(m); … }`. `rquickjs`'s
/// `Function` lacks a `call-as-new` method (see `Constructor::construct`
/// — only reachable via `Class::create_constructor` which is meant for
/// Rust-side class registration), and its `pub(crate)` inner field
/// blocks the obvious `Constructor(fun)` wrap. Going through `eval`
/// keeps the binding readable without a newtype-conversion detour
/// into rquickjs internals.
pub fn build_native_error<'js>(
  ctx: &rquickjs::Ctx<'js>,
  details: &ErrorDetails,
) -> rquickjs::Result<rquickjs::Value<'js>> {
  let factory_src = b"(function(n, m, s) { var e = new Error(m); e.name = n; if (s) { e.stack = s; } return e; })";
  let factory: rquickjs::Function<'js> = ctx.eval(factory_src.as_slice())?;
  factory.call::<_, rquickjs::Value<'js>>((details.name.clone(), details.message.clone(), details.stack.clone()))
}
