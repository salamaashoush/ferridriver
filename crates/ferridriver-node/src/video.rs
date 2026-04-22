//! NAPI binding for [`ferridriver::Video`].
//!
//! Mirrors Playwright's `Video` class from
//! `/tmp/playwright/packages/playwright-core/src/client/video.ts` and
//! the public-type contract in
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:21621`:
//!
//! ```text
//! interface Video {
//!   delete(): Promise<void>;
//!   path(): Promise<string>;
//!   saveAs(path: string): Promise<void>;
//! }
//! ```
//!
//! All three methods block until the owning page closes and the
//! encoder has finalised the file — matches Playwright's contract.

use std::sync::Arc;

use crate::error::IntoNapi;
use ferridriver::Video as CoreVideo;
use napi::Result;
use napi_derive::napi;

/// Video-recording handle returned by `page.video()`. `null` when the
/// owning context was not created with `recordVideo`.
#[napi]
pub struct Video {
  inner: Arc<CoreVideo>,
}

impl Video {
  pub(crate) fn from_core(inner: Arc<CoreVideo>) -> Self {
    Self { inner }
  }
}

#[napi]
impl Video {
  /// Playwright: `video.path(): Promise<string>`. Resolves once the
  /// page closes and the encoder finalises the file — matches
  /// Playwright's "guaranteed to be written to the filesystem upon
  /// closing the browser context" contract.
  #[napi]
  pub async fn path(&self) -> Result<String> {
    let path = self.inner.path().await.into_napi()?;
    Ok(path.to_string_lossy().into_owned())
  }

  /// Playwright: `video.saveAs(path): Promise<void>`. Safe to call
  /// before or after the page closes — blocks until the recording is
  /// finalised, then copies to `dest`.
  #[napi]
  pub async fn save_as(&self, path: String) -> Result<()> {
    self.inner.save_as(path).await.into_napi()
  }

  /// Playwright: `video.delete(): Promise<void>`. Blocks until the
  /// recording is finalised, then removes the file. No-op when the
  /// recording couldn't be produced (e.g. typed `Unsupported` from
  /// the backend).
  #[napi]
  pub async fn delete(&self) -> Result<()> {
    self.inner.delete().await.into_napi()
  }
}
