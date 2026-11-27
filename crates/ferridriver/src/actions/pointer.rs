//! Pointer action primitives: click, hover, drag step, tap, and hit-target interception.

use super::{
  ClickPrep, click_prep, click_prep_with_states, cross_origin_frame_offset, pause_for_change, pointer_retry,
  scroll_alignment,
};
use crate::backend::{AnyElement, AnyPage};
use crate::error::{FerriError, Result};

// ─── Auto-waiting ───────────────────────────────────────────────────────────

/// Resolve the click dispatch point for an element, in top-level page
/// (viewport) coordinates. Mirrors Playwright's
/// `/tmp/playwright/packages/playwright-core/src/server/dom.ts` click-point
/// logic: scroll-into-view, then take the element's padding-box + an
/// optional `position` offset, then walk the frame chain accumulating
/// `window.frameElement.getBoundingClientRect()` offsets so an iframe
/// element resolves to the right top-level coords.
///
/// All runs in a single JS round-trip; the per-backend `click_at_with`
/// receives ready-to-dispatch viewport coords.
///
/// # Errors
///
/// Returns an error if the JS evaluation fails or the element has no box.
pub async fn resolve_click_point(
  element: &AnyElement,
  position: Option<crate::options::Point>,
  scroll: bool,
  alignment: Option<&str>,
) -> Result<(f64, f64)> {
  let position_js = match position {
    Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
    None => "null".to_string(),
  };
  let alignment_js = alignment.unwrap_or("null");
  // Prefer the non-standard Chromium/WebKit `scrollIntoViewIfNeeded` —
  // a single native call that measures viewport + scroll in one step.
  // Fall back to the W3C `scrollIntoView({block:'center'})` on Firefox
  // (BiDi), which lacks the non-standard variant. Both paths are
  // genuine implementations; this is feature detection for the
  // best-available primitive, not a workaround.
  let js = format!(
    "function() {{ \
      var align = {alignment_js}; \
      if (!{scroll}) {{ \
      }} else if (align) {{ \
        this.scrollIntoView(Object.assign({{ behavior: 'instant' }}, align)); \
      }} else if (typeof this.scrollIntoViewIfNeeded === 'function') {{ \
        this.scrollIntoViewIfNeeded(); \
      }} else {{ \
        this.scrollIntoView({{ block: 'center', inline: 'center' }}); \
      }} \
      var r = this.getBoundingClientRect(); \
      var pos = {position_js}; \
      var x = pos ? (r.x + pos.x) : (r.x + r.width / 2); \
      var y = pos ? (r.y + pos.y) : (r.y + r.height / 2); \
      if (!pos && r.width * r.height > 0) {{ \
        var vw = window.innerWidth, vh = window.innerHeight; \
        var left = Math.max(r.left, 0), right = Math.min(r.right, vw); \
        var top = Math.max(r.top, 0), bottom = Math.min(r.bottom, vh); \
        if ((right - left) * (bottom - top) <= 0.99 || right <= left || bottom <= top) return 'error:notinviewport'; \
        x = (left + right) / 2; \
        y = (top + bottom) / 2; \
      }} \
      var win = this.ownerDocument.defaultView; \
      while (win && win !== win.parent && win.frameElement) {{ \
        var fr = win.frameElement.getBoundingClientRect(); \
        x += fr.x; \
        y += fr.y; \
        win = win.parent; \
      }} \
      return {{ x: x, y: y }}; \
    }}"
  );
  let val = element.call_js_fn_value(&js).await?;
  if let Some(reason @ serde_json::Value::String(_)) = &val {
    return Err(FerriError::Backend(reason.as_str().unwrap_or_default().to_string()));
  }
  val
    .and_then(|v| {
      let x = v.get("x").and_then(serde_json::Value::as_f64)?;
      let y = v.get("y").and_then(serde_json::Value::as_f64)?;
      Some((x, y))
    })
    .ok_or_else(|| FerriError::backend("could not compute click point"))
}

