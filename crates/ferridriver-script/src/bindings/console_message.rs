//! `ConsoleMessageJs` — `QuickJS` binding for
//! [`ferridriver::console_message::ConsoleMessage`].
//!
//! Mirrors Playwright's client-side `ConsoleMessage` class from
//! `/tmp/playwright/packages/playwright-core/src/client/consoleMessage.ts`:
//! sync `type()` / `text()` / `args()` / `location()` / `timestamp()` /
//! `page()`.

use ferridriver::console_message::ConsoleMessage as CoreConsoleMessage;
use rquickjs::JsLifetime;
use rquickjs::class::{Class, Trace};

use crate::bindings::js_handle::JSHandleJs;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "ConsoleMessage")]
pub struct ConsoleMessageJs {
  #[qjs(skip_trace)]
  inner: CoreConsoleMessage,
}

impl ConsoleMessageJs {
  #[must_use]
  pub fn new(inner: CoreConsoleMessage) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl ConsoleMessageJs {
  /// Playwright: `consoleMessage.type(): string`.
  #[qjs(rename = "type")]
  #[must_use]
  pub fn type_str(&self) -> String {
    self.inner.type_str().to_string()
  }

  /// Playwright: `consoleMessage.text(): string`.
  #[qjs(rename = "text")]
  #[must_use]
  pub fn text(&self) -> String {
    self.inner.text().to_string()
  }

  /// Playwright: `consoleMessage.args(): JSHandle[]`.
  ///
  /// # Errors
  ///
  /// Throws when a `JSHandle` object cannot be created in this context.
  #[qjs(rename = "args")]
  pub fn args<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let mut out = Vec::with_capacity(self.inner.args().len());
    for h in self.inner.args() {
      let instance = Class::instance(ctx.clone(), JSHandleJs::new(h.clone()))?;
      out.push(rquickjs::IntoJs::into_js(instance, &ctx)?);
    }
    Ok(out)
  }

  /// Playwright: `consoleMessage.location(): { url, lineNumber, columnNumber }`.
  ///
  /// # Errors
  ///
  /// Throws when the location object cannot be created.
  #[qjs(rename = "location")]
  pub fn location<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let loc = self.inner.location();
    let obj = rquickjs::Object::new(ctx)?;
    obj.set("url", loc.url.clone())?;
    obj.set("lineNumber", loc.line_number)?;
    obj.set("columnNumber", loc.column_number)?;
    Ok(obj.into_value())
  }

  /// Playwright: `consoleMessage.page(): Page | null`.
  ///
  /// # Errors
  ///
  /// Throws when the page's object cannot be created or restored in this context.
  #[qjs(rename = "page")]
  pub fn page<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<Class<'js, crate::bindings::page::PageJs>>> {
    self
      .inner
      .page()
      .map(|page| crate::bindings::identity::page_object(&ctx, page))
      .transpose()
      .map(crate::bindings::convert::Null)
  }

  /// Playwright: `consoleMessage.timestamp(): number`.
  #[qjs(rename = "timestamp")]
  #[must_use]
  pub fn timestamp(&self) -> f64 {
    crate::bindings::convert::u64_to_f64(self.inner.timestamp())
  }
}
