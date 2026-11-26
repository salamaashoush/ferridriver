//! `CdpPage` navigation, lifecycle waits, and history traversal.

use super::*;

impl CdpPage {
  // ---- Navigation ----

  /// Map an [`NavLifecycle`] to the [`LifecycleState::fired`] key the
  /// dispatcher writes for it. Centralised so `goto` / `reload` /
  /// `history_go` / `wait_for_navigation` all read the same vocabulary
  /// the transport produces.
  fn lifecycle_key(lifecycle: crate::backend::NavLifecycle) -> &'static str {
    match lifecycle {
      crate::backend::NavLifecycle::Commit => "commit",
      crate::backend::NavLifecycle::DomContentLoaded => "domcontentloaded",
      crate::backend::NavLifecycle::Load => "load",
    }
  }

  fn lifecycle_fired(fired: u8, target_event: &str) -> bool {
    let flag = match target_event {
      "commit" => LC_COMMIT,
      "domcontentloaded" => LC_DOMCONTENTLOADED,
      "load" => LC_LOAD,
      _ => 0,
    };
    flag != 0 && fired & flag != 0
  }

  /// The navigation timeout guards the WHOLE navigation, `Page.navigate`
  /// included.
  ///
  /// Playwright runs `Frame.goto` inside one progress controller whose
  /// deadline covers the protocol call as well as the lifecycle wait
  /// (`server/frames.ts::goto`). Timing only the lifecycle wait, as this
  /// did, means a server that never answers the navigation request
  /// leaves `page.goto` blocked past its own timeout — the request has
  /// not committed a document yet, so the lifecycle wait has not
  /// started.
  ///
  /// # Errors
  ///
  /// Returns a timeout error when the navigation does not reach `lifecycle` within `timeout_ms` (0 waits
  /// without limit), and an error if `Page.navigate` fails or reports a navigation error.
  pub async fn goto(
    &self,
    url: &str,
    lifecycle: crate::backend::NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<Response>> {
    if timeout_ms == 0 {
      // Playwright parity: 0 means no timeout.
      return self.goto_inner(url, lifecycle, timeout_ms, referer).await;
    }
    match tokio::time::timeout_at(
      crate::backend::deadline_for(timeout_ms),
      self.goto_inner(url, lifecycle, timeout_ms, referer),
    )
    .await
    {
      Ok(result) => result,
      Err(_) => Err(FerriError::timeout(format!("navigating to {url}"), timeout_ms)),
    }
  }

  async fn goto_inner(
    &self,
    url: &str,
    lifecycle: crate::backend::NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<Response>> {
    // No `injected_script.reset()` here. The init script registered via
    // `Page.addScriptToEvaluateOnNewDocument` persists for the page's
    // lifetime, so every post-navigation document already runs the
    // self-guarded `window.__fd` IIFE. Resetting the manager triggers
    // a redundant `Page.addScriptToEvaluateOnNewDocument` RTT on every
    // `goto()` — this code used to pay it once per nav for nothing.
    let target_event = Self::lifecycle_key(lifecycle);

    // Clear any previous main-doc request slot so a same-document
    // navigation (no new request) resolves as "no response" per
    // Playwright's `Response | null` contract. The network listener
    // refills the slot when it observes the next navigation request.
    self.nav_request_slot.clear();

    // Send navigation command. Response includes loaderId for this navigation.
    // `referrer` mirrors the CDP `Page.navigate` param (note the CDP
    // spelling uses two r's — we keep Playwright's single-r public spelling
    // `referer` and translate here).
    let mut nav_params = serde_json::json!({ "url": url });
    if let Some(r) = referer {
      nav_params["referrer"] = serde_json::Value::String(r.to_string());
    }
    let (pre_loader_id, pre_same_document) = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      (state.current_loader_id.clone(), state.same_document_seq)
    };
    let nav_result = self.cmd("Page.navigate", nav_params).await?;

    if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str())
      && !error_text.is_empty()
    {
      return Err(FerriError::Backend(format!("Navigation failed: {error_text}")));
    }

    // Page.navigate returns the navigated frame's `frameId`. PERF_AUDIT
    // §M.4 — eagerly seed the lazy `main_frame_id` cache from this
    // response so the wrapper-level [`crate::Page`] frame cache can be
    // populated without paying a separate `Page.getFrameTree` RTT
    // (the bootstrap previously paid that RTT inside `enable_domains`;
    // dropping it shifted the cost into per-action paths if not seeded
    // here).
    if let Some(fid) = nav_result.get("frameId").and_then(|v| v.as_str()) {
      let _ = self.main_frame_id.set(fid.to_string());
    }

    let nav_loader_id = nav_result
      .get("loaderId")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();

    // Mirrors Playwright's `Frame.gotoImpl`
    // (`/tmp/playwright/packages/playwright-core/src/server/frames.ts:644`):
    // wait for the InternalNavigation whose `documentId === navigateResult.newDocumentId`,
    // then for the AddLifecycle event matching the wait target. Both
    // gates use the same loaderId, so a late `Page.loadEventFired`
    // from a previous document cannot prematurely satisfy a fresh
    // navigation (the failure mode that produced "Not attached to an
    // active page" when `Page.reload` was sent in the middle of a
    // cross-origin RFH swap).
    // `Page.navigate` answers with a loaderId only when it created a
    // document. A fragment jump has none: it commits in the document
    // already loaded, announces itself with `Page.navigatedWithinDocument`
    // and issues no request. Waiting on the empty loader matched nothing
    // and ran out the whole navigation timeout; the event is waited for
    // because it can trail the command's answer, and the URL with it.
    if nav_loader_id.is_empty() {
      self
        .await_history_commit(&pre_loader_id, pre_same_document, target_event, timeout_ms)
        .await?;
      return Ok(None);
    }
    self
      .await_loader_lifecycle(&nav_loader_id, target_event, timeout_ms)
      .await?;
    // An `about:` document never issues a request, so there is no response
    // to wait for. Waiting for its commit to reach the network consumer
    // instead cost up to the whole grace on a busy runner.
    if url.starts_with("about:") {
      return Ok(None);
    }
    Ok(self.await_nav_response(Some(&nav_loader_id)).await)
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation. Returns `None` for
  /// same-document navigations (no new request was issued) or when
  /// the underlying request ended in failure.
  ///
  /// `grace` is how long to wait for the document request to be seen:
  /// non-zero only when a new document committed, since the request and
  /// the lifecycle reach us on separate consumers and the request can be
  /// processed second. A same-document navigation never issues one, so
  /// waiting there would only slow it down.
  async fn await_nav_response(&self, loader_id: Option<&str>) -> Option<Response> {
    let Some(loader_id) = loader_id else {
      return self.nav_request_slot.final_response(std::time::Duration::ZERO).await;
    };
    self
      .nav_request_slot
      .final_response_for(Some(loader_id), crate::network::NAV_REQUEST_GRACE)
      .await
  }

  fn current_loader_id(&self) -> String {
    self
      .lifecycle
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .current_loader_id
      .clone()
  }

  /// # Errors
  ///
  /// Returns [`FerriError::TargetClosed`] if the target crashes while waiting. A document that does not reach
  /// `load` within 30 seconds ends the wait without an error.
  pub async fn wait_for_navigation(&self) -> Result<()> {
    let pre_loader_id = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.current_loader_id.clone()
    };
    self.await_loader_change(&pre_loader_id, "load", 30_000).await
  }

  /// Snapshot the navigation state before an input action (cheap, no RTT):
  /// `(frameStartedNavigating generation, frameNavigated/commit generation)`.
  #[must_use]
  pub fn nav_snapshot(&self) -> (u64, u64) {
    let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (state.nav_started_seq, state.nav_committed_seq)
  }

  /// Playwright `waitForSignalsCreatedBy`: if the just-completed action
  /// started a navigation (the `frameStartedNavigating` generation advanced
  /// vs `snap`), wait — bounded, best-effort — for the next commit
  /// (`frameNavigated`). Waiting on the commit *generation* (not a loaderId
  /// change) means a reload, which re-commits the same loaderId, also
  /// satisfies the barrier. No-op (zero cost) when the action started no
  /// navigation, which is the common case.
  pub async fn settle_navigation(&self, snap: (u64, u64), timeout_ms: u64) {
    let (pre_started, pre_committed) = snap;
    {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if state.nav_started_seq == pre_started {
        return; // no navigation started — zero cost.
      }
    }
    let deadline = crate::backend::deadline_for(timeout_ms);
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A commit landed after our snapshot, or the target crashed — done.
        if state.crashed || state.nav_committed_seq != pre_committed {
          return;
        }
      }
      // Bounded: a navigation that starts but never commits (cancelled /
      // no-op) costs at most `timeout_ms`, never a hang.
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return;
      }
    }
  }

  /// Force a GC pass in the page's JS engine. Mirrors Playwright's
  /// `crPage.requestGC` (`HeapProfiler.collectGarbage`).
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `HeapProfiler.collectGarbage`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn request_gc(&self) -> Result<()> {
    self.cmd("HeapProfiler.collectGarbage", serde_json::json!({})).await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.bringToFront`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn bring_to_front(&self) -> Result<()> {
    self.cmd("Page.bringToFront", serde_json::json!({})).await?;
    Ok(())
  }

  /// Bring the page to the front of a browser that draws only the tab
  /// in front, and return once it is drawn there. A tab asked to the
  /// front while another is still opening is overtaken by it, and
  /// nothing says so: its animation frames keep firing either way. A
  /// capture is what only the tab in front answers, so one pixel of one
  /// is the evidence, and the request is made again until it arrives.
  /// When it never does, the caller's capture reports its own timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.bringToFront` or `Page.captureScreenshot`, the target has
  /// closed, or the command outlives its time budget.
  pub async fn bring_to_front_drawn(&self) -> Result<()> {
    let pixel = serde_json::json!({
      "format": "png",
      "clip": { "x": 0, "y": 0, "width": 1, "height": 1, "scale": 1 },
    });
    for _ in 0..FRONT_TAB_ATTEMPTS {
      self.bring_to_front().await?;
      let drawn = self.cmd("Page.captureScreenshot", pixel.clone());
      if let Ok(answer) = tokio::time::timeout(FRONT_TAB_DRAWN, drawn).await {
        return answer.map(|_| ());
      }
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.reload`, the target has closed, or the command outlives its
  /// time budget. Also returns [`FerriError::TargetClosed`] if the target crashes before the new document
  /// reaches `lifecycle`.
  pub async fn reload(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    // See `goto`: the registered init script persists across reloads;
    // no need to reset and re-fire `Page.addScriptToEvaluateOnNewDocument`.
    self.nav_request_slot.clear();
    let target_event = Self::lifecycle_key(lifecycle);
    // Snapshot the pre-reload `current_loader_id`. The reload command
    // does not return the new loaderId, so we wait for any commit
    // whose loaderId differs from this snapshot (mirrors
    // Playwright's `Page.reload` calling `waitForNavigation(requiresNewDocument: true)`
    // — `/tmp/playwright/packages/playwright-core/src/server/page.ts:447`).
    let pre_loader_id = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.current_loader_id.clone()
    };
    self.cmd("Page.reload", super::super::empty_params()).await?;
    self
      .await_loader_change(&pre_loader_id, target_event, timeout_ms)
      .await?;
    let loader_id = self.current_loader_id();
    Ok(
      self
        .await_nav_response((loader_id != pre_loader_id).then_some(loader_id.as_str()))
        .await,
    )
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.getNavigationHistory` or `Page.navigateToHistoryEntry`, the
  /// target has closed, or the command outlives its time budget. Also returns [`FerriError::TargetClosed`] if
  /// the target crashes before the entry commits.
  pub async fn go_back(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_go(-1, lifecycle, timeout_ms).await
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.getNavigationHistory` or `Page.navigateToHistoryEntry`, the
  /// target has closed, or the command outlives its time budget. Also returns [`FerriError::TargetClosed`] if
  /// the target crashes before the entry commits.
  pub async fn go_forward(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_go(1, lifecycle, timeout_ms).await
  }

  async fn history_go(
    &self,
    delta: i32,
    lifecycle: crate::backend::NavLifecycle,
    timeout_ms: u64,
  ) -> Result<Option<Response>> {
    let hist = self
      .cmd("Page.getNavigationHistory", super::super::empty_params())
      .await?;
    let current_i64 = hist
      .get("currentIndex")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);
    let current = i32::try_from(current_i64).unwrap_or(i32::MAX);
    let target = current + delta;
    let entries = hist.get("entries").and_then(|v| v.as_array());
    let Some(entries) = entries else {
      return Ok(None);
    };
    let Ok(target_usize) = usize::try_from(target) else {
      return Ok(None);
    };
    if target_usize >= entries.len() {
      return Ok(None);
    }
    let entry_id = entries[target_usize]
      .get("id")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);

    self.nav_request_slot.clear();
    let target_event = Self::lifecycle_key(lifecycle);
    let (pre_loader_id, pre_same_document) = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      (state.current_loader_id.clone(), state.same_document_seq)
    };
    self
      .cmd("Page.navigateToHistoryEntry", serde_json::json!({"entryId": entry_id}))
      .await?;
    // Playwright answers a history step that stays in the document with a
    // null response as soon as it commits (`Frame._waitForNavigation`).
    if self
      .await_history_commit(&pre_loader_id, pre_same_document, target_event, timeout_ms)
      .await?
    {
      return Ok(None);
    }
    let loader_id = self.current_loader_id();
    Ok(
      self
        .await_nav_response((loader_id != pre_loader_id).then_some(loader_id.as_str()))
        .await,
    )
  }

  /// Wait until the page's main-frame [`LifecycleState`] has both
  /// committed the supplied `expected_loader_id` AND fired
  /// `target_event` for that loader.
  ///
  /// `Page.navigate` returns the navigation's `loaderId` synchronously
  /// (Chrome 90+), so [`Self::goto`] uses this strict gate. Returns
  /// permissively on timeout (`Ok(())`) to preserve the prior
  /// "navigate-returns-with-whatever-we-have" semantics on hostile
  /// pages; the caller's response wait surfaces real failures.
  async fn await_loader_lifecycle(&self, expected_loader_id: &str, target_event: &str, timeout_ms: u64) -> Result<()> {
    let deadline = crate::backend::deadline_for(timeout_ms);
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      // `Notified::enable` arms the future before the state check so
      // a `notify_waiters` racing between check and await is observed.
      // Without this the dispatcher could fire its notify, the check
      // could still see the pre-notify state, and the subsequent await
      // would miss the wake.
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.crashed {
          return Err(FerriError::target_closed(Some("target crashed".into())));
        }
        if state.current_loader_id == expected_loader_id && Self::lifecycle_fired(state.fired, target_event) {
          return Ok(());
        }
      }
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return Ok(());
      }
    }
  }

  /// Variant of [`Self::await_loader_lifecycle`] for navigations whose
  /// `loaderId` is unknown ahead of time (`Page.reload`,
  /// `Page.navigateToHistoryEntry`, generic `page.waitForNavigation`).
  /// Waits for the main frame's `current_loader_id` to change away
  /// from `pre_loader_id` AND fire `target_event` for the new loader.
  async fn await_loader_change(&self, pre_loader_id: &str, target_event: &str, timeout_ms: u64) -> Result<()> {
    let deadline = crate::backend::deadline_for(timeout_ms);
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.crashed {
          return Err(FerriError::target_closed(Some("target crashed".into())));
        }
        if state.current_loader_id != pre_loader_id
          && !state.current_loader_id.is_empty()
          && Self::lifecycle_fired(state.fired, target_event)
        {
          return Ok(());
        }
      }
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return Ok(());
      }
    }
  }

  /// [`Self::await_loader_change`] for a history step, which may land on
  /// an entry of the current document: that commits no loader and fires
  /// no lifecycle event, only `Page.navigatedWithinDocument` for the main
  /// frame. Returns `true` when the step stayed in the document, `false`
  /// when a new document committed and fired `target_event` (or on
  /// timeout, which stays permissive like `await_loader_change`).
  async fn await_history_commit(
    &self,
    pre_loader_id: &str,
    pre_same_document: u64,
    target_event: &str,
    timeout_ms: u64,
  ) -> Result<bool> {
    let deadline = crate::backend::deadline_for(timeout_ms);
    let main_frame = self.peek_main_frame_id();
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.crashed {
          return Err(FerriError::target_closed(Some("target crashed".into())));
        }
        if state.current_loader_id != pre_loader_id
          && !state.current_loader_id.is_empty()
          && Self::lifecycle_fired(state.fired, target_event)
        {
          return Ok(false);
        }
        if state.same_document_seq != pre_same_document
          && state.current_loader_id == pre_loader_id
          && main_frame.as_deref().is_none_or(|id| id == state.same_document_frame)
        {
          return Ok(true);
        }
      }
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return Ok(false);
      }
    }
  }

  /// Backend-level synchronous accessor for the cached top-level
  /// `frameId`. Populated by `CdpPage::goto` from the
  /// `Page.navigate` response (see `nav_result.frameId`) and by the
  /// lazy `set_content` path.
  #[must_use]
  pub fn peek_main_frame_id(&self) -> Option<String> {
    self.main_frame_id.get().cloned()
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.evaluate`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn url(&self) -> Result<Option<String>> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "location.href",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.evaluate`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn title(&self) -> Result<Option<String>> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "document.title",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }
}
