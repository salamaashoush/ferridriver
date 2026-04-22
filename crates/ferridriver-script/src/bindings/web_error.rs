//! `WebErrorJs` — QuickJS binding for
//! [`ferridriver::web_error::WebError`].
//!
//! Mirrors Playwright's client-side `WebError` class from
//! `/tmp/playwright/packages/playwright-core/src/client/webError.ts`:
//! sync `error()` getter returning `{ name, message, stack }`. `page()`
//! is omitted (symmetric with `DownloadJs` / `FileChooserJs` /
//! `ConsoleMessageJs`); script-land callers have no need for the
//! page back-reference.

use ferridriver::web_error::WebError as CoreWebError;
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
  /// Playwright: `webError.error(): Error`. Returns a plain object
  /// with `{ name, message, stack }` matching JS `Error`'s shape —
  /// not a thrown `Error` instance so script-land callers can inspect
  /// the fields without `try/catch`.
  #[qjs(rename = "error")]
  pub fn error<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let d = self.inner.error();
    let obj = rquickjs::Object::new(ctx.clone())?;
    obj.set("name", d.name.clone())?;
    obj.set("message", d.message.clone())?;
    obj.set("stack", d.stack.clone())?;
    rquickjs::IntoJs::into_js(obj, &ctx)
  }
}
