//! `DialogJs` — QuickJS binding for [`ferridriver::dialog::Dialog`].
//!
//! Mirrors Playwright's client-side `Dialog` class from
//! `/tmp/playwright/packages/playwright-core/src/client/dialog.ts`:
//! read-only accessors (`type()`, `message()`, `defaultValue()`) +
//! async `accept(promptText?)` / `dismiss()`.

use ferridriver::dialog::Dialog as CoreDialog;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;
use rquickjs::function::Opt;

use crate::bindings::convert::FerriResultCtxExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Dialog")]
pub struct DialogJs {
  #[qjs(skip_trace)]
  inner: CoreDialog,
}

impl DialogJs {
  #[must_use]
  pub fn new(inner: CoreDialog) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl DialogJs {
  /// Playwright: `dialog.type(): string` — `"alert"` / `"beforeunload"`
  /// / `"confirm"` / `"prompt"`.
  #[qjs(rename = "type")]
  pub fn dialog_type(&self) -> String {
    self.inner.dialog_type().as_str().to_string()
  }

  /// Playwright: `dialog.message(): string`.
  #[qjs(rename = "message")]
  pub fn message(&self) -> String {
    self.inner.message().to_string()
  }

  /// Playwright: `dialog.defaultValue(): string`. Empty for non-prompt
  /// dialogs.
  #[qjs(rename = "defaultValue")]
  pub fn default_value(&self) -> String {
    self.inner.default_value().to_string()
  }

  /// Playwright: `dialog.page(): Page | null`. `null` when the dialog
  /// opened before its page was available (early page initialization)
  /// or the owning page has been closed.
  #[qjs(rename = "page")]
  pub fn page(&self) -> Option<crate::bindings::page::PageJs> {
    self.inner.page().map(crate::bindings::page::PageJs::new)
  }

  /// Playwright: `dialog.accept(promptText?): Promise<void>`.
  /// `promptText` is applied to `prompt` dialogs only; other types
  /// ignore it. Double-accept / accept-after-dismiss rejects with the
  /// Playwright-exact message `"Cannot accept dialog which is already
  /// handled!"`.
  #[qjs(rename = "accept")]
  pub async fn accept(&self, ctx: rquickjs::Ctx<'_>, prompt_text: Opt<String>) -> rquickjs::Result<()> {
    self.inner.accept(prompt_text.0).await.into_js_with(&ctx)
  }

  /// Playwright: `dialog.dismiss(): Promise<void>`.
  #[qjs(rename = "dismiss")]
  pub async fn dismiss(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.dismiss().await.into_js_with(&ctx)
  }
}
