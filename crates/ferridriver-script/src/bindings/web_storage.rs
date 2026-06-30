//! `WebStorageJs`: QuickJS binding for the page-scoped `WebStorage`
//! accessor.
//!
//! Mirrors Playwright's client-side `WebStorage`
//! (`/tmp/playwright/packages/playwright-core/src/client/webStorage.ts`),
//! exposed as the `page.localStorage` / `page.sessionStorage`
//! properties. Each method evaluates against the live storage object on
//! the page's main frame in core.

use std::sync::Arc;

use ferridriver::Page;
use ferridriver::options::WebStorageKind;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::{FerriResultCtxExt, serde_to_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebStorage")]
pub struct WebStorageJs {
  #[qjs(skip_trace)]
  page: Arc<Page>,
  #[qjs(skip_trace)]
  kind: WebStorageKind,
}

impl WebStorageJs {
  #[must_use]
  pub fn new(page: Arc<Page>, kind: WebStorageKind) -> Self {
    Self { page, kind }
  }
}

#[rquickjs::methods]
impl WebStorageJs {
  /// Playwright: `webStorage.items(): Promise<{ name, value }[]>`.
  #[qjs(rename = "items")]
  pub async fn items<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let items = self.page.web_storage_items(self.kind).await.into_js_with(&ctx)?;
    serde_to_js(&ctx, &items)
  }

  /// Playwright: `webStorage.getItem(name): Promise<string | null>`.
  #[qjs(rename = "getItem")]
  pub async fn get_item<'js>(&self, ctx: rquickjs::Ctx<'js>, name: String) -> rquickjs::Result<Option<String>> {
    self
      .page
      .web_storage_get_item(self.kind, &name)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `webStorage.setItem(name, value): Promise<void>`.
  #[qjs(rename = "setItem")]
  pub async fn set_item<'js>(&self, ctx: rquickjs::Ctx<'js>, name: String, value: String) -> rquickjs::Result<()> {
    self
      .page
      .web_storage_set_item(self.kind, &name, &value)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `webStorage.removeItem(name): Promise<void>`.
  #[qjs(rename = "removeItem")]
  pub async fn remove_item<'js>(&self, ctx: rquickjs::Ctx<'js>, name: String) -> rquickjs::Result<()> {
    self
      .page
      .web_storage_remove_item(self.kind, &name)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `webStorage.clear(): Promise<void>`.
  #[qjs(rename = "clear")]
  pub async fn clear<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<()> {
    self.page.web_storage_clear(self.kind).await.into_js_with(&ctx)
  }
}
