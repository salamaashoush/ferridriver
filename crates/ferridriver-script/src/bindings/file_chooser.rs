//! `FileChooserJs` — `QuickJS` binding for
//! [`ferridriver::file_chooser::FileChooser`].
//!
//! Mirrors Playwright's client-side `FileChooser` class from
//! `/tmp/playwright/packages/playwright-core/src/client/fileChooser.ts`:
//! sync `element()` / `isMultiple()` accessors + async
//! `setFiles(files, options?)`.

use ferridriver::file_chooser::FileChooser as CoreFileChooser;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultCtxExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "FileChooser")]
pub struct FileChooserJs {
  #[qjs(skip_trace)]
  inner: CoreFileChooser,
}

impl FileChooserJs {
  #[must_use]
  pub fn new(inner: CoreFileChooser) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl FileChooserJs {
  /// Playwright: `fileChooser.element(): ElementHandle`.
  #[qjs(rename = "element")]
  #[must_use]
  pub fn element(&self) -> crate::bindings::element_handle::ElementHandleJs {
    crate::bindings::element_handle::ElementHandleJs::new(self.inner.element().clone())
  }

  /// Playwright: `fileChooser.isMultiple(): boolean`.
  #[qjs(rename = "isMultiple")]
  #[must_use]
  pub fn is_multiple(&self) -> bool {
    self.inner.is_multiple()
  }

  /// Playwright: `fileChooser.page(): Page`. Derived from the captured
  /// element's owning page.
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
  ) -> rquickjs::Result<rquickjs::Class<'js, crate::bindings::page::PageJs>> {
    crate::bindings::identity::page_object(&ctx, std::sync::Arc::clone(self.inner.page()))
  }

  /// Playwright: `fileChooser.setFiles(files, options?): Promise<void>`.
  /// Accepts the full `string | string[] | FilePayload | FilePayload[]`
  /// union — delegates through the captured `ElementHandle`'s
  /// `setInputFiles`, which reuses the §1.5 path/payload plumbing.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `files` or options bag, a `PermissionDeniedError` for
  /// a path outside the realm's read grant, or the core error when the files cannot be set.
  #[qjs(rename = "setFiles")]
  pub async fn set_files<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
    let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
    self.inner.set_files(files).maybe_options(opts).await.into_js_with(&ctx)
  }
}
