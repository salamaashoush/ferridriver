//! NAPI binding for [`ferridriver::dialog::Dialog`].
//!
//! Mirrors Playwright's client-side `Dialog` from
//! `/tmp/playwright/packages/playwright-core/src/client/dialog.ts`:
//! sync read-only accessors + async `accept(promptText?)` / `dismiss()`.

use ferridriver::dialog::Dialog as CoreDialog;
use napi::Result;
use napi_derive::napi;

/// Live dialog handle — observed via `page.waitForEvent('dialog')` or
/// `page.on('dialog', cb)` (the latter currently delivers a JSON
/// snapshot with the same fields; consumers can still call
/// [`crate::page::Page::accept_dialog`] / `dismiss_dialog` by opaque
/// id. Full live-handle delivery on `on` will follow in a later
/// refactor).
#[napi]
pub struct Dialog {
  pub(crate) inner: CoreDialog,
}

impl Dialog {
  pub(crate) fn from_core(inner: CoreDialog) -> Self {
    Self { inner }
  }
}

#[napi]
impl Dialog {
  /// Playwright: `dialog.type(): string` —
  /// `"alert" | "beforeunload" | "confirm" | "prompt"`.
  #[napi(js_name = "type")]
  pub fn dialog_type(&self) -> String {
    self.inner.dialog_type().as_str().to_string()
  }

  /// Playwright: `dialog.message(): string`.
  #[napi]
  pub fn message(&self) -> String {
    self.inner.message().to_string()
  }

  /// Playwright: `dialog.defaultValue(): string`. Empty for non-prompt
  /// dialogs.
  #[napi]
  pub fn default_value(&self) -> String {
    self.inner.default_value().to_string()
  }

  /// Playwright: `dialog.accept(promptText?): Promise<void>`.
  /// `promptText` is applied to `prompt` dialogs only. Double-accept
  /// / accept-after-dismiss rejects with the Playwright-exact message
  /// `"Cannot accept dialog which is already handled!"`.
  #[napi]
  pub async fn accept(&self, prompt_text: Option<String>) -> Result<()> {
    use crate::error::IntoNapi;
    self.inner.accept(prompt_text).await.into_napi()
  }

  /// Playwright: `dialog.dismiss(): Promise<void>`.
  #[napi]
  pub async fn dismiss(&self) -> Result<()> {
    use crate::error::IntoNapi;
    self.inner.dismiss().await.into_napi()
  }
}
