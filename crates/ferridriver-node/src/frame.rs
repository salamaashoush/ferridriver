//! Frame class -- NAPI binding for `ferridriver::Frame`.

use crate::abort::Bag;
use crate::error::IntoNapi;
use napi::Result;
use napi_derive::napi;

/// A frame within a page (main frame or iframe).
/// Mirrors Playwright's Frame interface.
#[napi]
pub struct Frame {
  inner: ferridriver::Frame,
}

impl Frame {
  pub(crate) fn wrap(inner: ferridriver::Frame) -> Self {
    Self { inner }
  }
}

crate::macros::napi_locator_queries!(Frame, optional_frame_locator);
crate::macros::napi_selector_surface!(Frame);

#[napi]
impl Frame {
  #[napi(getter)]
  pub fn webmcp(&self) -> crate::web_mcp::WebMcp {
    crate::web_mcp::WebMcp::wrap(self.inner.webmcp())
  }

  /// Frame name (from the `name` attribute of the iframe element).
  /// Playwright: `frame.name(): string` (sync).
  #[napi]
  pub fn name(&self) -> String {
    self.inner.name()
  }

  /// Frame URL. Playwright: `frame.url(): string` (sync).
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// Whether this is the main (top-level) frame.
  #[napi]
  pub fn is_main_frame(&self) -> bool {
    self.inner.is_main_frame()
  }

  /// Parent frame. Returns null for the main frame.
  /// Playwright: `frame.parentFrame(): Frame | null` (sync).
  #[napi(ts_return_type = "Frame | null")]
  pub fn parent_frame(&self) -> Option<crate::identity::FrameObject> {
    self.inner.parent_frame().map(crate::identity::FrameObject)
  }

  /// Child frames of this frame.
  /// Playwright: `frame.childFrames(): Frame[]` (sync).
  #[napi(ts_return_type = "Frame[]")]
  pub fn child_frames(&self) -> Vec<crate::identity::FrameObject> {
    self
      .inner
      .child_frames()
      .into_iter()
      .map(crate::identity::FrameObject)
      .collect()
  }

  /// Playwright: `frame.goto(url, options?: { referer?, timeout?,
  /// waitUntil? }): Promise<Response | null>`. `null` for a navigation
  /// within the document.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn goto(
    &self,
    url: String,
    options: Option<crate::types::GotoOptions>,
  ) -> Result<Option<crate::network::Response>> {
    let resp = self.inner.goto(&url).bag(options)?.await.into_napi()?;
    let page = std::sync::Arc::clone(self.inner.page_arc());
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, page)))
  }

  /// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`.
  #[napi(
    ts_args_type = "options?: { timeout?: number, signal?: AbortSignal, url?: string | RegExp, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }",
    ts_return_type = "Promise<Response | null>"
  )]
  pub fn wait_for_navigation(
    &self,
    env: napi::Env,
    options: Option<crate::types::WaitForNavigationOptions>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<Option<crate::network::Response>>> {
    // Listening from the call: the navigation is usually started before
    // the promise is awaited.
    let navigation = self.inner.wait_for_navigation().bag(options);
    let page = std::sync::Arc::clone(self.inner.page_arc());
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let resp = navigation?.await.into_napi()?;
      Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, page)))
    })
    .build(&env)
  }

  /// Playwright: `frame.frameElement(): Promise<ElementHandle>`.
  #[napi]
  pub async fn frame_element(&self) -> Result<crate::element_handle::ElementHandle> {
    let handle = self.inner.frame_element().await.into_napi()?;
    Ok(crate::element_handle::ElementHandle::wrap(handle))
  }

  /// Whether this frame has been detached from the page.
  /// Playwright: `frame.isDetached(): boolean` (sync).
  #[napi]
  pub fn is_detached(&self) -> bool {
    self.inner.is_detached()
  }

  /// Playwright: `frame.page(): Page` — the page this frame belongs to.
  #[napi(ts_return_type = "Page")]
  pub fn page(&self) -> crate::identity::PageObject {
    crate::identity::PageObject(std::sync::Arc::clone(self.inner.page_arc()))
  }

  #[napi(js_name = "type")]
  pub async fn type_text(
    &self,
    selector: String,
    text: String,
    options: Option<crate::types::TypeOptions>,
  ) -> Result<()> {
    self.inner.r#type(&selector, &text).bag(options)?.await.into_napi()
  }
}
