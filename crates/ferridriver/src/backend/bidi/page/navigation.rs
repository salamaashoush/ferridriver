//! `BidiPage` frames and navigation.

use super::{Arc, BidiPage, FerriError, FrameInfo, NavLifecycle, Response, Result, collect_frames, json};

impl BidiPage {
  // ── Frames ──────────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when `browsingContext.getTree` fails or its response has no `contexts` array.
  /// A child frame whose name lookup fails keeps an empty name instead.
  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>> {
    let result = self
      .cmd("browsingContext.getTree", json!({"root": &*self.context_id}))
      .await?;
    let contexts = result
      .get("contexts")
      .and_then(|v| v.as_array())
      .ok_or_else(|| FerriError::protocol("browsingContext.getTree", "missing contexts"))?;

    let mut frames = Vec::new();
    for ctx in contexts {
      collect_frames(ctx, None, &mut frames);
    }

    // BiDi doesn't include frame names in the context tree.
    // Resolve names in parallel by evaluating `window.name` in each child frame.
    let child_indices: Vec<usize> = frames
      .iter()
      .enumerate()
      .filter(|(_, f)| f.parent_frame_id.is_some() && f.name.is_empty())
      .map(|(i, _)| i)
      .collect();
    if !child_indices.is_empty() {
      let futs: Vec<_> = child_indices
        .iter()
        .map(|&i| self.eval_internal("window.name", &frames[i].frame_id))
        .collect();
      let results = futures::future::join_all(futs).await;
      for (idx, result) in child_indices.into_iter().zip(results) {
        if let Ok(Some(val)) = result
          && let Some(name) = val.as_str()
        {
          frames[idx].name = name.to_string();
        }
      }
    }
    Ok(frames)
  }

  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` command fails or its result cannot be parsed, and
  /// `FerriError::Evaluation` when the expression throws. For a child frame it also fails when
  /// injecting the selector engine fails or the operation's deadline passes while waiting for the
  /// frame to load.
  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>> {
    // In BiDi, frames ARE browsing contexts. A frame-scoped eval
    // (selector engine queries) needs `window.__fd` in that child
    // context — inject on demand after the child is parseable
    // (Residual 2). Skip for the main context (already injected by the
    // caller's `ensure_engine_injected`).
    if frame_id != &*self.context_id {
      self.wait_context_ready(frame_id).await?;
      self.ensure_engine_injected_in(frame_id).await?;
    }
    self.eval_internal(expression, frame_id).await
  }

  /// Mark a child frame's owner `<iframe>` element with the child's
  /// frame id via the trace snapshot streamer (mirrors
  /// `bidiPage.ts::getFrameElement`: `browsingContext.locateNodes` with
  /// a context locator in the parent, then `script.callFunction` in the
  /// parent realm with the element as argument).
  ///
  /// # Errors
  ///
  /// Returns an error when `browsingContext.locateNodes` fails or finds no element for the child
  /// frame or `script.callFunction` fails, and `FerriError::Evaluation` when the marking script
  /// throws.
  pub async fn mark_snapshot_iframe(
    &self,
    child_frame_id: &str,
    parent_frame_id: &str,
    streamer_global: &str,
  ) -> Result<()> {
    let located = self
      .cmd(
        "browsingContext.locateNodes",
        json!({
          "context": parent_frame_id,
          "locator": { "type": "context", "value": { "context": child_frame_id } },
        }),
      )
      .await?;
    let shared_id = located
      .get("nodes")
      .and_then(|n| n.get(0))
      .and_then(|n| n.get("sharedId"))
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("browsingContext.locateNodes", "no node for child frame"))?
      .to_string();
    let declaration =
      format!("(el, frameId) => {{ const s = window[{streamer_global:?}]; if (s) s.markIframe(el, frameId); }}");
    let result = self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": declaration,
          "target": { "context": parent_frame_id },
          "arguments": [
            { "type": "sharedReference", "sharedId": shared_id },
            { "type": "string", "value": child_frame_id },
          ],
          "awaitPromise": false,
          "resultOwnership": "none",
        }),
      )
      .await?;
    if result.get("type").and_then(serde_json::Value::as_str) == Some("exception") {
      let text = result
        .pointer("/exceptionDetails/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("markIframe threw");
      return Err(FerriError::evaluation(text.to_string()));
    }
    Ok(())
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when `browsingContext.navigate` fails, and a timeout error when the
  /// navigation or the wait for its response outlasts `timeout_ms`. Failures of the
  /// `network.setExtraHeaders` calls that carry `referer` are ignored.
  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<Response>> {
    let deadline = crate::backend::deadline_for(timeout_ms);
    self.injected_script.reset();
    self.clear_handle_realms();
    self.nav_request_slot.clear();

    // WebDriver BiDi `browsingContext.navigate` has no `referrer` param —
    // Playwright's own BiDi backend drops it too
    // (`/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPage.ts::navigateFrame`
    // takes a `referrer` arg and never uses it). The honest BiDi analogue
    // is `network.setExtraHeaders` which we set for the duration of this
    // navigation and reset afterwards so it doesn't leak into subsequent
    // requests on the same context.
    let had_referer = referer.is_some();
    if let Some(r) = referer {
      let _ = self
        .cmd(
          "network.setExtraHeaders",
          json!({
            "headers": [{ "name": "Referer", "value": { "type": "string", "value": r } }],
            "contexts": [&*self.context_id],
          }),
        )
        .await;
    }

    let wait_state = Self::lifecycle_to_wait(lifecycle);
    let result = tokio::time::timeout_at(
      crate::backend::deadline_for(timeout_ms),
      self.send_typed(&ferridriver_bidi_proto::browsing_context::Navigate {
        context: &self.context_id,
        url,
        wait: Some(wait_state),
      }),
    )
    .await;

    if had_referer {
      let _ = self
        .cmd(
          "network.setExtraHeaders",
          json!({ "headers": [], "contexts": [&*self.context_id] }),
        )
        .await;
    }

    match result {
      // `navigation` is null for a same-document jump — nothing was
      // requested, so nothing is coming.
      Ok(Ok(nav)) => {
        let new_document = nav.navigation.is_some();
        let grace = if new_document {
          crate::network::NAV_REQUEST_GRACE
        } else {
          std::time::Duration::ZERO
        };
        // A command that waited for the document answers after the
        // document's request was announced; `none` answers before it.
        let announced = new_document && wait_state != ferridriver_bidi_proto::browsing_context::ReadinessState::None;
        self.await_nav_response(grace, announced, deadline, timeout_ms).await
      },
      Ok(Err(e)) => Err(e),
      Err(_) => Err(FerriError::timeout(format!("navigating to {url}"), timeout_ms)),
    }
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation. Returns `None` for
  /// same-document navigations (no new request was issued) or when
  /// the underlying request ended in failure.
  /// `grace` is how long to wait for the document request to be seen:
  /// non-zero only when a new document committed, since the request and
  /// the navigation result reach us on separate consumers and the
  /// request can be processed second. A same-document navigation issues
  /// no request, so waiting there would only slow it down.
  async fn await_nav_response(
    &self,
    grace: std::time::Duration,
    announced: bool,
    deadline: tokio::time::Instant,
    timeout_ms: u64,
  ) -> Result<Option<Response>> {
    let barrier = self
      .event_barrier
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    tokio::time::timeout_at(deadline, async {
      let mut grace = grace;
      if let Some(barrier) = barrier {
        barrier.wait().await?;
        // The consumer has now processed everything that came before the
        // answer, so a request announced by then is in the slot and a
        // document with none (`about:blank`) has nothing to wait for.
        if announced {
          grace = std::time::Duration::ZERO;
        }
      }
      Ok(self.nav_request_slot.final_response(grace).await)
    })
    .await
    .map_err(|_| FerriError::timeout("processing navigation events", timeout_ms))?
  }

  /// # Errors
  ///
  /// Returns a timeout error when no `browsingContext.load` event for this context arrives within
  /// 30 seconds, and a backend error when the event channel closes first.
  pub async fn wait_for_navigation(&self) -> Result<()> {
    // Tap the load event for this context and wait for it — a missed
    // event here is a 30s hang, so the wait must be lossless.
    let mut rx = self.session.transport.tap_events();
    let ctx = Arc::clone(&self.context_id);
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
      while let Some(event) = rx.recv().await {
        if event.method == "browsingContext.load"
          && let Some(c) = event.params.get("context").and_then(|v| v.as_str())
          && c == &*ctx
        {
          return Ok(());
        }
      }
      Err(FerriError::backend("Event channel closed"))
    });
    match timeout.await {
      Ok(result) => result,
      Err(_) => Err(FerriError::timeout("wait_for_navigation", 30_000)),
    }
  }

  /// Force a GC pass in the page's JS engine. Mirrors Playwright's
  /// `bidiPage.requestGC`: evaluate `TestUtils.gc()` and surface an
  /// exception result (Firefox builds without `TestUtils`) as
  /// `Unsupported` — Playwright's `BiDi` backend throws the same way.
  ///
  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` command fails, and `FerriError::Unsupported` when
  /// `TestUtils.gc()` throws because the Firefox build has no `TestUtils`.
  pub async fn request_gc(&self) -> Result<()> {
    let result = self
      .cmd(
        "script.evaluate",
        json!({
          "expression": "TestUtils.gc()",
          "target": {"context": &*self.context_id},
          "awaitPromise": true,
        }),
      )
      .await?;
    if result.get("type").and_then(|v| v.as_str()) == Some("exception") {
      return Err(FerriError::unsupported(
        "requestGC: TestUtils.gc() is unavailable in this Firefox build over WebDriver BiDi",
      ));
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `browsingContext.activate` command fails.
  pub async fn bring_to_front(&self) -> Result<()> {
    self
      .send_typed(&ferridriver_bidi_proto::browsing_context::Activate {
        context: &self.context_id,
      })
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `browsingContext.reload` command fails, and a timeout error when the
  /// reload or the wait for its response outlasts `timeout_ms`.
  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    let deadline = crate::backend::deadline_for(timeout_ms);
    self.injected_script.reset();
    self.clear_handle_realms();
    self.nav_request_slot.clear();
    let wait_state = Self::lifecycle_to_wait(lifecycle);
    let result = tokio::time::timeout_at(
      crate::backend::deadline_for(timeout_ms),
      self.send_typed(&ferridriver_bidi_proto::browsing_context::Reload {
        context: &self.context_id,
        ignore_cache: None,
        wait: Some(wait_state),
      }),
    )
    .await;

    match result {
      Ok(Ok(_)) => {
        self
          .await_nav_response(
            crate::network::NAV_REQUEST_GRACE,
            wait_state != ferridriver_bidi_proto::browsing_context::ReadinessState::None,
            deadline,
            timeout_ms,
          )
          .await
      },
      Ok(Err(e)) => Err(e),
      Err(_) => Err(FerriError::timeout("reloading", timeout_ms)),
    }
  }

  /// # Errors
  ///
  /// Returns an error when the `browsingContext.traverseHistory` command fails, and a timeout error
  /// when the traversal or the wait for its response outlasts `timeout_ms`.
  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.traverse_history(-1, lifecycle, timeout_ms, "go_back").await
  }

  /// # Errors
  ///
  /// Returns an error when the `browsingContext.traverseHistory` command fails, and a timeout error
  /// when the traversal or the wait for its response outlasts `timeout_ms`.
  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.traverse_history(1, lifecycle, timeout_ms, "go_forward").await
  }

