//! Frame API -- mirrors Playwright's Frame interface.
//!
//! A Frame represents an execution context within a Page.
//! The main frame is the top-level page frame. Child frames
//! correspond to `<iframe>` elements.
//!
//! Frame has the same evaluation and locator methods as Page,
//! but scoped to its specific frame context.

use std::sync::Arc;

use crate::error::Result;
use crate::locator::Locator;
use crate::options::{RoleOptions, StringOrRegex, TextOptions};
use crate::page::Page;

mod actions;
mod eval;
mod navigation;

/// A frame within a page. Mirrors Playwright's
/// [Frame interface](https://playwright.dev/docs/api/class-frame).
///
/// Frame instances are thin handles — the authoritative name/url/parent
/// state lives in `crate::frame_cache::FrameCache` on the owning Page.
/// Cloning a Frame is cheap (`Arc<Page>` + `Arc<str>`) and multiple
/// clones see the same live state.
#[derive(Clone)]
pub struct Frame {
  /// The page this frame belongs to (Arc for cheap cloning in locator chains).
  page: Arc<Page>,
  /// Frame ID (from CDP or backend). `Arc<str>` so locator chains are cheap.
  pub(crate) id: Arc<str>,
}

impl Frame {
  #[must_use]
  pub fn webmcp(&self) -> crate::web_mcp::WebMcp {
    crate::web_mcp::WebMcp::new(self.clone())
  }

  /// Create a frame handle pointing at an id present in the page's
  /// frame cache. The cache is the source of truth for name/url/parent.
  pub(crate) fn new(page: Arc<Page>, id: Arc<str>) -> Self {
    Self { page, id }
  }

  /// The frame a wait that began on this one is about now. A cross-process
  /// navigation hands the page a main frame under a new id, and the main
  /// frame stays itself through it, as Playwright's main `Frame` does;
  /// any other frame is this one.
  fn live(&self, main: bool) -> Frame {
    if main {
      let id = self
        .page
        .with_frame_cache(crate::frame_cache::FrameCache::main_frame_id);
      if id != self.id {
        return Frame::new(Arc::clone(&self.page), id);
      }
    }
    self.clone()
  }

  /// Backend-issued frame identifier. Sync — the underlying `Arc<str>`
  /// is set at construction. Used by the network module to match
  /// `Request.frame()` lookups against the page's frame cache.
  #[must_use]
  pub fn frame_id(&self) -> &str {
    &self.id
  }

