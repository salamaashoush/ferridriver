//! `FrameJs`: JS wrapper around `ferridriver::Frame`.
//!
//! Mirrors Playwright's `Frame` interface. Shared locator queries and
//! selector actions come from `qjs_impl!`.

use ferridriver::Frame;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;
use rquickjs::function::Opt;

use crate::bindings::convert::FerriResultCtxExt;

/// JS-visible wrapper around [`ferridriver::Frame`]. Constructed only by
/// `PageJs` / other `FrameJs` instances (`mainFrame`, `frames`, `frame`,
/// `parentFrame`, `childFrames`).
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Frame")]
pub struct FrameJs {
  #[qjs(skip_trace)]
  inner: Frame,
}

impl FrameJs {
  #[must_use]
  pub(crate) fn new(inner: Frame) -> Self {
    Self { inner }
  }

  fn backend_page_id(&self) -> usize {
    self.inner.page_arc().backend_page_id()
  }
}

crate::bindings::macros::qjs_impl! {
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs hands methods their Ctx and JS arguments by value"
  )]
  impl FrameJs {
    @mixin locator_queries(optional_frame_locator);
    @mixin selector_surface();

    #[qjs(get)]
    #[must_use]
    pub fn webmcp(&self) -> crate::bindings::web_mcp::WebMcpJs {
      crate::bindings::web_mcp::WebMcpJs::new(self.inner.webmcp())
    }

    /// Frame name (from the `<iframe name=...>` attribute). Sync.
    #[qjs(rename = "name")]
    #[must_use]
    pub fn name(&self) -> String {
      self.inner.name()
    }

    /// Frame URL. Sync.
    #[qjs(rename = "url")]
    #[must_use]
    pub fn url(&self) -> String {
      self.inner.url()
    }

    /// True when this is the top-level page frame. Sync.
    #[qjs(rename = "isMainFrame")]
    #[must_use]
    pub fn is_main_frame(&self) -> bool {
      self.inner.is_main_frame()
    }

    /// Parent frame (null for the main frame). Sync.
    ///
    /// # Errors
    ///
    /// Throws when the engine cannot create or restore the `Frame` object.
    #[qjs(rename = "parentFrame")]
    pub fn parent_frame<'js>(
      &self,
      ctx: rquickjs::Ctx<'js>,
    ) -> rquickjs::Result<crate::bindings::convert::Null<rquickjs::Class<'js, FrameJs>>> {
      let parent = self.inner.parent_frame();
      Ok(crate::bindings::convert::Null(
        parent
          .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
          .transpose()?,
      ))
    }

    /// Child frames of this frame. Sync.
    ///
    /// # Errors
    ///
    /// Throws when the engine cannot create or restore a `Frame` object.
    #[qjs(rename = "childFrames")]
    pub fn child_frames<'js>(
      &self,
      ctx: rquickjs::Ctx<'js>,
    ) -> rquickjs::Result<Vec<rquickjs::Class<'js, FrameJs>>> {
      self
        .inner
        .child_frames()
        .into_iter()
        .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
        .collect()
    }

    /// Whether this frame has been detached from the page. Sync.
    #[qjs(rename = "isDetached")]
    #[must_use]
    pub fn is_detached(&self) -> bool {
      self.inner.is_detached()
    }

    /// Playwright: `frame.goto(url, options?: { referer?, timeout?,
    /// waitUntil? }): Promise<Response | null>`.
    ///
    /// # Errors
    ///
    /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
    /// Playwright name, such as `TimeoutError`, when the navigation fails.
    #[qjs(rename = "goto")]
    pub async fn goto<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      url: String,
      options: Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>> {
      call_site
        .scope(async move {
          let opts = crate::bindings::page::parse_goto_options(&ctx, options)?;
          let response = self.inner.goto(&url).maybe_options(opts).await.into_js_with(&ctx)?;
          Ok(response.map(|r| {
            crate::bindings::network::ResponseJs::new_with_page(r, std::sync::Arc::clone(self.inner.page_arc()))
          }))
        })
        .await
        .map(crate::bindings::convert::Null)
    }

    /// Playwright: `frame.frameElement(): Promise<ElementHandle>`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error under its Playwright name, such as `TargetClosedError`, when
    /// resolving the element fails.
    #[qjs(rename = "frameElement")]
    pub async fn frame_element(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<crate::bindings::element_handle::ElementHandleJs> {
      call_site
        .scope(async move {
          let handle = self.inner.frame_element().await.into_js_with(&ctx)?;
          Ok(crate::bindings::element_handle::ElementHandleJs::new(handle))
        })
        .await
    }

    /// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
    /// waitUntil? }): Promise<Response | null>`.
    #[qjs(rename = "waitForNavigation")]
    #[must_use]
    pub fn wait_for_navigation<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      options: Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::promise::Promised<
      impl std::future::Future<
        Output = rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>>,
      > + 'js,
    > {
      // Listening from the call: the navigation is usually started before
      // the promise is first awaited.
      let navigation = self.inner.wait_for_navigation();
      let opts = crate::bindings::convert::arm_now(&ctx, || {
        crate::bindings::page::parse_wait_for_navigation_options(&ctx, &options)
      });
      let page = std::sync::Arc::clone(self.inner.page_arc());
      rquickjs::promise::Promised::from(call_site.scope(async move {
        let opts = opts.map_err(|deferred| deferred.raise(&ctx))?;
        let response = navigation.options(opts).await.into_js_with(&ctx)?;
        Ok(crate::bindings::convert::Null(
          response.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, page)),
        ))
      }))
    }

    /// Playwright: `frame.page(): Page`.
    ///
    /// # Errors
    ///
    /// Throws when the engine cannot create or restore the `Page` object.
    #[qjs(rename = "page")]
    pub fn page<'js>(
      &self,
      ctx: rquickjs::Ctx<'js>,
    ) -> rquickjs::Result<rquickjs::Class<'js, crate::bindings::page::PageJs>> {
      crate::bindings::identity::page_object(&ctx, std::sync::Arc::clone(self.inner.page_arc()))
    }
  }
}