  /// `WebDriver` `BiDi` `browsingContext.traverseHistory` has no `wait` parameter:
  /// it initiates traversal and returns `{}` before the destination document
  /// loads (unlike `navigate`/`reload`). Tap events before sending the command
  /// and wait for the destination lifecycle (or same-document history/fragment
  /// event) so `page.url()` and the main-document response are ready on return.
  async fn traverse_history(
    &self,
    delta: i64,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    label: &str,
  ) -> Result<Option<Response>> {
    let deadline = crate::backend::deadline_for(timeout_ms);
    self.nav_request_slot.clear();
    let mut rx = self.session.transport.tap_events();
    let result = tokio::time::timeout_at(
      deadline,
      self.send_typed(&ferridriver_bidi_proto::browsing_context::TraverseHistory {
        context: &self.context_id,
        delta,
      }),
    )
    .await;

    match result {
      Ok(Ok(_)) => {},
      Ok(Err(e)) if e.to_string().contains("no such history entry") => return Ok(None),
      Ok(Err(e)) => return Err(e),
      Err(_) => return Err(FerriError::timeout(label, timeout_ms)),
    }

    let is_new_document = tokio::time::timeout_at(deadline, async {
      while let Some(event) = rx.recv().await {
        if event.params.get("context").and_then(|v| v.as_str()) != Some(&*self.context_id) {
          continue;
        }
        match event.method.as_str() {
          "browsingContext.historyUpdated" | "browsingContext.fragmentNavigated" => return Ok(false),
          "browsingContext.navigationCommitted" if matches!(lifecycle, NavLifecycle::Commit) => return Ok(true),
          "browsingContext.domContentLoaded"
            if matches!(lifecycle, NavLifecycle::Commit | NavLifecycle::DomContentLoaded) =>
          {
            return Ok(true);
          },
          "browsingContext.load" => return Ok(true),
          "browsingContext.navigationFailed" | "browsingContext.navigationAborted" => {
            return Err(FerriError::Backend(format!("{label}: navigation aborted or failed")));
          },
          "browsingContext.contextDestroyed" => {
            return Err(FerriError::target_closed(Some(
              "page closed during history traversal".to_string(),
            )));
          },
          _ => {},
        }
      }
      Err(FerriError::backend(format!("BiDi event stream closed during {label}")))
    })
    .await
    .map_err(|_| FerriError::timeout(label, timeout_ms))??;

    let grace = if is_new_document {
      crate::network::NAV_REQUEST_GRACE
    } else {
      std::time::Duration::ZERO
    };
    self
      .await_nav_response(grace, is_new_document, deadline, timeout_ms)
      .await
  }

  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` of `location.href` fails or its result cannot be
  /// parsed, and `FerriError::Evaluation` when it throws.
  pub async fn url(&self) -> Result<Option<String>> {
    self
      .eval_internal("location.href", &self.context_id)
      .await
      .map(|v| v.and_then(|val| val.as_str().map(String::from)))
  }

  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` of `document.title` fails or its result cannot be
  /// parsed, and `FerriError::Evaluation` when it throws.
  pub async fn title(&self) -> Result<Option<String>> {
    self
      .eval_internal("document.title", &self.context_id)
      .await
      .map(|v| v.and_then(|val| val.as_str().map(String::from)))
  }
}
