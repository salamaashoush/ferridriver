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
    let base = Locator {
      page: Arc::clone(&self.page),
      selector: selector.to_string(),
      frame_id: Some(self.id.clone()),
      strict: true,
    };
    match options {
      Some(opts) => base.filter(&opts),
      None => base,
    }
  }

  #[must_use]
  pub fn get_by_role(&self, role: &str, opts: &RoleOptions) -> Locator {
    let sel = crate::locator::build_role_selector(role, opts);
    Locator {
      page: Arc::clone(&self.page),
      selector: sel,
      frame_id: Some(self.id.clone()),
      strict: true,
    }
  }

  #[must_use]
  pub fn get_by_text(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("text=\"{text}\"")
    } else {
      format!("text={text}")
    };
    Locator {
      page: Arc::clone(&self.page),
      selector: sel,
      frame_id: Some(self.id.clone()),
      strict: true,
    }
  }

  #[must_use]
  pub fn get_by_test_id(&self, test_id: &str) -> Locator {
    Locator::new(
      Arc::clone(&self.page),
      format!("testid={test_id}"),
      Some(self.id.clone()),
    )
  }

  #[must_use]
  pub fn get_by_label(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("label=\"{text}\"")
    } else {
      format!("label={text}")
    };
    Locator {
      page: Arc::clone(&self.page),
      selector: sel,
      frame_id: Some(self.id.clone()),
      strict: true,
    }
  }

  #[must_use]
  pub fn get_by_placeholder(&self, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("placeholder=\"{text}\"")
    } else {
      format!("placeholder={text}")
    };
    Locator {
      page: Arc::clone(&self.page),
      selector: sel,
      frame_id: Some(self.id.clone()),
      strict: true,
    }
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