/// Hold exactly an action's `modifiers` option for the action, returning
/// the modifiers to restore after it. An action without the option
/// keeps whatever the keyboard holds.
pub(crate) async fn hold_modifiers(page: &AnyPage, modifiers: &[crate::options::Modifier]) -> Result<Option<u32>> {
  if modifiers.is_empty() {
    return Ok(None);
  }
  let held = page
    .ensure_modifiers(crate::options::modifiers_bitmask(modifiers))
    .await?;
  Ok(Some(held))
}

pub(crate) async fn restore_modifiers(page: &AnyPage, restore: Option<u32>) -> Result<()> {
  match restore {
    Some(modifiers) => page.ensure_modifiers(modifiers).await.map(|_| ()),
    None => Ok(()),
  }
}

/// Dispatch a click with the full Playwright [`crate::options::ClickOptions`]
/// surface. Mirrors Playwright's `/tmp/playwright/packages/playwright-core/
/// src/server/dom.ts::ElementHandle._click`:
///
/// 1. If `!force`, run the actionability checks (visibility / attached /
///    stable / enabled / editable where applicable). On `force=true`,
///    skip them entirely and rely on the element's current state.
/// 2. Press the modifier keys (keydown) — even in `trial` mode; per
///    Playwright, modifiers are pressed so callers can test
///    modifier-visible UI without firing the actual mouse event.
/// 3. If `trial`, release modifiers and return `Ok(())` without firing
///    any mouse event.
/// 4. Otherwise resolve the click point (element center or padding-box
///    offset by `position`, with iframe-chain accumulation), then
///    dispatch via the backend's `click_at_with(x, y, args)`.
/// 5. Release modifiers on all paths (error or success) so page state
///    doesn't leak between actions.
///
/// `click_count`, `delay`, `button`, and `steps` are honored in
/// [`crate::backend::BackendClickArgs`] — each backend's `click_at_with`
/// is responsible for emitting the right wire events with those values.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the
/// per-backend click dispatch. Modifier release is best-effort and does
/// not override the primary error.
pub async fn click_with_opts(element: &AnyElement, page: &AnyPage, opts: &crate::options::ClickOptions) -> Result<()> {
  let args = crate::backend::BackendClickArgs::from_options(opts);
  // Retry the entire pointer action when the hit-target interceptor
  // reports another element captured the click — mirrors Playwright's
  // `_retryAction` loop in
  // `/tmp/playwright/packages/playwright-core/src/server/dom.ts:310`.
  // The most common reason in headed-mode tests: the page mutates
  // (onload `setTimeout` fires, dynamic ad slot lays out) between
  // `clickPrep` resolving the point and Chrome dispatching the
  // queued `Input.dispatchMouseEvent`, so the click lands on whatever
  // element flowed into that pixel position. Re-resolving the point
  // on retry resyncs us to the current layout.
  let retry_backoff_ms: [u64; 5] = [0, 20, 100, 100, 500];
  let mut attempt: usize = 0;
  let mut last_reason: Option<String> = None;
  loop {
    if attempt > 0 {
      let wait_ms = retry_backoff_ms[attempt.min(retry_backoff_ms.len() - 1)];
      // The action's deadline falls inside this wait: report why the last
      // attempt failed, which the action's call log records, rather than
      // sleep into the deadline and time out with no reason.
      if let Some(reason) = last_reason.as_ref()
        && crate::operation_budget::operation_remaining_ms().is_some_and(|left| left <= wait_ms)
      {
        return Err(FerriError::Backend(reason.clone()));
      }
      pause_for_change(page, std::time::Duration::from_millis(wait_ms)).await;
    }

    // Cycle scroll alignment across retries — Playwright `_retryPointerAction`
    // (`undefined` => precise `scrollIntoViewIfNeeded`, then end/center/start)
    // to reveal targets under `position:sticky` overlays. The count runs
    // on across the locator loop's tries as well as this loop's own.
    let scroll_align = if opts.scrolls() {
      scroll_alignment(pointer_retry() + attempt)
    } else {
      Some("false")
    };

    // Single combined pre-flight: actionability (visible /
    // enabled / stable) + `scrollIntoView` + viewport-intersected
    // `clickPoint` in ONE callFunctionOn.
    // The interceptor is armed inside the pre-flight call itself unless
    // the action skips it (force) or only checks it (trial).
    let arm_in_prep = !opts.is_force() && !opts.is_trial();
    let (x, y, armed) = if opts.is_force() {
      let (x, y) = resolve_click_point(element, opts.position, opts.scrolls(), None).await?;
      (x, y, None)
    } else {
      let hit_action = arm_in_prep.then_some("mouse");
      let prep = match click_prep(element, page, opts.position, scroll_align, hit_action).await {
        Ok(prep) => prep,
        // The deadline cut this attempt off before it could say why it
        // failed: report why the one before it did.
        Err(error) if error.is_timeout_error() => {
          return Err(last_reason.map_or(error, FerriError::Backend));
        },
        Err(error) => return Err(error),
      };
      match prep {
        ClickPrep::Ready { x, y, armed } => (x, y, armed),
        ClickPrep::NotActionable { reason } => {
          // `notconnected` => the element is gone; surface it so the
          // locator re-resolves. Transient failures (notinviewport while a
          // scroll is in flight, not-yet-visible/stable) retry with backoff
          // and the next scroll alignment — Playwright's `_retryAction`.
          if reason.contains("notconnected") {
            return Err(FerriError::Backend(reason));
          }
          attempt += 1;
          if attempt >= retry_backoff_ms.len() + 2 {
            return Err(FerriError::Backend(reason));
          }
          last_reason = Some(reason);
          continue;
        },
      }
    };

    // Install Playwright's `setupHitTargetInterceptor` page-side
    // BEFORE dispatching the mouse events. force=true bypasses the
    // check entirely; trial=true runs only the preliminary
    // `expectHitTarget` (Playwright's trial performs the same check
    // inside `setupHitTargetInterceptor` without dispatching).
    // Protocol failure while arming (JS bridge unavailable, teardown
    // race) dispatches uninstrumented — Playwright treats "no events
    // received" as done.
    let interceptor_installed = if opts.is_force() {
      false
    } else if opts.is_trial() {
      if let Ok(HitResult::Missed { description }) = expect_hit_target(element, x, y).await {
        // Surface immediately: "intercepts pointer events" is
        // retryable, and the Locator-level loop owns the deadline, the
        // locator-handler checkpoint, and the retry backoff (Playwright
        // has exactly one such loop). Retrying here would double-sleep
        // past the caller's timeout.
        return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
      }
      false
    } else {
      match armed {
        Some(InstallOutcome::Armed) => true,
        Some(InstallOutcome::Miss(description)) => {
          // Another element already covers the hit point: do NOT
          // dispatch; bubble the retryable miss (see the trial branch).
          return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
        },
        None => false,
      }
    };

    let restore = hold_modifiers(page, &opts.modifiers).await?;
    let dispatch = if opts.is_trial() {
      Ok(())
    } else {
      let (dx, dy) = cross_origin_frame_offset().await?;
      page.click_at_with(x + dx, y + dy, &args).await
    };
    let dispatch = dispatch.and(restore_modifiers(page, restore).await);

    if let Err(e) = dispatch {
      // Clean up any installed interceptor on dispatch failure so the
      // next attempt isn't poisoned by a stale listener.
      if interceptor_installed {
        let _ = finalize_hit_interceptor(element).await;
      }
      // Some of the events may have reached the page, which may have
      // acted on them; clicking again could submit a form twice.
      if opts.is_trial() {
        return Err(e);
      }
      return Err(FerriError::InputSent {
        action: "click".into(),
        message: e.to_string(),
      });
    }

    if !interceptor_installed {
      return Ok(());
    }

    // `Err` from `finalize_hit_interceptor` means the page-side
    // helper round-trip itself failed (target navigated away, JS
    // engine torn down, ...). The mouse events have already
    // dispatched at that point; treat it as success so the action
    // doesn't error on a teardown race. A real action-result failure
    // would have surfaced from `click_at_with` earlier.
    let Ok(hit) = finalize_hit_interceptor(element).await else {
      return Ok(());
    };
    match hit {
      HitResult::Done => return Ok(()),
      HitResult::Missed { description } => {
        // The events dispatched but landed elsewhere (layout shifted
        // between point resolution and the browser processing the
        // queued input). Bubble the retryable miss to the Locator loop
        // (see the trial branch above for the single-loop rationale).
        return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
      },
    }
  }
}

