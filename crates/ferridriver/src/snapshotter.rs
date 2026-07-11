//! DOM snapshot capture for traces (`tracing.start({ snapshots: true })`).
//!
//! Installs Playwright's page-side snapshot streamer (vendored compiled
//! source, `src/injected/snapshotter_injected.js`) into every document of
//! a traced context via the context init-script registry, then captures a
//! named snapshot of each frame around every traced action. Snapshots are
//! written into the trace as v8 `frame-snapshot` events whose
//! `snapshotName` matches the action's `beforeSnapshot` / `afterSnapshot`
//! fields, so the viewer's snapshot pane renders the DOM state around
//! each action. Stylesheet text captured by the streamer is deduplicated
//! by sha1 into `resources/` and referenced via `resourceOverrides`.
//!
//! Child frames are annotated onto their parent's `<iframe>` element
//! ([`annotate_iframe`], mirroring
//! `snapshotter.ts::_annotateFrameHierarchy`), so the parent snapshot
//! serializes the iframe as `src="/snapshot/<frameId>"` and the viewer
//! inlines the child frame's own snapshot instead of a placeholder.
//! Annotation fires on frame attach (the page bookkeeping listener) and
//! is re-asserted before every capture — the attach-time call can race
//! an action's snapshot, and the mark is a per-document element
//! property that a parent navigation silently drops.

use std::sync::Arc;

use crate::trace::{TraceEvent, TraceRecorder, TraceResource};

/// Window global holding the streamer instance. Fixed name: one streamer
/// per document, shared by every recorder of the context.
const STREAMER_GLOBAL: &str = "__ferridriver_snapshot_streamer__";

const SNAPSHOTTER_JS: &str = include_str!("injected/snapshotter_injected.js");

/// Per-frame capture round-trip cap. The capture is best effort — a
/// stalled document (navigation limbo, busy main thread) is skipped
/// rather than wedging the action that triggered it.
const CAPTURE_TIMEOUT_MS: u64 = 2_000;

/// Per-document snapshot-history epoch. The capture expression compares
/// the document's stored epoch against the recording's current one and
/// runs `captureSnapshot(true)` (full reset, no `[[n,m]]`
/// back-references) on mismatch — a fresh document (`undefined`), a
/// document that outlived a prior recording, a re-`tracing.start`, and
/// every chunk boundary all begin with a self-contained snapshot, so
/// stale node refs can never dangle into snapshots the current trace
/// file does not contain (mirrors Playwright's reset-on-start protocol,
/// `snapshotter.ts`). Epoch comparison happens lazily inside the next
/// capture — no boundary-time evaluate into every frame, which would
/// stall on frames whose execution context is gone.
const EPOCH_GLOBAL: &str = "__ferridriver_snapshot_epoch__";

/// Per-document force-reset flag, set after a capture whose result was
/// dropped (the streamer's snapshot number advanced past what the trace
/// file contains).
const NEEDS_RESET_GLOBAL: &str = "__ferridriver_snapshot_needs_reset__";

/// Source that installs the streamer into a document (idempotent — the
/// streamer bails if the global already exists).
pub(crate) fn install_source() -> String {
  format!("({})({STREAMER_GLOBAL:?}, true);", SNAPSHOTTER_JS.trim_end())
}

/// Expression evaluated in each frame to capture one snapshot. Returns a
/// JSON string (the wire-clean stringify pattern shared with the utility
/// wrapper) or `undefined` when the streamer is not installed. Resets
/// the streamer's history when the document's epoch is stale or a
/// dropped capture flagged it.
fn capture_expression(epoch: u64) -> String {
  format!(
    "window[{STREAMER_GLOBAL:?}] && (() => {{ \
       const reset = window[{EPOCH_GLOBAL:?}] !== {epoch} || window[{NEEDS_RESET_GLOBAL:?}] === true; \
       window[{EPOCH_GLOBAL:?}] = {epoch}; \
       window[{NEEDS_RESET_GLOBAL:?}] = false; \
       return JSON.stringify(window[{STREAMER_GLOBAL:?}].captureSnapshot(reset)); \
     }})()"
  )
}

/// Expression that forces the frame's next capture to be self-contained
/// (evaluated after a dropped capture).
fn mark_reset_expression() -> String {
  format!("window[{NEEDS_RESET_GLOBAL:?}] = true")
}

/// Mark a child frame's `<iframe>` element in its parent frame with the
/// child's frame id (`window[streamer].markIframe(el, frameId)`), so
/// the parent's snapshot serializes the iframe as
/// `src="/snapshot/<frameId>"` and the viewer inlines the child's
/// snapshot. The backend resolves the frame-owner element at protocol
/// level (CDP `DOM.getFrameOwner`, `WebKit` `DOM.resolveNode {frameId}`,
/// `BiDi` `browsingContext.locateNodes` with a context locator).
///
/// # Errors
///
/// Propagates the backend's protocol error (frame detached, context
/// not ready); callers treat annotation as best effort.
pub(crate) async fn annotate_iframe(
  page: &crate::backend::AnyPage,
  child_frame_id: &str,
  parent_frame_id: &str,
) -> crate::error::Result<()> {
  page
    .mark_snapshot_iframe(child_frame_id, parent_frame_id, STREAMER_GLOBAL)
    .await
}

