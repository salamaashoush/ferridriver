//! NAPI binding for [`ferridriver::download::Download`].
//!
//! Mirrors Playwright's client-side `Download` from
//! `/tmp/playwright/packages/playwright-core/src/client/download.ts`:
//! sync `url()` / `suggestedFilename()` + async `path()` / `saveAs()` /
//! `createReadStream()` / `cancel()` / `delete()` / `failure()`.

use std::path::PathBuf;

use ferridriver::download::Download as CoreDownload;
use napi::Result;
use napi_derive::napi;

use crate::error::IntoNapi;

/// Live download handle — observed via
/// `page.waitForEvent('download')` or `page.on('download', cb)`.
#[napi]
pub struct Download {
  pub(crate) inner: CoreDownload,
}

impl Download {
  pub(crate) fn from_core(inner: CoreDownload) -> Self {
    Self { inner }
  }
}

#[napi]
impl Download {
  /// Playwright: `download.url(): string`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Playwright: `download.suggestedFilename(): string`.
  #[napi]
  pub fn suggested_filename(&self) -> String {
    self.inner.suggested_filename()
  }

  /// Playwright: `download.page(): Page`. Throws if the owning page
  /// has already been closed — TS consumers don't see a dead-page case
  /// in Playwright, but Rust's weak-backref model surfaces it honestly.
  #[napi]
  pub fn page(&self) -> Result<crate::page::Page> {
    let page = self
      .inner
      .page()
      .ok_or_else(|| napi::Error::from_reason("download's owning page has been closed"))?;
    Ok(crate::page::Page::wrap(page))
  }

  /// Playwright: `download.path(): Promise<string>`.
  #[napi]
  pub async fn path(&self) -> Result<String> {
    let p = self.inner.path().await.into_napi()?;
    Ok(p.to_string_lossy().into_owned())
  }

  /// Playwright: `download.saveAs(path): Promise<void>`.
  #[napi]
  pub async fn save_as(&self, path: String) -> Result<()> {
    self.inner.save_as(&PathBuf::from(path)).await.into_napi()
  }

  /// Playwright: `download.cancel(): Promise<void>`.
  #[napi]
  pub async fn cancel(&self) -> Result<()> {
    self.inner.cancel().await.into_napi()
  }

  /// Playwright: `download.delete(): Promise<void>`.
  #[napi]
  pub async fn delete(&self) -> Result<()> {
    self.inner.delete().await.into_napi()
  }

  /// Playwright: `download.failure(): Promise<string | null>`.
  #[napi(ts_return_type = "Promise<string | null>")]
  pub async fn failure(&self) -> Option<String> {
    self.inner.failure().await
  }
}

// `createReadStream` — Playwright's Node client returns `Readable`
// from a server-side file stream. In ferridriver's local-file model,
// `await download.path()` already resolves the on-disk path, so
// consumers get the same ergonomics via
// `fs.createReadStream(await download.path())`. A native-NAPI Readable
// binding would require wiring the full `stream.Readable` lifecycle
// through `napi::sys` and is left for a future NAPI-parity pass — the
// underlying primitive (`path()`) is already exposed.