/// Outcome of the page-side hit-target interceptor — see
/// `crates/ferridriver/src/injected/index.ts::finalizeHitInterceptor`.
enum HitResult {
  /// The captured mousedown / mouseup events landed on the target
  /// element (or one of its descendants); the click succeeded.
  Done,
  /// Another element (or no element) was at the hit point when the
  /// mouse events fired. The retry loop in [`click_with_opts`]
  /// recomputes the click point and tries again.
  Missed { description: String },
}

/// Outcome of arming the page-side hit-target interceptor.
#[derive(Debug)]
pub enum InstallOutcome {
  /// Interceptor armed; finalize after the dispatch.
  Armed,
  /// The preliminary `expectHitTarget` already reports another element
  /// at the hit point — nothing was armed and the action must NOT
  /// dispatch (Playwright returns the description into its retry loop).
  Miss(String),
}

/// Trial-mode preliminary hit-target check: `expectHitTarget` only, no
/// interceptor armed and no events dispatched. Playwright's trial run
/// performs the same check inside `setupHitTargetInterceptor`.
async fn expect_hit_target(element: &AnyElement, x: f64, y: f64) -> Result<HitResult> {
  let js = format!(
    "function() {{ const r = window.__fd.expectHitTarget({{x: {x}, y: {y}}}, this); \
     return r === 'done' ? 'done' : JSON.stringify(r); }}"
  );
  let val = element.call_js_fn_value(&js).await?;
  let raw = val
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_default();
  if raw.is_empty() || raw == "done" || raw == "\"done\"" {
    return Ok(HitResult::Done);
  }
  if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw)
    && let Some(desc) = parsed.get("hitTargetDescription").and_then(|v| v.as_str())
  {
    return Ok(HitResult::Missed {
      description: desc.to_string(),
    });
  }
  Ok(HitResult::Done)
}

