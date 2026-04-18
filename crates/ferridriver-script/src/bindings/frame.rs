//! `FrameJs`: JS wrapper around `ferridriver::Frame`.
//!
//! Mirrors Playwright's
//! [Frame](https://playwright.dev/docs/api/class-frame) sync navigation
//! tree API — `name()`, `url()`, `isMainFrame()`, `parentFrame()`,
//! `childFrames()`, `isDetached()` — plus the small set of async
//! accessors needed for writing scripts that deal with iframes
//! (evaluate / title / content, locator).
//!
//! Action methods (`click`, `fill`, `hover`, etc.) and the full
//! getBy* option surface ship in **task 3.9** (Frame action methods).
//!
//! The underlying `ferridriver::Frame` is a cheap `(Arc<Page>, Arc<str>)`
//! handle — cloning it is free. All name/url/parent/children/detached
//! state is read live from the page-owned frame cache (see
//! `crate::frame_cache::FrameCache`) seeded at `Page::init_frame_cache`.

use ferridriver::Frame;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;
use crate::bindings::locator::LocatorJs;

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
}

#[rquickjs::methods]
impl FrameJs {
  // ── Sync frame-tree accessors (Playwright parity, task 3.8) ────────

  /// Frame name (from the `<iframe name=...>` attribute). Sync.
  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name()
  }

  /// Frame URL. Sync.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// True when this is the top-level page frame. Sync.
  #[qjs(rename = "isMainFrame")]
  pub fn is_main_frame(&self) -> bool {
    self.inner.is_main_frame()
  }

  /// Parent frame (null for the main frame). Sync.
  #[qjs(rename = "parentFrame")]
  pub fn parent_frame(&self) -> Option<FrameJs> {
    self.inner.parent_frame().map(FrameJs::new)
  }

  /// Child frames of this frame. Sync.
  #[qjs(rename = "childFrames")]
  pub fn child_frames(&self) -> Vec<FrameJs> {
    self.inner.child_frames().into_iter().map(FrameJs::new).collect()
  }

  /// Whether this frame has been detached from the page. Sync.
  #[qjs(rename = "isDetached")]
  pub fn is_detached(&self) -> bool {
    self.inner.is_detached()
  }

  // ── Evaluation (frame-scoped) ──────────────────────────────────────

  #[qjs(rename = "evaluate")]
  pub async fn evaluate(&self, expression: String) -> rquickjs::Result<Option<String>> {
    self
      .inner
      .evaluate(&expression)
      .await
      .map(|opt| opt.map(|v| v.to_string()))
      .into_js()
  }

  #[qjs(rename = "evaluateStr")]
  pub async fn evaluate_str(&self, expression: String) -> rquickjs::Result<String> {
    self.inner.evaluate_str(&expression).await.into_js()
  }

  #[qjs(rename = "title")]
  pub async fn title(&self) -> rquickjs::Result<String> {
    self.inner.title().await.into_js()
  }

  #[qjs(rename = "content")]
  pub async fn content(&self) -> rquickjs::Result<String> {
    self.inner.content().await.into_js()
  }

  // ── Locator (frame-scoped) ─────────────────────────────────────────

  /// Create a locator scoped to this frame. Options come in **3.9**.
  #[qjs(rename = "locator")]
  pub fn locator(&self, selector: String) -> LocatorJs {
    LocatorJs::new(self.inner.locator(&selector, None))
  }
}