/// Capture a named DOM snapshot of every live frame of `page` into
/// `recorder`. Best effort per frame; ordering puts the main frame first.
pub(crate) async fn capture_page_snapshot(
  recorder: &Arc<TraceRecorder>,
  page: &crate::page::Page,
  call_id: &str,
  snapshot_name: &str,
) {
  let page_id = format!("page@{}", page.backend_page_id());
  // Re-assert iframe marks before serializing the parent frame — the
  // attach-time annotation can still be in flight when the action's
  // capture fires, and a parent navigation mints fresh iframe elements
  // that carry no mark.
  for (frame_id, parent_id) in page.trace_child_frame_list() {
    let annotated = tokio::time::timeout(
      std::time::Duration::from_millis(CAPTURE_TIMEOUT_MS),
      annotate_iframe(page.inner(), &frame_id, &parent_id),
    )
    .await;
    if let Ok(Err(e)) = annotated {
      tracing::debug!(target: "ferridriver::trace", "markIframe skipped for {frame_id}: {e}");
    }
  }
  for (frame_id, is_main) in page.trace_frame_list() {
    let expression = capture_expression(recorder.snapshot_epoch());
    let evaluated = tokio::time::timeout(std::time::Duration::from_millis(CAPTURE_TIMEOUT_MS), async {
      if is_main {
        page.inner().evaluate(&expression).await
      } else {
        page.inner().evaluate_in_frame(&expression, &frame_id).await
      }
    })
    .await;
    let (reachable, data) = match evaluated {
      Ok(Ok(Some(serde_json::Value::String(raw)))) => (true, serde_json::from_str::<serde_json::Value>(&raw).ok()),
      Ok(Ok(_)) => (true, None),
      // Timeout or protocol error: the frame is unreachable — a NEW
      // document starts with an undefined epoch, which the capture
      // expression already treats as "needs reset", so there is
      // nothing to mark (and a mark round-trip would just stall the
      // same way the capture did).
      _ => (false, None),
    };
    let Some(data) = data else {
      if reachable {
        // The streamer already advanced its snapshot number for a
        // capture this trace will never contain — every `[[n,m]]`
        // back-reference across the gap would resolve to the wrong
        // entry in the viewer. Force the frame's next capture to be
        // self-contained.
        force_frame_reset(page, &frame_id, is_main).await;
      }
      continue;
    };
    push_frame_snapshot(recorder, &page_id, &frame_id, is_main, call_id, snapshot_name, &data);
  }
}

/// Mark one frame's next capture as needing a full reset (best effort;
/// only called on frames that just answered an evaluate).
async fn force_frame_reset(page: &crate::page::Page, frame_id: &str, is_main: bool) {
  let expression = mark_reset_expression();
  let marked = tokio::time::timeout(std::time::Duration::from_millis(CAPTURE_TIMEOUT_MS), async {
    if is_main {
      page.inner().evaluate(&expression).await
    } else {
      page.inner().evaluate_in_frame(&expression, frame_id).await
    }
  })
  .await;
  if !matches!(marked, Ok(Ok(_))) {
    tracing::debug!(target: "ferridriver::trace", "snapshot reset mark unreachable for {frame_id}");
  }
}

/// Convert one frame's `SnapshotData` (from the injected streamer) into
/// a v8 `frame-snapshot` event, extracting stylesheet text into
/// sha1-named resources.
fn push_frame_snapshot(
  recorder: &Arc<TraceRecorder>,
  page_id: &str,
  frame_id: &str,
  is_main: bool,
  call_id: &str,
  snapshot_name: &str,
  data: &serde_json::Value,
) {
  let mut resource_overrides = Vec::new();
  if let Some(overrides) = data.get("resourceOverrides").and_then(|v| v.as_array()) {
    for entry in overrides {
      let Some(url) = entry.get("url").and_then(|u| u.as_str()) else {
        continue;
      };
      match entry.get("content") {
        Some(serde_json::Value::String(text)) => {
          // `calculateSha1(buffer) + '.' + extension` (snapshotter.ts).
          let name = format!("{}.css", crate::tracing::sha1_hex(text.as_bytes()));
          recorder.push_resource(&TraceResource {
            name: name.clone(),
            bytes: text.clone().into_bytes(),
          });
          resource_overrides.push(serde_json::json!({ "url": url, "sha1": name }));
        },
        Some(serde_json::Value::Number(generation)) => {
          resource_overrides.push(serde_json::json!({ "url": url, "ref": generation }));
        },
        _ => {},
      }
    }
  }

  let snapshot = serde_json::json!({
    "callId": call_id,
    "snapshotName": snapshot_name,
    "pageId": page_id,
    "frameId": frame_id,
    "frameUrl": data.get("url").cloned().unwrap_or_default(),
    "doctype": data.get("doctype").cloned().unwrap_or(serde_json::Value::Null),
    "html": data.get("html").cloned().unwrap_or(serde_json::Value::Null),
    "viewport": data.get("viewport").cloned().unwrap_or(serde_json::Value::Null),
    "timestamp": recorder.monotonic_ms(),
    "wallTime": data.get("wallTime").cloned().unwrap_or(serde_json::Value::Null),
    "collectionTime": data.get("collectionTime").cloned().unwrap_or(serde_json::Value::Null),
    "resourceOverrides": resource_overrides,
    "isMainFrame": is_main,
  });
  recorder.push_event(&TraceEvent::FrameSnapshot(snapshot));
}