  /// Frame name (from the `name` attribute of the iframe element).
  /// Playwright: [`frame.name()`](https://playwright.dev/docs/api/class-frame#frame-name)
  /// -- `name(): string` sync, reads cached state.
  #[must_use]
  pub fn name(&self) -> String {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).map(|r| r.info.name.clone()).unwrap_or_default())
  }

  /// Frame URL.
  /// Playwright: [`frame.url()`](https://playwright.dev/docs/api/class-frame#frame-url)
  /// -- `url(): string` sync.
  #[must_use]
  pub fn url(&self) -> String {
    let (url, is_main) = self.page.with_frame_cache(|c| {
      (
        c.record(&self.id).map(|r| r.info.url.clone()).unwrap_or_default(),
        c.main_frame_id() == self.id,
      )
    });
    // A page is on its initial empty document until a navigation
    // commits, and no backend reports that one: Playwright's
    // `page.url()` reads `about:blank` there.
    if url.is_empty() && is_main {
      return String::from("about:blank");
    }
    url
  }

  /// Whether this is the main (top-level) frame. Mirrors Playwright's
  /// equivalent of `frame.parentFrame() === null`.
  #[must_use]
  pub fn is_main_frame(&self) -> bool {
    self.page.with_frame_cache(|c| c.main_frame_id() == self.id)
  }

  /// Parent frame. Returns `None` for the main frame. Sync — reads from
  /// the page's frame cache (Playwright:
  /// [`frame.parentFrame()`](https://playwright.dev/docs/api/class-frame#frame-parent-frame)).
  #[must_use]
  pub fn parent_frame(&self) -> Option<Frame> {
    let pid = self.page.with_frame_cache(|c| c.parent_id(&self.id))?;
    Some(Frame::new(Arc::clone(&self.page), pid))
  }

  /// Child frames. Sync — reads from the page's frame cache.
  /// Playwright: [`frame.childFrames()`](https://playwright.dev/docs/api/class-frame#frame-child-frames).
  #[must_use]
  pub fn child_frames(&self) -> Vec<Frame> {
    let ids = self.page.with_frame_cache(|c| c.child_ids(&self.id));
    ids
      .into_iter()
      .map(|id| Frame::new(Arc::clone(&self.page), id))
      .collect()
  }

  // ── Locators (frame-scoped) ──────────────────────────────────────────

  /// Create a locator scoped to this frame.
  ///
  /// Playwright: `frame.locator(selector, options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:324`).
  /// The options-bag form is [`Self::locator_with`].
  #[must_use]
  pub fn locator(&self, selector: &str) -> Locator {
    Locator::new(self.clone(), selector.to_string())
  }

  /// The locator a SELECTOR-taking method resolves through.
  ///
  /// Not the same as [`Self::locator`]: a Locator is always strict,
  /// while `frame.click(selector)` and friends take their strictness
  /// from the context's `strictSelectors`, which defaults to false.
  /// Playwright makes the same split in one place
  /// (`server/frameSelectors.ts:53`), where an explicit per-call
  /// `strict` wins and the context option is the fallback.
  fn selector_locator(&self, selector: &str) -> Locator {
    self.locator(selector).strict(self.page.strict_selectors())
  }

  /// [`Self::locator`] with Playwright's `LocatorOptions` filter bag
  /// (including `visible`).
  #[must_use]
  pub fn locator_with(&self, selector: &str, options: &crate::options::FilterOptions) -> Locator {
    self.locator(selector).filter_with(options)
  }

  #[must_use]
  pub fn get_by_role(
    &self,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    let frame = self.clone();
    let role = role.into();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(frame.clone(), crate::locator::build_role_selector(role.as_str(), opts))
    })
  }

  /// `getByText` in this frame. Accepts `string | RegExp`.
  #[must_use]
  pub fn get_by_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:text", text.into())
  }

  /// `getByTestId` in this frame, against the attribute this frame's
  /// context reads ([`crate::context::ContextRef::test_id_attribute`]).
  #[must_use]
  pub fn get_by_test_id(&self, test_id: impl Into<StringOrRegex>) -> Locator {
    Locator::new(
      self.clone(),
      crate::locator::build_testid_selector(&self.test_id_attribute(), &test_id.into()),
    )
  }

  /// The `testIdAttribute` in force for this frame — its page's context,
  /// falling back to the process default for a page with no context (a
  /// standalone MCP session).
  #[must_use]
  pub fn test_id_attribute(&self) -> std::borrow::Cow<'static, str> {
    self.page.context().map_or_else(
      crate::selectors::default_test_id_attribute,
      super::context::ContextRef::test_id_attribute,
    )
  }

  /// `getByLabel` in this frame.
  #[must_use]
  pub fn get_by_label(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:label", text.into())
  }

  /// `getByPlaceholder` in this frame.
  #[must_use]
  pub fn get_by_placeholder(
    &self,
    text: impl Into<StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("placeholder", text.into())
  }

  /// Locate elements by `alt` attribute. Mirrors Playwright's
  /// `frame.getByAltText(text, options?)`.
  #[must_use]
  pub fn get_by_alt_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("alt", text.into())
  }

  /// Locate elements by `title` attribute. Mirrors Playwright's
  /// `frame.getByTitle(text, options?)`.
  #[must_use]
  pub fn get_by_title(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("title", text.into())
  }

  fn text_like_builder(
    &self,
    kind: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let frame = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(
        frame.clone(),
        crate::locator::build_text_like_selector(kind, &text, opts),
      )
    })
  }

  fn attr_builder(
    &self,
    attr: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let frame = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(frame.clone(), crate::locator::build_attr_selector(attr, &text, opts))
    })
  }

  /// Playwright: `frame.frameLocator(selector?: string): FrameLocator`.
  /// With a selector, the `<iframe>` it matches in this frame's document;
  /// without one, whichever frame in this frame's subtree (this one
  /// included) the eventual locator matches in.
  #[must_use]
  pub fn frame_locator<'a>(&self, selector: impl Into<Option<&'a str>>) -> crate::locator::FrameLocator {
    let selector = selector.into().unwrap_or(crate::locator::ANY_FRAME);
    crate::locator::FrameLocator::for_iframe_in(self.clone(), selector.to_string())
  }

  /// Playwright: `frame.$(selector, options?: { strict?: boolean }):
  /// Promise<ElementHandle | null>`. The first element in this frame the
  /// selector matches right now, or `None`. Without `strict`, the
  /// context's `strictSelectors` decides whether a second match is an
  /// error.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse, matches more than
  /// one element under strict mode, or the query fails.
  pub async fn query_selector(
    &self,
    selector: &str,
    strict: Option<bool>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    self
      .page
      .traced_as(
        "Frame",
        "$",
        serde_json::json!({ "selector": selector }),
        self.query_selector_untraced(selector, strict),
      )
      .await
  }

  pub(crate) async fn query_selector_untraced(
    &self,
    selector: &str,
    strict: Option<bool>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    // Parsed here so that a selector that cannot be parsed is an error and
    // not, like one that matches nothing, an absent element.
    crate::selectors::parse(selector)?;
    let locator = match strict {
      Some(strict) => self.locator(selector).strict(strict),
      None => self.selector_locator(selector),
    };
    match locator.element_handle().await {
      Ok(handle) => Ok(Some(handle)),
      Err(crate::error::FerriError::ElementNotFound { .. }) => Ok(None),
      Err(error) => Err(error),
    }
  }

  /// Playwright: `frame.$$(selector): Promise<ElementHandle[]>`. Every
  /// element in this frame the selector matches, in document order.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse or the query fails.
  pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<crate::element_handle::ElementHandle>> {
    self
      .page
      .traced_as(
        "Frame",
        "$$",
        serde_json::json!({ "selector": selector }),
        self.locator(selector).element_handles(),
      )
      .await
  }

  /// Playwright: `frame.frameElement(): Promise<ElementHandle>`. The
  /// `<iframe>` or `<frame>` in the parent frame that holds this one.
  ///
  /// # Errors
  ///
  /// Returns an error for the main frame, which no element holds, and for
  /// a frame whose element has left its parent's document.
  pub async fn frame_element(&self) -> Result<crate::element_handle::ElementHandle> {
    let detached = || crate::error::FerriError::backend("Frame has been detached.");
    let parent = self.parent_frame().ok_or_else(detached)?;
    let mut owner = None;
    for candidate in parent.locator("iframe, frame").element_handles().await? {
      let holds_this = owner.is_none()
        && candidate
          .content_frame()
          .await?
          .is_some_and(|frame| frame.id == self.id);
      if holds_this {
        owner = Some(candidate);
      } else {
        let _ = candidate.dispose().await;
      }
    }
    owner.ok_or_else(detached)
  }

  /// Show an element highlight for `selector` in this frame, beside any
  /// already shown. Internal API mirroring Playwright's
  /// `frame._highlight(selector, style)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse or JS evaluation fails.
  pub async fn highlight(&self, selector: &str, style: Option<&str>) -> Result<()> {
    self
      .page
      .add_highlight(Arc::clone(&self.id), selector, style.map(str::to_string))
      .await
  }

  /// Stop highlighting `selector` in this frame. Internal API mirroring
  /// Playwright's `frame._hideHighlight(selector)`.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn hide_highlight(&self, selector: &str) -> Result<()> {
    self.page.remove_highlight(&self.id, selector).await
  }

  /// Whether this frame has been detached from the page. Sync -- reads
  /// the cached `detached` flag maintained by the page's frame event
  /// listener. Playwright:
  /// [`frame.isDetached()`](https://playwright.dev/docs/api/class-frame#frame-is-detached).
  #[must_use]
  pub fn is_detached(&self) -> bool {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).is_none_or(|r| r.detached))
  }

  /// Get the page this frame belongs to.
  #[must_use]
  pub fn page(&self) -> &Page {
    &self.page
  }

  /// Reference to the owning `Arc<Page>`. Locators hold a `Frame` and
  /// reach the backend through `frame.page_arc()`.
  #[must_use]
  pub fn page_arc(&self) -> &Arc<Page> {
    &self.page
  }

  /// Backend frame id (CDP/BiDi). Stable through navigations; used to
  /// scope evaluation to this frame's execution context.
  #[must_use]
  pub fn id(&self) -> &Arc<str> {
    &self.id
  }
}

impl std::fmt::Debug for Frame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let (name, url, main) = self.page.with_frame_cache(|c| {
      let rec = c.record(&self.id);
      (
        rec.map(|r| r.info.name.clone()),
        rec.map(|r| r.info.url.clone()),
        c.main_frame_id() == self.id,
      )
    });
    f.debug_struct("Frame")
      .field("id", &self.id)
      .field("name", &name)
      .field("url", &url)
      .field("main", &main)
      .finish_non_exhaustive()
  }
}