/// Tear down the interceptor and read the captured outcome.
async fn finalize_hit_interceptor(element: &AnyElement) -> Result<HitResult> {
  let js = "function() { return JSON.stringify(window.__fd.finalizeHitInterceptor()); }";
  let val = element.call_js_fn_value(js).await?;
  let raw = val
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_default();
  if raw.is_empty() || raw == "\"done\"" {
    return Ok(HitResult::Done);
  }
  // Object form: { hitTargetDescription: "..." }.
  if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw)
    && let Some(desc) = parsed.get("hitTargetDescription").and_then(|v| v.as_str())
  {
    return Ok(HitResult::Missed {
      description: desc.to_string(),
    });
  }
  Ok(HitResult::Done)
}

/// Dispatch a hover with the full [`crate::options::HoverOptions`]
/// surface. Mirrors Playwright's `server/dom.ts::ElementHandle._hover`:
/// scroll-into-view, actionability (unless `force`), press modifiers,
/// then emit `steps` interpolated `mousemove` events ending at the
/// element's centre (or the `position` offset). No `mousedown` /
/// `mouseup` is fired — hover is the bare pointer move. Modifiers are
/// released on every exit path so page state doesn't leak.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the
/// per-backend mouse-move dispatch.
pub async fn hover_with_opts(element: &AnyElement, page: &AnyPage, opts: &crate::options::HoverOptions) -> Result<()> {
  let scroll_align = if opts.scroll == Some(crate::options::ActionScroll::None) {
    Some("false")
  } else {
    scroll_alignment(pointer_retry())
  };
  let (x, y, armed) = if opts.is_force() {
    let (x, y) = resolve_click_point(
      element,
      opts.position,
      opts.scroll != Some(crate::options::ActionScroll::None),
      scroll_alignment(pointer_retry()),
    )
    .await?;
    (x, y, None)
  } else {
    let hit_action = (!opts.is_trial()).then_some("hover");
    match click_prep_with_states(element, page, opts.position, &["visible"], scroll_align, hit_action).await? {
      ClickPrep::Ready { x, y, armed } => (x, y, armed),
      ClickPrep::NotActionable { reason } => return Err(FerriError::Backend(reason)),
    }
  };

  // Hit-target check (Playwright's `'hover'` interceptor set:
  // `mousemove`): trial runs only the preliminary `expectHitTarget`;
  // the real dispatch armed the interceptor inside `click_prep_with_states`.
  let interceptor_installed = if opts.is_force() {
    false
  } else if opts.is_trial() {
    if let Ok(HitResult::Missed { description }) = expect_hit_target(element, x, y).await {
      return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
    }
    false
  } else {
    match armed {
      Some(InstallOutcome::Armed) => true,
      Some(InstallOutcome::Miss(description)) => {
        return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
      },
      None => false,
    }
  };

  let restore = hold_modifiers(page, &opts.modifiers).await?;
  let result: Result<()> = if opts.is_trial() {
    Ok(())
  } else {
    match cross_origin_frame_offset().await {
      Ok((dx, dy)) => {
        page
          .hover_at_with(x + dx, y + dy, &crate::backend::BackendHoverArgs { steps: 1 })
          .await
      },
      Err(error) => Err(error),
    }
  };
  let result = result.and(restore_modifiers(page, restore).await);

  if interceptor_installed {
    // Always tear down (a dispatch error must not leak an armed
    // listener into the next attempt); the captured outcome only
    // matters when the dispatch itself succeeded.
    let hit = finalize_hit_interceptor(element).await;
    if result.is_ok()
      && let Ok(HitResult::Missed { description }) = hit
    {
      return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
    }
  }
  result
}

