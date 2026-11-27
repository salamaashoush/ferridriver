//! Interactive locator picker, highlight overlays, and inspector pause on [`Page`].

use std::sync::Arc;

use crate::error::Result;
use crate::frame_cache::FrameCache;
use crate::locator::Locator;

use super::{HighlightEntry, Page};

impl Page {
  /// Open the interactive locator picker: highlight elements under the
  /// cursor and resolve with a [`Locator`] for the element the user clicks.
  ///
  /// Mirrors Playwright's `page.pickLocator(): Promise<Locator>`. The picker
  /// generates a selector for the clicked element using the same engine that
  /// backs `codegen`/recording, then returns `page.locator(selector)`.
  ///
  /// Cancel an in-progress pick with [`Page::cancel_pick_locator`].
  ///
  /// # Errors
  ///
  /// Returns an error if the picker scaffolding cannot be injected, the page
  /// closes before a selection is made, or the returned selector is empty.
  pub async fn pick_locator(self: &Arc<Self>) -> Result<Locator> {
    self.inner.ensure_engine_injected().await?;
    self.inner.evaluate(crate::selectors::RECORDER_SUPPORT_JS).await?;
    self.inner.evaluate(Self::PICKER_JS).await?;

    // Poll the page-side global for the picked selector. Playwright's
    // `pickLocator` waits indefinitely for the user to click; we mirror
    // that (no timeout) while honoring cancellation: when the picker is
    // torn down via `cancel_pick_locator`/`hide_highlight` without a
    // selection, `__fdPicker` flips to `false` and we surface a cancelled
    // error. Polling (rather than a cross-task exposed-function callback)
    // keeps engine teardown race-free on the QuickJS host.
    loop {
      let probe = self
        .inner
        .evaluate(
          "JSON.stringify({ \
             selector: (typeof window.__fdPickedSelector === 'string') ? window.__fdPickedSelector : null, \
             active: window.__fdPicker === true })",
        )
        .await?;
      let parsed: serde_json::Value = match probe {
        Some(serde_json::Value::String(s)) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        Some(v) => v,
        None => serde_json::Value::Null,
      };
      if let Some(sel) = parsed.get("selector").and_then(serde_json::Value::as_str) {
        return Ok(self.locator(sel));
      }
      if !parsed
        .get("active")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        return Err(crate::error::FerriError::interrupted(
          "pickLocator: cancelled before a selection was made",
        ));
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  /// Cancel an in-progress [`Page::pick_locator`] and hide its highlight.
  ///
  /// Mirrors Playwright's `page.cancelPickLocator()`. Any pending
  /// `pick_locator` future resolves with a `page closed`-style error once the
  /// page-side picker is torn down (the exposed callback is removed, so the
  /// oneshot sender drops).
  ///
  /// # Errors
  ///
  /// Returns an error if the page-side teardown evaluation fails.
  pub async fn cancel_pick_locator(&self) -> Result<()> {
    self
      .inner
      .evaluate("(function(){ if (window.__fdPickerCancel) window.__fdPickerCancel(); })()")
      .await?;
    let _ = self.inner.remove_exposed_function("__fdPickLocator").await;
    Ok(())
  }

  /// Hide any highlight overlay currently shown by the picker or by
  /// `highlight`-style helpers.
  ///
  /// Mirrors Playwright's `page.hideHighlight()`.
  ///
  /// # Errors
  ///
  /// Returns an error if the page-side teardown evaluation fails.
  pub async fn hide_highlight(&self) -> Result<()> {
    self.lock_highlights().clear();
    let ids: Vec<Arc<str>> = self.with_frame_cache(|c| c.live_ids().collect());
    let main = self.with_frame_cache(FrameCache::main_frame_id);
    for id in ids.iter().filter(|id| **id != main) {
      let _ = self
        .inner
        .evaluate_in_frame("window.__fd && window.__fd.hideHighlight()", id)
        .await;
    }
    self
      .inner
      .evaluate(
        "(function(){ if (window.__fd && window.__fd.hideHighlight) window.__fd.hideHighlight(); \
         if (window.__fdPickerCancel) window.__fdPickerCancel(); })()",
      )
      .await?;
    Ok(())
  }

  fn lock_highlights(&self) -> std::sync::MutexGuard<'_, Vec<HighlightEntry>> {
    self
      .highlights
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// `HighlightController.addHighlight`.
  pub(crate) async fn add_highlight(
    self: &Arc<Self>,
    frame_id: Arc<str>,
    selector: &str,
    style: Option<String>,
  ) -> Result<()> {
    crate::selectors::parse(selector)?;
    {
      let mut entries = self.lock_highlights();
      entries.retain(|e| !(e.frame_id == frame_id && e.selector == selector));
      entries.push(HighlightEntry {
        frame_id,
        selector: selector.to_string(),
        style,
      });
    }
    self.apply_highlights().await?;
    self.keep_highlights_applied();
    Ok(())
  }

  /// `HighlightController.removeHighlight`.
  pub(crate) async fn remove_highlight(&self, frame_id: &str, selector: &str) -> Result<()> {
    self
      .lock_highlights()
      .retain(|e| !(&*e.frame_id == frame_id && e.selector == selector));
    self.apply_highlights().await
  }

  /// Give every live frame exactly its own highlights.
  async fn apply_highlights(&self) -> Result<()> {
    let entries = self.lock_highlights().clone();
    let main = self.with_frame_cache(FrameCache::main_frame_id);
    let ids: Vec<Arc<str>> = self.with_frame_cache(|c| c.live_ids().collect());
    let mut first_error = None;
    for id in ids {
      let mine: Vec<(String, Option<String>)> = entries
        .iter()
        .filter(|e| e.frame_id == id)
        .map(|e| (e.selector.clone(), e.style.clone()))
        .collect();
      let frame_id = (main != id).then_some(&*id);
      if let Err(e) = crate::selectors::set_highlights(&self.inner, &mine, frame_id).await
        && !mine.is_empty()
      {
        first_error.get_or_insert(e);
      }
    }
    first_error.map_or(Ok(()), Err)
  }

  /// The one-second re-apply loop, started by the first highlight and
  /// ended once none are left or the page closes.
  fn keep_highlights_applied(self: &Arc<Self>) {
    if self.highlight_timer.swap(true, std::sync::atomic::Ordering::AcqRel) {
      return;
    }
    let page = Arc::downgrade(self);
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Some(page) = page.upgrade() else { return };
        if page.is_closed() || page.lock_highlights().is_empty() {
          page.highlight_timer.store(false, std::sync::atomic::Ordering::Release);
          return;
        }
        let _ = page.apply_highlights().await;
      }
    });
  }

  /// Playwright: `page.pause(): Promise<void>` (`client/page.ts:858`).
  /// Playwright suspends script execution and hands control to the
  /// Inspector/recorder UI. ferridriver has no recorder, so this is a
  /// typed [`crate::error::FerriError::Unsupported`] rather than a
  /// silent no-op that would mislead callers into thinking a breakpoint
  /// was honoured.
  ///
  /// # Errors
  ///
  /// Always returns [`crate::error::FerriError::Unsupported`].
  pub fn pause(&self) -> Result<()> {
    Err(crate::error::FerriError::Unsupported(format!(
      "page.pause() (page at {}) requires the Playwright Inspector/recorder UI, which ferridriver does not provide",
      self.url()
    )))
  }
}
