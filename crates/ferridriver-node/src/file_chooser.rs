//! NAPI binding for [`ferridriver::file_chooser::FileChooser`].
//!
//! Mirrors Playwright's client-side `FileChooser` from
//! `/tmp/playwright/packages/playwright-core/src/client/fileChooser.ts`:
//! sync `element()` / `isMultiple()` / `page()` accessors + async
//! `setFiles(files, options?)`.

use ferridriver::file_chooser::FileChooser as CoreFileChooser;
use napi::Result;
use napi_derive::napi;

/// Live file-chooser handle — observed via
/// `page.waitForEvent('filechooser')` or `page.on('filechooser', cb)`.
#[napi]
pub struct FileChooser {
  pub(crate) inner: CoreFileChooser,
}

impl FileChooser {
  pub(crate) fn from_core(inner: CoreFileChooser) -> Self {
    Self { inner }
  }
}

#[napi]
impl FileChooser {
  /// Playwright: `fileChooser.page(): Page`.
  #[napi(ts_return_type = "Page")]
  pub fn page(&self) -> crate::identity::PageObject {
    crate::identity::PageObject(std::sync::Arc::clone(self.inner.page()))
  }

  /// Playwright: `fileChooser.element(): ElementHandle`.
  #[napi]
  pub fn element(&self) -> crate::element_handle::ElementHandle {
    crate::element_handle::ElementHandle::wrap(self.inner.element().clone())
  }

  /// Playwright: `fileChooser.isMultiple(): boolean`.
  #[napi]
  pub fn is_multiple(&self) -> bool {
    self.inner.is_multiple()
  }

  /// Playwright: `fileChooser.setFiles(files, options?): Promise<void>`.
  /// Accepts the full `string | string[] | FilePayload | FilePayload[]`
  /// union — delegates to the underlying `ElementHandle.setInputFiles`.
  #[napi(ts_args_type = "files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions")]
  pub async fn set_files(
    &self,
    files: crate::types::NapiInputFiles,
    mut options: Option<crate::types::SetInputFilesOptions>,
  ) -> Result<()> {
    let signal = crate::abort::take(&mut options);
    let opts = options.map(Into::into);
    crate::abort::run(signal, self.inner.set_files(files.0, opts)).await
  }
}