/// What to do with the pointer at the point one half of a drag resolved.
pub(crate) enum DragStep {
  /// Move to the source and press (Playwright's `'move and down'`).
  Press,
  /// Move to the target in `steps` from where the press was, and release
  /// (`'move and up'`).
  Release { from: (f64, f64), steps: u32 },
}

/// One half of Playwright's `dragAndDrop` (`server/frames.ts`), each its
/// own pointer action as there: wait for the element to be visible and
/// stable, reveal it, check that it receives the pointer at its point
/// (as a mouse press on the source, a drag over the target), then act.
/// Returns the point acted at.
pub(crate) async fn drag_step(
  element: &AnyElement,
  page: &AnyPage,
  opts: &crate::options::DragAndDropOptions,
  step: DragStep,
) -> Result<(f64, f64)> {
  let force = opts.force.unwrap_or(false);
  let trial = opts.trial.unwrap_or(false);
  let (position, interceptor) = match step {
    DragStep::Press => (opts.source_position, "mouse"),
    DragStep::Release { .. } => (opts.target_position, "drag"),
  };
  let scroll_align = if opts.scroll == Some(crate::options::ActionScroll::None) {
    Some("false")
  } else {
    scroll_alignment(pointer_retry())
  };
  let (x, y, armed_outcome) = if force {
    let (x, y) = resolve_click_point(
      element,
      position,
      opts.scroll != Some(crate::options::ActionScroll::None),
      scroll_alignment(pointer_retry()),
    )
    .await?;
    (x, y, None)
  } else {
    let hit_action = (!trial).then_some(interceptor);
    match click_prep_with_states(element, page, position, &["visible"], scroll_align, hit_action).await? {
      ClickPrep::Ready { x, y, armed } => (x, y, armed),
      ClickPrep::NotActionable { reason } => return Err(FerriError::Backend(reason)),
    }
  };
  let armed = if force {
    false
  } else if trial {
    if let Ok(HitResult::Missed { description }) = expect_hit_target(element, x, y).await {
      return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
    }
    false
  } else {
    match armed_outcome {
      Some(InstallOutcome::Armed) => true,
      Some(InstallOutcome::Miss(description)) => {
        return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
      },
      None => false,
    }
  };
  if trial {
    return Ok((x, y));
  }
  let (dx, dy) = cross_origin_frame_offset().await?;
  let (px, py) = (x + dx, y + dy);
  let result = match step {
    DragStep::Press => match page.move_mouse(px, py).await {
      Ok(()) => page.mouse_down(px, py, "left", 1).await,
      Err(error) => Err(error),
    },
    DragStep::Release { from, steps } => match page.move_mouse_smooth(from.0, from.1, px, py, steps).await {
      Ok(()) => page.mouse_up(px, py, "left", 1).await,
      Err(error) => Err(error),
    },
  };
  if armed {
    let hit = finalize_hit_interceptor(element).await;
    if result.is_ok()
      && let Ok(HitResult::Missed { description }) = hit
    {
      return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
    }
  }
  result.map(|()| (px, py))
}

