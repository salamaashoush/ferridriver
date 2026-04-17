//! Frame API -- mirrors Playwright's Frame interface.
//!
//! A Frame represents an execution context within a Page.
//! The main frame is the top-level page frame. Child frames
//! correspond to `<iframe>` elements.
//!
//! Frame has the same evaluation and locator methods as Page,
//! but scoped to its specific frame context.

use std::sync::Arc;

use crate::backend::FrameInfo;
use crate::error::Result;
use crate::locator::Locator;
use crate::options::{RoleOptions, TextOptions, WaitOptions};
use crate::page::Page;

/// A frame within a page. Mirrors Playwright's Frame interface.
#[derive(Clone)]
pub struct Frame {
  /// The page this frame belongs to (Arc for cheap cloning in locator chains).
  page: Arc<Page>,
  /// Frame ID (from CDP or backend). `Arc<str>` so locator chains are cheap.
  pub(crate) id: Arc<str>,
  /// Parent frame ID (None for main frame).
  pub(crate) parent_id: Option<String>,
  /// Frame name (from `<iframe name="...">` attribute).
  name_str: String,
  /// Frame URL.
  url_str: String,
}

impl Frame {
  /// Create a frame from backend `FrameInfo`.
  pub(crate) fn from_info(page: Arc<Page>, info: FrameInfo) -> Self {
    Self {
      page,
      id: Arc::from(info.frame_id),
      parent_id: info.parent_frame_id,
      name_str: info.name,
      url_str: info.url,
    }
  }

  /// Frame name (from the `name` attribute of the iframe element).
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name_str
  }

  /// Frame URL.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.url_str
  }

  /// Whether this is the main (top-level) frame.
  #[must_use]
  pub fn is_main_frame(&self) -> bool {
    self.parent_id.is_none()
  }

  /// Get the parent frame. Returns None for the main frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved.
  pub async fn parent_frame(&self) -> Result<Option<Frame>> {
    if let Some(pid) = &self.parent_id {
      let frames = self.page.frames().await?;
      Ok(frames.into_iter().find(|f| &*f.id == pid.as_str()))
    } else {
      Ok(None)
    }
  }

  /// Get child frames of this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved.
  pub async fn child_frames(&self) -> Result<Vec<Frame>> {
    let frames = self.page.frames().await?;
    Ok(
      frames
        .into_iter()
        .filter(|f| f.parent_id.as_deref() == Some(&*self.id))
        .collect(),
    )
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

  /// Check if this frame has been detached from the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved.
  pub async fn is_detached(&self) -> Result<bool> {
    let frames = self.page.inner().get_frame_tree().await?;
    Ok(!frames.iter().any(|f| f.frame_id.as_str() == &*self.id))
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
    f.debug_struct("Frame")
      .field("id", &self.id)
      .field("parent_id", &self.parent_id)
      .field("name", &self.name_str)
      .field("url", &self.url_str)
      .finish_non_exhaustive()
  }
}
