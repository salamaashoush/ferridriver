//! NAPI binding for the page-scoped `WebStorage` accessor.
//!
//! Mirrors Playwright's client-side `WebStorage`
//! (`/tmp/playwright/packages/playwright-core/src/client/webStorage.ts`),
//! exposed as the readonly `page.localStorage` / `page.sessionStorage`
//! properties. Every method evaluates against the live storage object on
//! the page's main frame in core.

use std::sync::Arc;

use ferridriver::options::{NameValue, WebStorageKind};
use napi::bindgen_prelude::Result;
use napi_derive::napi;

/// `{ name, value }` storage entry returned by [`WebStorage::items`].
#[napi(object)]
pub struct WebStorageItem {
  pub name: String,
  pub value: String,
}

impl From<NameValue> for WebStorageItem {
  fn from(nv: NameValue) -> Self {
    Self {
      name: nv.name,
      value: nv.value,
    }
  }
}

/// Live web-storage accessor for a single origin. Playwright:
/// `page.localStorage` / `page.sessionStorage`.
#[napi]
pub struct WebStorage {
  inner: Arc<ferridriver::Page>,
  kind: WebStorageKind,
}

impl WebStorage {
  pub(crate) fn new(inner: Arc<ferridriver::Page>, kind: WebStorageKind) -> Self {
    Self { inner, kind }
  }
}

#[napi]
impl WebStorage {
  /// Playwright: `webStorage.items(): Promise<{ name, value }[]>`.
  #[napi]
  pub async fn items(&self) -> Result<Vec<WebStorageItem>> {
    self
      .inner
      .web_storage_items(self.kind)
      .await
      .map(|items| items.into_iter().map(WebStorageItem::from).collect())
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `webStorage.getItem(name): Promise<string | null>`.
  #[napi(ts_return_type = "Promise<string | null>")]
  pub async fn get_item(&self, name: String) -> Result<Option<String>> {
    self
      .inner
      .web_storage_get_item(self.kind, &name)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `webStorage.setItem(name, value): Promise<void>`.
  #[napi]
  pub async fn set_item(&self, name: String, value: String) -> Result<()> {
    self
      .inner
      .web_storage_set_item(self.kind, &name, &value)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `webStorage.removeItem(name): Promise<void>`.
  #[napi]
  pub async fn remove_item(&self, name: String) -> Result<()> {
    self
      .inner
      .web_storage_remove_item(self.kind, &name)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `webStorage.clear(): Promise<void>`.
  #[napi]
  pub async fn clear(&self) -> Result<()> {
    self
      .inner
      .web_storage_clear(self.kind)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }
}