/// Dispatch a tap (touch event) with the full Playwright
/// [`crate::options::TapOptions`] surface. Mirrors
/// `server/dom.ts::ElementHandle._tap` + `input.ts::Touchscreen::tap`:
/// scroll-into-view, actionability (unless `force`), press modifiers,
/// then emit a real `Input.dispatchTouchEvent { touchStart; touchEnd }`
/// pair at the element's centre (or `position` offset).
///
/// Every backend dispatches native, trusted touch input: CDP
/// `Input.dispatchTouchEvent`, `WebKit` `Input.dispatchTapEvent`,
/// `BiDi` an `input.performActions` `touch` pointer sequence. On `BiDi`
/// only the modifiers variant surfaces
/// [`crate::error::FerriError::Unsupported`] (Firefox does not apply
/// key-source modifier state to touch-pointer events).
///
/// Modifiers are pressed before dispatch and released on every exit
/// path so page state never leaks.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the
/// backend's native `tap_at_with` dispatch.
pub async fn tap_with_opts(element: &AnyElement, page: &AnyPage, opts: &crate::options::TapOptions) -> Result<()> {
  let scroll_align = if opts.scroll == Some(crate::options::ActionScroll::None) {
    Some("false")
  } else {
    scroll_alignment(pointer_retry())
  };
  let (x, y, armed) = if opts.is_force() {
    let (x, y) = resolve_click_point(
      element,
      opts.position,
      opts.scroll != Some(crate::options::ActionScroll::None),
      scroll_alignment(pointer_retry()),
    )
    .await?;
    (x, y, None)
  } else {
    let hit_action = (!opts.is_trial()).then_some("tap");
    match click_prep_with_states(
      element,
      page,
      opts.position,
      &["visible", "enabled"],
      scroll_align,
      hit_action,
    )
    .await?
    {
      ClickPrep::Ready { x, y, armed } => (x, y, armed),
      ClickPrep::NotActionable { reason } => return Err(FerriError::Backend(reason)),
    }
  };

  // Hit-target check (Playwright's `'tap'` interceptor set:
  // pointerdown/up + touchstart/end/cancel) — same lifecycle as hover.
  let interceptor_installed = if opts.is_force() {
    false
  } else if opts.is_trial() {
    if let Ok(HitResult::Missed { description }) = expect_hit_target(element, x, y).await {
      return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
    }
    false
  } else {
    match armed {
      Some(InstallOutcome::Armed) => true,
      Some(InstallOutcome::Miss(description)) => {
        return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
      },
      None => false,
    }
  };

  let restore = hold_modifiers(page, &opts.modifiers).await?;
  let result: Result<()> = if opts.is_trial() {
    Ok(())
  } else {
    match cross_origin_frame_offset().await {
      Ok((dx, dy)) => page.tap_at_with(x + dx, y + dy).await,
      Err(error) => Err(error),
    }
  };
  let result = result.and(restore_modifiers(page, restore).await);

  if interceptor_installed {
    let hit = finalize_hit_interceptor(element).await;
    if result.is_ok()
      && let Ok(HitResult::Missed { description }) = hit
    {
      return Err(FerriError::Backend(format!("{description} intercepts pointer events")));
    }
  }
  result
}
