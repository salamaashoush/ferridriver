//! `DownloadJs` — QuickJS binding for
//! [`ferridriver::download::Download`].
//!
//! Mirrors Playwright's client-side `Download` class from
//! `/tmp/playwright/packages/playwright-core/src/client/download.ts`:
//! sync `url()` / `suggestedFilename()` accessors + async `path()` /
//! `saveAs(path)` / `cancel()` / `delete()` / `failure()`.

use ferridriver::download::Download as CoreDownload;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Download")]
pub struct DownloadJs {
  #[qjs(skip_trace)]
  inner: CoreDownload,
}

impl DownloadJs {
  #[must_use]
  pub fn new(inner: CoreDownload) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl DownloadJs {
  /// Playwright: `download.url(): string`.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Playwright: `download.suggestedFilename(): string`.
  #[qjs(rename = "suggestedFilename")]
  pub fn suggested_filename(&self) -> String {
    self.inner.suggested_filename()
  }

  /// Playwright: `download.path(): Promise<string>`.
  #[qjs(rename = "path")]
  pub async fn path(&self) -> rquickjs::Result<String> {
    let p = self.inner.path().await.into_js()?;
    Ok(p.to_string_lossy().into_owned())
  }

  /// Playwright: `download.saveAs(path): Promise<void>`.
  #[qjs(rename = "saveAs")]
  pub async fn save_as(&self, path: String) -> rquickjs::Result<()> {
    self.inner.save_as(&std::path::PathBuf::from(path)).await.into_js()
  }

  /// Playwright: `download.cancel(): Promise<void>`.
  #[qjs(rename = "cancel")]
  pub async fn cancel(&self) -> rquickjs::Result<()> {
    self.inner.cancel().await.into_js()
  }

  /// Playwright: `download.delete(): Promise<void>`.
  #[qjs(rename = "delete")]
  pub async fn delete(&self) -> rquickjs::Result<()> {
    self.inner.delete().await.into_js()
  }

  /// Playwright: `download.failure(): Promise<string | null>`.
  #[qjs(rename = "failure")]
  pub async fn failure(&self) -> Option<String> {
    self.inner.failure().await
  }
}
