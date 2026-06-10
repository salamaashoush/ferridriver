//! `VideoJs` — QuickJS binding for [`ferridriver::Video`].
//!
//! Mirrors Playwright's client-side `Video` class from
//! `/tmp/playwright/packages/playwright-core/src/client/video.ts` and
//! the public-type contract in
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:21621`:
//! sync construction, async `path()` / `saveAs()` / `delete()` that
//! block until the owning page closes and the encoder finalises.

use std::sync::Arc;

use crate::bindings::convert::FerriResultCtxExt;
use ferridriver::Video as CoreVideo;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Video")]
pub struct VideoJs {
  #[qjs(skip_trace)]
  inner: Arc<CoreVideo>,
}

impl VideoJs {
  #[must_use]
  pub fn new(inner: Arc<CoreVideo>) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl VideoJs {
  /// Playwright: `video.path(): Promise<string>`.
  #[qjs(rename = "path")]
  pub async fn path(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    let path = self.inner.path().await.into_js_with(&ctx)?;
    Ok(path.to_string_lossy().into_owned())
  }

  /// Playwright: `video.saveAs(path: string): Promise<void>`.
  #[qjs(rename = "saveAs")]
  pub async fn save_as(&self, ctx: rquickjs::Ctx<'_>, path: String) -> rquickjs::Result<()> {
    self.inner.save_as(path).await.into_js_with(&ctx)
  }

  /// Playwright: `video.delete(): Promise<void>`.
  #[qjs(rename = "delete")]
  pub async fn delete(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.delete().await.into_js_with(&ctx)
  }
}
