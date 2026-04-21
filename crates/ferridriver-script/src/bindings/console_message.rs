//! `ConsoleMessageJs` — QuickJS binding for
//! [`ferridriver::console_message::ConsoleMessage`].
//!
//! Mirrors Playwright's client-side `ConsoleMessage` class from
//! `/tmp/playwright/packages/playwright-core/src/client/consoleMessage.ts`:
//! sync `type()` / `text()` / `args()` / `location()` / `timestamp()`.
//! `page()` is omitted (symmetric with `DownloadJs` / `FileChooserJs`).

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
  pub fn type_str(&self) -> String {
    self.inner.type_str().to_string()
  }

  /// Playwright: `consoleMessage.text(): string`.
  #[qjs(rename = "text")]
  pub fn text(&self) -> String {
    self.inner.text().to_string()
  }

  /// Playwright: `consoleMessage.args(): JSHandle[]`.
  #[qjs(rename = "args")]
  pub fn args<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let mut out = Vec::with_capacity(self.inner.args().len());
    for h in self.inner.args() {
      let instance = Class::instance(ctx.clone(), JSHandleJs::new(h.clone()))?;
      out.push(rquickjs::IntoJs::into_js(instance, &ctx)?);
    }
    Ok(out)
  }

  /// Playwright: `consoleMessage.location(): { url, lineNumber, columnNumber }`.
  #[qjs(rename = "location")]
  pub fn location<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let loc = self.inner.location();
    let obj = rquickjs::Object::new(ctx.clone())?;
    obj.set("url", loc.url.clone())?;
    obj.set("lineNumber", loc.line_number)?;
    obj.set("columnNumber", loc.column_number)?;
    rquickjs::IntoJs::into_js(obj, &ctx)
  }

  /// Playwright: `consoleMessage.timestamp(): number`.
  #[qjs(rename = "timestamp")]
  pub fn timestamp(&self) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
      self.inner.timestamp() as f64
    }
  }
}
