//! `DownloadJs` — `QuickJS` binding for
//! [`ferridriver::download::Download`].
//!
//! Mirrors Playwright's client-side `Download` class from
//! `/tmp/playwright/packages/playwright-core/src/client/download.ts`:
//! sync `url()` / `suggestedFilename()` accessors + async `path()` /
//! `saveAs(path)` / `cancel()` / `delete()` / `failure()`.

use ferridriver::download::Download as CoreDownload;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultCtxExt;

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
  #[must_use]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Playwright: `download.suggestedFilename(): string`.
  #[qjs(rename = "suggestedFilename")]
  #[must_use]
  pub fn suggested_filename(&self) -> String {
    self.inner.suggested_filename()
  }

  /// Playwright: `download.page(): Page`. Answers `null` only when the
  /// owning page has already been closed (the Rust weak-backref model
  /// surfaces that case; Playwright's TS type is non-nullable).
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
  ) -> rquickjs::Result<crate::bindings::convert::Null<rquickjs::Class<'js, crate::bindings::page::PageJs>>> {
    let page = self.inner.page();
    Ok(crate::bindings::convert::Null(
      page
        .map(|page| crate::bindings::identity::page_object(&ctx, page))
        .transpose()?,
    ))
  }

  /// Playwright: `download.path(): Promise<string>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the download failed or was cancelled.
  #[qjs(rename = "path")]
  pub async fn path(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    let p = self.inner.path().await.into_js_with(&ctx)?;
    Ok(p.to_string_lossy().into_owned())
  }

  /// Playwright: `download.saveAs(path): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `PermissionDeniedError` when the realm's write grant refuses `path`, or the core
  /// error when the download failed or the copy cannot be written.
  #[qjs(rename = "saveAs")]
  pub async fn save_as(&self, ctx: rquickjs::Ctx<'_>, path: String) -> rquickjs::Result<()> {
    crate::bindings::convert::writable(&ctx, std::path::Path::new(&path))?;
    self
      .inner
      .save_as(&std::path::PathBuf::from(path))
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `download.cancel(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the download cannot be cancelled.
  #[qjs(rename = "cancel")]
  pub async fn cancel(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.cancel().await.into_js_with(&ctx)
  }

  /// Playwright: `download.delete(): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the downloaded file cannot be deleted.
  #[qjs(rename = "delete")]
  pub async fn delete(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.delete().await.into_js_with(&ctx)
  }

  /// Playwright: `download.failure(): Promise<string | null>`.
  #[qjs(rename = "failure")]
  pub async fn failure(&self) -> crate::bindings::convert::Null<String> {
    crate::bindings::convert::Null(self.inner.failure().await)
  }
}
