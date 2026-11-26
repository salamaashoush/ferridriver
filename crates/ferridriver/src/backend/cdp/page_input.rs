//! `CdpPage` mouse, keyboard, touch, and native drag-and-drop input.

use super::{
  Arc, CdpPage, DRAG_CLEANUP_JS, DRAG_SETUP_JS, KeyStroke, Result, cdp_button_bit, key_down_params, key_up_params,
};

impl CdpPage {
  // ---- Input ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.click_at_opts(x, y, "left", 1).await
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let mods = self.keyboard.modifiers();
    let press = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": click_count, "modifiers": mods}),
    );
    let release = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": click_count, "modifiers": mods}),
    );
    let _ = tokio::try_join!(press, release)?;
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  /// Single-click fast path: `click_count == 1`, no delay, `steps == 1`.
  /// Pipelines the (optional pre-press move +) press + release in one
  /// `try_join!` so the renderer sees them back-to-back at one CDP RTT.
  async fn single_click_fast_path(&self, x: f64, y: f64, button: &str, mods: u32, skip_move: bool) -> Result<()> {
    let press = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({
        "type": "mousePressed",
        "x": x,
        "y": y,
        "button": button,
        "clickCount": 1,
        "modifiers": mods,
      }),
    );
    let release = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({
        "type": "mouseReleased",
        "x": x,
        "y": y,
        "button": button,
        "clickCount": 1,
        "modifiers": mods,
      }),
    );
    if skip_move {
      let _ = tokio::try_join!(press, release)?;
    } else {
      let moved = self.cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({
          "type": "mouseMoved",
          "x": x,
          "y": y,
          "modifiers": mods,
        }),
      );
      let _ = tokio::try_join!(moved, press, release)?;
    }
    Ok(())
  }

  /// Dispatch a click at `(x, y)` honoring the full Playwright option
  /// bag: `button`, `click_count`, delay between press/release, and
  /// `steps` interpolated mousemoves from the last cursor position to
  /// the target, every event carrying the held modifiers.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn click_at_with(&self, x: f64, y: f64, args: &super::super::BackendClickArgs) -> Result<()> {
    let button = args.button.as_cdp();
    let mods = self.keyboard.modifiers();
    // Steps-1 intermediate mousemoves + one final at (x,y). Playwright
    // default is `steps: 1` → single mousemove at dest. Mirror that by
    // emitting a `mouseMoved` at the target before press so the page
    // sees the move even when we can't track the prior cursor.
    let steps = args.steps.max(1);
    // Read the prior cursor position synchronously. When `steps == 1`
    // and the prior position is already at the click target, the
    // pre-press `mouseMoved` is a no-op event-wise (Chrome treats
    // identical-position consecutive moves as a single hover state),
    // but the RTT itself still costs ~1 CDP RTT. Skip it on the
    // common bench shape (back-to-back clicks at the same button).
    // Mirrors Playwright's `_lastPosition` short-circuit in
    // `/tmp/playwright/packages/playwright-core/src/server/chromium/crInput.ts`
    // (Mouse class). The short-circuit is bypassed when `steps > 1`
    // (caller asked for an animated move) or when no prior position
    // is known (first click — must seed cursor in the renderer).
    let skip_move = steps == 1
      && match self.last_cursor_pos.lock() {
        Ok(g) => matches!(*g, Some((px, py)) if (px - x).abs() < 0.5 && (py - y).abs() < 0.5),
        Err(_) => false,
      };
    if args.delay_ms == 0 && steps == 1 {
      if args.click_count == 1 {
        self.single_click_fast_path(x, y, button, mods, skip_move).await?;
      } else {
        // Pipeline move + all press/release pairs (e.g. dblclick) in one batch.
        let mut events = Vec::with_capacity((usize::from(!skip_move)) + (args.click_count as usize) * 2);
        if !skip_move {
          events.push(serde_json::json!({
            "type": "mouseMoved",
            "x": x,
            "y": y,
            "modifiers": mods,
          }));
        }
        for n in 1..=args.click_count {
          events.push(serde_json::json!({
            "type": "mousePressed",
            "x": x,
            "y": y,
            "button": button,
            "clickCount": n,
            "modifiers": mods,
          }));
          events.push(serde_json::json!({
            "type": "mouseReleased",
            "x": x,
            "y": y,
            "button": button,
            "clickCount": n,
            "modifiers": mods,
          }));
        }
        futures::future::try_join_all(events.into_iter().map(|ev| self.cmd("Input.dispatchMouseEvent", ev))).await?;
      }
      if let Ok(mut guard) = self.last_cursor_pos.lock() {
        *guard = Some((x, y));
      }
      return Ok(());
    }

    if !skip_move {
      // Pipeline the interpolated moves: enqueue the burst in order and
      // await all acks together (same mechanism `single_click_fast_path`
      // relies on for press-before-release ordering) — one round trip
      // instead of one per step. No button is held here, so the drag
      // manager never needs to intercept mid-burst.
      let moves = (1..=steps).map(|i| {
        let t = f64::from(i) / f64::from(steps);
        // conservative: interpolate from (0,0) when we lack prior-pos state
        let sx = x * t;
        let sy = y * t;
        self.cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mouseMoved",
            "x": if i == steps { x } else { sx },
            "y": if i == steps { y } else { sy },
            "modifiers": mods,
          }),
        )
      });
      futures::future::try_join_all(moves).await?;
    }
    for n in 1..=args.click_count {
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mousePressed",
            "x": x,
            "y": y,
            "button": button,
            "clickCount": n,
            "modifiers": mods,
          }),
        )
        .await?;
      if args.delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(args.delay_ms)).await;
      }
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mouseReleased",
            "x": x,
            "y": y,
            "button": button,
            "clickCount": n,
            "modifiers": mods,
          }),
        )
        .await?;
    }
    // Record final cursor position for the next click's
    // skip-redundant-mouseMoved short-circuit.
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  /// Dispatch a hover at `(x, y)`: `steps` interpolated `mouseMoved`
  /// events with the held modifiers on each, ending at `(x, y)` exactly.
  /// No `mousePressed` / `mouseReleased`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn hover_at_with(&self, x: f64, y: f64, args: &super::super::BackendHoverArgs) -> Result<()> {
    let mods = self.keyboard.modifiers();
    let steps = args.steps.max(1);
    let skip_move = steps == 1
      && match self.last_cursor_pos.lock() {
        Ok(g) => matches!(*g, Some((px, py)) if (px - x).abs() < 0.5 && (py - y).abs() < 0.5),
        Err(_) => false,
      };
    if !skip_move {
      // Pipelined like the click move burst: no button held, one round
      // trip for the whole interpolation.
      let moves = (1..=steps).map(|i| {
        let t = f64::from(i) / f64::from(steps);
        let sx = if i == steps { x } else { x * t };
        let sy = if i == steps { y } else { y * t };
        self.cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mouseMoved",
            "x": sx,
            "y": sy,
            "modifiers": mods,
          }),
        )
      });
      futures::future::try_join_all(moves).await?;
    }
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  /// Dispatch a native tap at `(x, y)` via CDP `Input.dispatchTouchEvent`:
  /// `touchStart` with a single `TouchPoint { x, y }` immediately followed
  /// by `touchEnd` with an empty `touchPoints` array. Matches Playwright's
  /// `server/chromium/crInput.ts::RawTouchscreenImpl::tap` (commit
  /// reference: `/tmp/playwright/packages/playwright-core/src/server/chromium/crInput.ts:174`).
  /// The held modifiers ride on each event so the page sees
  /// `event.shiftKey` etc. as expected.
  ///
  /// Before the first dispatch we flip `Emulation.setTouchEmulationEnabled
  /// { enabled: true, maxTouchPoints: 1 }`. Chromium needs the emulator
  /// enabled for `Input.dispatchTouchEvent` to actually route the events
  /// through the renderer's touch hit-tester — without it, the protocol
  /// accepts the call but no DOM `touchstart` / `pointerup(touch)`
  /// listener fires. Playwright's `BrowserContextOptions.hasTouch` wires
  /// this on context creation; we opt in lazily on first tap so callers
  /// who never tap pay nothing.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Emulation.setTouchEmulationEnabled` or `Input.dispatchTouchEvent`,
  /// the target has closed, or the command outlives its time budget.
  pub async fn tap_at_with(&self, x: f64, y: f64) -> Result<()> {
    let mods = self.keyboard.modifiers();
    if !self.touch_emulation_enabled.load(std::sync::atomic::Ordering::Relaxed) {
      self
        .cmd(
          "Emulation.setTouchEmulationEnabled",
          serde_json::json!({ "enabled": true, "maxTouchPoints": 1 }),
        )
        .await?;
      self
        .touch_emulation_enabled
        .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let start = self.cmd(
      "Input.dispatchTouchEvent",
      serde_json::json!({
        "type": "touchStart",
        "modifiers": mods,
        "touchPoints": [{ "x": x, "y": y }],
      }),
    );
    let end = self.cmd(
      "Input.dispatchTouchEvent",
      serde_json::json!({
        "type": "touchEnd",
        "modifiers": mods,
        "touchPoints": [],
      }),
    );
    let _ = tokio::try_join!(start, end)?;
    Ok(())
  }

  fn set_cursor_pos(&self, x: f64, y: f64) {
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
  }

  /// Spawn the lossless `Input.dragIntercepted` tap once per page. The
  /// task writes each intercepted payload into the drag manager's slot
  /// and wakes the intercept flow; the abort handle rides
  /// `listener_tasks` so `close_page` reaps it.
  fn ensure_drag_intercept_tap(&self) {
    if self
      .drag_manager
      .tap_started
      .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
      return;
    }
    let mut rx = self
      .transport
      .tap_event_methods(&["Input.dragIntercepted"], self.session_id.as_deref());
    let dm = Arc::clone(&self.drag_manager);
    let task = tokio::spawn(async move {
      while let Some(evt) = rx.recv().await {
        if let Some(data) = evt.get("params").and_then(|p| p.get("data")).cloned() {
          match dm.intercepted_slot.lock() {
            Ok(mut slot) => *slot = Some(data),
            Err(poisoned) => *poisoned.into_inner() = Some(data),
          }
          // `notify_one` stores a permit when nobody is waiting yet, so
          // an event landing between the intercept flow's slot check and
          // its `notified().await` is not lost.
          dm.intercepted_notify.notify_one();
        }
      }
    });
    if let Ok(mut tasks) = self.listener_tasks.lock() {
      tasks.push(task.abort_handle());
    }
  }

  /// Evaluate `js` in the main frame's default context and every tracked
  /// child-frame context concurrently, returning each successful
  /// evaluation's boolean result. Per-frame failures (mid-navigation,
  /// destroyed context) are skipped, and the whole batch is capped at
  /// one second so a wedged frame cannot stall the input pipeline —
  /// the moral equivalent of Playwright's
  /// `safeNonStallingEvaluateInAllFrames`.
  async fn eval_bool_in_all_contexts(&self, js: &str, await_promise: bool) -> Vec<bool> {
    let base = serde_json::json!({
      "expression": js,
      "returnByValue": true,
      "awaitPromise": await_promise,
    });
    let mut params = vec![base.clone()];
    for ctx_id in self
      .frame_contexts
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .values()
    {
      let mut p = base.clone();
      p["contextId"] = serde_json::json!(ctx_id);
      params.push(p);
    }
    let evals = params.into_iter().map(|p| self.cmd("Runtime.evaluate", p));
    let joined = tokio::time::timeout(std::time::Duration::from_secs(1), futures::future::join_all(evals));
    match joined.await {
      Ok(results) => results
        .into_iter()
        .filter_map(std::result::Result::ok)
        .map(|r| {
          r.get("result")
            .and_then(|res| res.get("value"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        })
        .collect(),
      Err(_) => Vec::new(),
    }
  }

  /// Port of `crDragDrop.ts::DragManager.interceptDragCausedByMove` for
  /// a mousemove dispatched while the left button is held: arm the
  /// page-side dragstart detectors, enable `Input.setInterceptDrags`,
  /// ship the move, and if the page reports an un-prevented `dragstart`,
  /// adopt the `Input.dragIntercepted` payload as the active drag and
  /// announce it with a `dragEnter`.
  async fn intercept_drag_caused_by_move(&self, x: f64, y: f64, mask: u32) -> Result<()> {
    let mods = self.keyboard.modifiers();
    self.ensure_drag_intercept_tap();
    match self.drag_manager.intercepted_slot.lock() {
      Ok(mut slot) => *slot = None,
      Err(poisoned) => *poisoned.into_inner() = None,
    }
    self.eval_bool_in_all_contexts(DRAG_SETUP_JS, false).await;
    if let Err(e) = self
      .cmd("Input.setInterceptDrags", serde_json::json!({"enabled": true}))
      .await
    {
      // Embedder builds without drag interception (the command only
      // exists on real Chromium): fall back to the plain move rather
      // than failing every held-button move.
      if e.to_string().contains("wasn't found") {
        tracing::debug!(target: "ferridriver::cdp", "Input.setInterceptDrags unavailable; skipping drag interception");
        self
          .cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseMoved", "x": x, "y": y, "button": "left", "buttons": mask, "modifiers": mods}),
          )
          .await?;
        self.set_cursor_pos(x, y);
        return Ok(());
      }
      return Err(e);
    }
    let move_result = self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y, "button": "left", "buttons": mask, "modifiers": mods}),
      )
      .await;
    let expecting = self
      .eval_bool_in_all_contexts(DRAG_CLEANUP_JS, true)
      .await
      .into_iter()
      .any(|started| started);
    let _ = self
      .cmd("Input.setInterceptDrags", serde_json::json!({"enabled": false}))
      .await;
    move_result?;

    let take_slot = || match self.drag_manager.intercepted_slot.lock() {
      Ok(mut slot) => slot.take(),
      Err(poisoned) => poisoned.into_inner().take(),
    };
    let mut drag_data = take_slot();
    if expecting && drag_data.is_none() {
      // An un-prevented dragstart means Chromium WILL emit
      // `Input.dragIntercepted`; the cap only guards a crashed renderer.
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
      loop {
        drag_data = take_slot();
        if drag_data.is_some() {
          break;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
          break;
        }
        if tokio::time::timeout(deadline - now, self.drag_manager.intercepted_notify.notified())
          .await
          .is_err()
        {
          drag_data = take_slot();
          break;
        }
      }
    }
    if let Some(data) = drag_data {
      self
        .cmd(
          "Input.dispatchDragEvent",
          serde_json::json!({"type": "dragEnter", "x": x, "y": y, "data": data, "modifiers": mods}),
        )
        .await?;
      *self.drag_manager.drag_data.lock().await = Some(data);
    }
    self.set_cursor_pos(x, y);
    Ok(())
  }

  /// Cancel an in-flight intercepted drag (`crDragDrop.ts::cancelDrag`).
  /// Returns `true` if a drag was active and cancelled.
  async fn cancel_drag(&self) -> Result<bool> {
    if self.drag_manager.drag_data.lock().await.take().is_none() {
      return Ok(false);
    }
    let (x, y) = self.last_cursor_pos.lock().ok().and_then(|g| *g).unwrap_or((0.0, 0.0));
    self
      .cmd(
        "Input.dispatchDragEvent",
        serde_json::json!({
          "type": "dragCancel",
          "x": x,
          "y": y,
          "data": {"items": [], "dragOperationsMask": 0xFFFF},
        }),
      )
      .await?;
    Ok(true)
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent` or `Input.dispatchDragEvent`, the target
  /// has closed, or the command outlives its time budget.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    // Route through the drag manager (Playwright `RawMouse.move` →
    // `DragManager.interceptDragCausedByMove`): while a native drag is
    // in flight the move is re-dispatched as `dragOver`; while the left
    // button is held it runs under an interception window that captures
    // a starting native drag.
    let drag_data = self.drag_manager.drag_data.lock().await.clone();
    if let Some(data) = drag_data {
      self
        .cmd(
          "Input.dispatchDragEvent",
          serde_json::json!({"type": "dragOver", "x": x, "y": y, "data": data, "modifiers": self.keyboard.modifiers()}),
        )
        .await?;
      self.set_cursor_pos(x, y);
      return Ok(());
    }
    let mask = self.drag_manager.pressed_buttons();
    if mask & 1 == 0 {
      // No left button held — plain move. `button`/`buttons` carry any
      // held secondary button so `event.buttons`-gating handlers see it.
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mouseMoved",
            "x": x,
            "y": y,
            "button": self.drag_manager.move_button(),
            "buttons": mask,
            "modifiers": self.keyboard.modifiers(),
          }),
        )
        .await?;
      self.set_cursor_pos(x, y);
      return Ok(());
    }
    self.intercept_drag_caused_by_move(x, y, mask).await
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent` or `Input.dispatchDragEvent` for any step,
  /// the target has closed, or the command outlives its time budget.
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    let steps = steps.max(1);
    for i in 1..=steps {
      let t = f64::from(i) / f64::from(steps);
      let x = from_x + (to_x - from_x) * t;
      let y = from_y + (to_y - from_y) * t;
      // Per-step `move_mouse` (not a raw dispatch) so a drag started
      // mid-path is intercepted exactly like Playwright's `Mouse.move`
      // steps loop, which routes every step through `RawMouse.move`.
      self.move_mouse(x, y).await?;
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn mouse_wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({
          "type": "mouseWheel",
          "x": x,
          "y": y,
          "deltaX": delta_x,
          "deltaY": delta_y,
          "modifiers": self.keyboard.modifiers(),
        }),
      )
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let mask = self.drag_manager.update_buttons(|m| *m |= cdp_button_bit(button));
    // Playwright skips the protocol press while a native drag is in
    // flight (`crInput.ts::RawMouseImpl.down`) — the browser considers
    // the button already held by the drag session.
    if self.drag_manager.drag_data.lock().await.is_some() {
      self.set_cursor_pos(x, y);
      return Ok(());
    }
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({
          "type": "mousePressed",
          "x": x,
          "y": y,
          "button": button,
          "buttons": mask,
          "clickCount": click_count,
          "modifiers": self.keyboard.modifiers(),
        }),
      )
      .await?;
    self.set_cursor_pos(x, y);
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent` or `Input.dispatchDragEvent`, the target
  /// has closed, or the command outlives its time budget.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let mask = self.drag_manager.update_buttons(|m| *m &= !cdp_button_bit(button));
    // An in-flight intercepted drag ends in a `drop`, not a
    // `mouseReleased` (`crInput.ts::RawMouseImpl.up`).
    let drag_data = self.drag_manager.drag_data.lock().await.take();
    if let Some(data) = drag_data {
      self
        .cmd(
          "Input.dispatchDragEvent",
          serde_json::json!({"type": "drop", "x": x, "y": y, "data": data, "modifiers": self.keyboard.modifiers()}),
        )
        .await?;
      self.set_cursor_pos(x, y);
      return Ok(());
    }
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({
          "type": "mouseReleased",
          "x": x,
          "y": y,
          "button": button,
          "buttons": mask,
          "clickCount": click_count,
          "modifiers": self.keyboard.modifiers(),
        }),
      )
      .await?;
    self.set_cursor_pos(x, y);
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.insertText`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn insert_text(&self, text: &str) -> Result<()> {
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  pub(crate) async fn dispatch_key_down(&self, _name: &str, stroke: &KeyStroke) -> Result<()> {
    // Escape cancels an in-flight intercepted drag instead of reaching
    // the page.
    if stroke.description.code == "Escape" && self.cancel_drag().await? {
      return Ok(());
    }
    self
      .cmd(
        "Input.dispatchKeyEvent",
        key_down_params(stroke, crate::backend::keyboard::MAC),
      )
      .await?;
    Ok(())
  }

  pub(crate) async fn dispatch_key_up(&self, _name: &str, stroke: &KeyStroke) -> Result<()> {
    self.cmd("Input.dispatchKeyEvent", key_up_params(stroke)).await?;
    Ok(())
  }
}
