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
use crate::options::{RoleOptions, TextOptions, WaitOptions};
use crate::page::Page;

/// A frame within a page. Mirrors Playwright's
/// [Frame interface](https://playwright.dev/docs/api/class-frame).
///
/// Frame instances are thin handles — the authoritative name/url/parent
/// state lives in [`crate::frame_cache::FrameCache`] on the owning Page.
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
  /// Create a frame handle pointing at an id present in the page's
  /// frame cache. The cache is the source of truth for name/url/parent.
  pub(crate) fn new(page: Arc<Page>, id: Arc<str>) -> Self {
    Self { page, id }
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
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).map(|r| r.info.url.clone()).unwrap_or_default())
  }

  /// Whether this is the main (top-level) frame. Mirrors Playwright's
  /// equivalent of `frame.parentFrame() === null`.
  #[must_use]
  pub fn is_main_frame(&self) -> bool {
    self
      .page
      .with_frame_cache(|c| c.main_frame_id().as_deref() == Some(&*self.id))
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

  // ── Evaluation (frame-scoped) ────────────────────────────────────────

  /// Evaluate JavaScript in this frame's context.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    if self.is_main_frame() {
      self.page.evaluate(expression).await
    } else {
      self
        .page
        .inner
        .evaluate_in_frame(expression, &self.id)
        .await
        .map_err(Into::into)
    }
  }

  /// Evaluate JS and return as string.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn evaluate_str(&self, expression: &str) -> Result<String> {
    self.evaluate(expression).await.map(|v| {
      v.map(|val| {
        if let Some(s) = val.as_str() {
          s.to_string()
        } else {
          val.to_string()
        }
      })
      .unwrap_or_default()
    })
  }

  // ── Locators (frame-scoped) ──────────────────────────────────────────

  /// Create a locator scoped to this frame.
  ///
  /// Playwright: `frame.locator(selector, options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:324`).
  /// Frame-level `.locator` only accepts a selector string and honors
  /// the full `LocatorOptions` bag (including `visible`).
  #[must_use]
  pub fn locator(&self, selector: &str, options: Option<crate::options::FilterOptions>) -> Locator {
    let base = Locator::new(self.clone(), selector.to_string());
    match options {
      Some(opts) => base.filter(&opts),
      None => base,
    }
  }

  #[must_use]
  pub fn get_by_role(&self, role: &str, opts: &RoleOptions) -> Locator {
    Locator::new(self.clone(), crate::locator::build_role_selector(role, opts))
  }

  #[must_use]
  pub fn get_by_text(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("text=\"{text}\"")
    } else {
      format!("text={text}")
    };
    Locator::new(self.clone(), sel)
  }

  #[must_use]
  pub fn get_by_test_id(&self, test_id: &str) -> Locator {
    Locator::new(self.clone(), format!("testid={test_id}"))
  }

  #[must_use]
  pub fn get_by_label(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("label=\"{text}\"")
    } else {
      format!("label={text}")
    };
    Locator::new(self.clone(), sel)
  }

  #[must_use]
  pub fn get_by_placeholder(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("placeholder=\"{text}\"")
    } else {
      format!("placeholder={text}")
    };
    Locator::new(self.clone(), sel)
  }

  /// Locate elements by `alt` attribute. Mirrors Playwright's
  /// `frame.getByAltText(text, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts`).
  #[must_use]
  pub fn get_by_alt_text(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("alt=\"{text}\"")
    } else {
      format!("alt={text}")
    };
    Locator::new(self.clone(), sel)
  }

  /// Locate elements by `title` attribute. Mirrors Playwright's
  /// `frame.getByTitle(text, options?)`.
  #[must_use]
  pub fn get_by_title(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("title=\"{text}\"")
    } else {
      format!("title={text}")
    };
    Locator::new(self.clone(), sel)
  }

  /// Create a `FrameLocator` for an `<iframe>` matching `selector`
  /// inside this frame's document. Mirrors Playwright's
  /// `frame.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> crate::locator::FrameLocator {
    crate::locator::FrameLocator::for_iframe_in(self.clone(), selector.to_string())
  }

  // ── Content (frame-scoped) ───────────────────────────────────────────

  /// Get the frame's HTML content.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn content(&self) -> Result<String> {
    let r = self.evaluate("document.documentElement.outerHTML").await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  /// Get the frame's title.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn title(&self) -> Result<String> {
    let r = self.evaluate("document.title").await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  // ── Navigation (frame-scoped) ────────────────────────────────────────

  /// Navigate this frame to a URL.
  ///
  /// # Errors
  ///
  /// Returns an error if navigation fails.
  pub async fn goto(&self, url: &str) -> Result<()> {
    if self.is_main_frame() {
      self.page.goto(url, None).await
    } else {
      // For child frames, set location via JS
      self
        .evaluate(&format!("window.location.href = '{}'", url.replace('\'', "\\'")))
        .await?;
      Ok(())
    }
  }

  // ── Waiting ──────────────────────────────────────────────────────────

  /// Wait for a selector within this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found within the timeout.
  pub async fn wait_for_selector(&self, selector: &str, opts: WaitOptions) -> Result<()> {
    self.locator(selector, None).wait_for(opts).await
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

  /// Set the HTML content of this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    let escaped = crate::steps::js_escape(html);
    self
      .evaluate(&format!("document.documentElement.innerHTML = '{escaped}'"))
      .await?;
    Ok(())
  }

  /// Add a `<script>` tag to this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if script injection fails.
  pub async fn add_script_tag(
    &self,
    url: Option<&str>,
    content: Option<&str>,
    script_type: Option<&str>,
  ) -> Result<()> {
    let t = script_type.unwrap_or("text/javascript");
    if let Some(url) = url {
      self.evaluate(&format!(
                "(function(){{return new Promise(function(r,j){{var s=document.createElement('script');\
                 s.type='{}';s.src='{}';s.onload=r;s.onerror=function(){{j(new Error('Failed'))}};document.head.appendChild(s)}})}})();",
                crate::steps::js_escape(t), crate::steps::js_escape(url)
            )).await?;
    } else if let Some(content) = content {
      self.evaluate(&format!(
                "(function(){{var s=document.createElement('script');s.type='{}';s.text='{}';document.head.appendChild(s)}})()",
                crate::steps::js_escape(t), crate::steps::js_escape(content)
            )).await?;
    }
    Ok(())
  }

  /// Add a `<style>` tag or `<link>` stylesheet to this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if style injection fails.
  pub async fn add_style_tag(&self, url: Option<&str>, content: Option<&str>) -> Result<()> {
    if let Some(url) = url {
      self.evaluate(&format!(
                "(function(){{return new Promise(function(r,j){{var l=document.createElement('link');\
                 l.rel='stylesheet';l.href='{}';l.onload=r;l.onerror=function(){{j(new Error('Failed'))}};document.head.appendChild(l)}})}})();",
                crate::steps::js_escape(url)
            )).await?;
    } else if let Some(content) = content {
      self
        .evaluate(&format!(
          "(function(){{var s=document.createElement('style');s.textContent='{}';document.head.appendChild(s)}})()",
          crate::steps::js_escape(content)
        ))
        .await?;
    }
    Ok(())
  }

  /// Wait for the frame to reach a specific load state.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame does not reach load state within the timeout.
  pub async fn wait_for_load_state(&self) -> Result<()> {
    if self.is_main_frame() {
      self.page.wait_for_load_state(None).await
    } else {
      // For iframes, check document.readyState via JS
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
      loop {
        if tokio::time::Instant::now() >= deadline {
          return Err("Timeout waiting for frame load state".into());
        }
        if let Ok(Some(v)) = self.evaluate("document.readyState").await {
          if v.as_str() == Some("complete") {
            return Ok(());
          }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
      }
    }
  }

  // ── Action methods (Playwright parity — task 3.9) ──────────────────────
  //
  // Mirrors Playwright's frame action surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each method delegates to `self.locator(selector, None).<action>()` —
  // Frame's locator already scopes by `frame_id`, so the action runs in
  // the iframe's execution context (CDP) or against the synthesized
  // iframe (WebKit). Option bags are intentionally minimal here; they
  // ride on top of the existing Locator surface and pick up extensions
  // (timeout/force/etc.) when those land on Locator itself.

  // -- Mouse / pointer ---------------------------------------------------

  /// Click the element matched by `selector`. Accepts Playwright's full
  /// `FrameClickOptions` bag (see [`crate::options::ClickOptions`]).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  pub async fn click(&self, selector: &str, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    self.locator(selector, None).click(opts).await
  }

  /// Double-click the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dblclick fails.
  pub async fn dblclick(&self, selector: &str) -> Result<()> {
    self.locator(selector, None).dblclick().await
  }

  /// Hover the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  pub async fn hover(&self, selector: &str) -> Result<()> {
    self.locator(selector, None).hover().await
  }

  /// Tap (touch) the element matched by `selector`. Mirrors
  /// `frame.tap(selector, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:308`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the tap fails.
  pub async fn tap(&self, selector: &str) -> Result<()> {
    self.locator(selector, None).tap().await
  }

  /// Focus the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or focus fails.
  pub async fn focus(&self, selector: &str) -> Result<()> {
    self.locator(selector, None).focus().await
  }

  // -- Form input --------------------------------------------------------

  /// Fill an input matching `selector` with `value`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  pub async fn fill(&self, selector: &str, value: &str) -> Result<()> {
    self.locator(selector, None).fill(value).await
  }

  /// Type characters into an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  pub async fn r#type(&self, selector: &str, text: &str) -> Result<()> {
    self.locator(selector, None).r#type(text).await
  }

  /// Press a key on an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  pub async fn press(&self, selector: &str, key: &str) -> Result<()> {
    self.locator(selector, None).press(key).await
  }

  /// Check a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub async fn check(&self, selector: &str) -> Result<()> {
    self.locator(selector, None).check().await
  }

  /// Uncheck a checkbox matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  pub async fn uncheck(&self, selector: &str) -> Result<()> {
    self.locator(selector, None).uncheck().await
  }

  /// Set the checked state of a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub async fn set_checked(&self, selector: &str, checked: bool) -> Result<()> {
    self.locator(selector, None).set_checked(checked).await
  }

  /// Select a `<select>` option in the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot
  /// be selected.
  pub async fn select_option(&self, selector: &str, value: &str) -> Result<Vec<String>> {
    self.locator(selector, None).select_option(value).await
  }

  /// Set input files on a `<input type=file>` matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  pub async fn set_input_files(&self, selector: &str, paths: &[String]) -> Result<()> {
    self.locator(selector, None).set_input_files(paths).await
  }

  // -- Drag and drop -----------------------------------------------------

  /// Drag from `source` to `target` selectors within this frame. Mirrors
  /// `frame.dragAndDrop(source, target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:304`.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found or the
  /// drag-and-drop operation fails.
  pub async fn drag_and_drop(
    &self,
    source: &str,
    target: &str,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let src = self.locator(source, None);
    let tgt = self.locator(target, None);
    let (src, tgt) = match opts.strict {
      Some(s) => (src.strict(s), tgt.strict(s)),
      None => (src, tgt),
    };
    src.drag_to(&tgt, Some(opts)).await
  }

  // -- Synthetic events --------------------------------------------------

  /// Dispatch a DOM event on the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dispatch fails.
  pub async fn dispatch_event(&self, selector: &str, event_type: &str) -> Result<()> {
    self.locator(selector, None).dispatch_event(event_type).await
  }

  // -- Content / attribute reads ----------------------------------------

  /// Get the text content of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn text_content(&self, selector: &str) -> Result<Option<String>> {
    self.locator(selector, None).text_content().await
  }

  /// Get `innerText` of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_text(&self, selector: &str) -> Result<String> {
    self.locator(selector, None).inner_text().await
  }

  /// Get `innerHTML` of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_html(&self, selector: &str) -> Result<String> {
    self.locator(selector, None).inner_html().await
  }

  /// Get an attribute on the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn get_attribute(&self, selector: &str, name: &str) -> Result<Option<String>> {
    self.locator(selector, None).get_attribute(name).await
  }

  /// Get `value` from a form control matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn input_value(&self, selector: &str) -> Result<String> {
    self.locator(selector, None).input_value().await
  }

  // -- State checks ------------------------------------------------------

  /// True if the element matched by `selector` is visible.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_visible(&self, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_visible().await
  }

  /// True if the element matched by `selector` is hidden.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_hidden(&self, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_hidden().await
  }

  /// True if the element matched by `selector` is enabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_enabled(&self, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_enabled().await
  }

  /// True if the element matched by `selector` is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_disabled(&self, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_disabled().await
  }

  /// True if the element matched by `selector` is editable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_editable(&self, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_editable().await
  }

  /// True if a checkbox/radio matched by `selector` is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_checked(&self, selector: &str) -> Result<bool> {
    self.locator(selector, None).is_checked().await
  }
}

impl std::fmt::Debug for Frame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let (name, url, main) = self.page.with_frame_cache(|c| {
      let rec = c.record(&self.id);
      (
        rec.map(|r| r.info.name.clone()),
        rec.map(|r| r.info.url.clone()),
        c.main_frame_id().as_deref() == Some(&*self.id),
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
