//! `frameLocator` enter-frame and any-frame selector resolution helpers.

use std::sync::Arc;

use crate::error::Result;
use crate::selectors;

/// `frameLocator()` without a selector: the rest of the selector may
/// match in any frame below the one it starts from.
#[doc(hidden)]
pub const ANY_FRAME: &str = "internal:control=any-frame";

/// What an any-frame locator runs when no frame matches yet: a selector
/// that cannot match, so actions keep waiting and counts read zero.
const MATCHES_NOTHING: &str = ":not(*)";

/// The selector after `frameLocator()`'s token, or `None` when the
/// selector does not start in any frame.
pub(super) fn any_frame_rest(selector: &str) -> Result<Option<&str>> {
  let Some(rest) = selector.trim().strip_prefix(ANY_FRAME) else {
    return Ok(None);
  };
  match rest.trim_start().strip_prefix(">>").map(str::trim) {
    Some(rest) if !rest.is_empty() => Ok(Some(rest)),
    _ => Err(crate::error::FerriError::InvalidSelector {
      selector: selector.to_string(),
      reason: "Selector cannot be empty after frameLocator()".into(),
    }),
  }
}

/// Resolve `frameLocator` enter-frame hops from `start` into the deepest
/// frame plus the selector to run inside it.
pub(super) async fn resolve_frame_hops(
  start: crate::frame::Frame,
  selector: &str,
) -> Result<(crate::frame::Frame, String)> {
  const MARK: &str = ">> internal:control=enter-frame >>";
  if !selector.contains("internal:control=enter-frame") {
    return Ok((start, selector.to_string()));
  }
  let mut parts = selector.split(MARK).map(str::trim);
  let mut cur = start;
  let mut pending = parts.next().unwrap_or("").to_string();
  for next in parts {
    let page_arc = Arc::clone(cur.page_arc());
    let fid = (!cur.is_main_frame()).then(|| cur.id().to_string());
    let el = crate::selectors::query_one(page_arc.inner(), &pending, false, fid.as_deref()).await?;
    let handle = crate::element_handle::ElementHandle::from_any_element(Arc::clone(&page_arc), el).await?;
    cur = handle
      .content_frame()
      .await?
      .ok_or_else(|| crate::error::FerriError::protocol("frameLocator", "<iframe> has no content frame"))?;
    pending = next.to_string();
  }
  Ok((cur, pending))
}

/// Try `selector` from `root` and every frame below it, keeping the
/// frames it matches in. A start frame whose hops lead nowhere, or whose
/// document is between loads, is skipped as Playwright's no-stall pass
/// skips it.
pub(super) async fn resolve_in_any_frame(
  root: &crate::frame::Frame,
  selector: &str,
) -> Result<(crate::frame::Frame, String)> {
  let mut starts = vec![root.clone()];
  let mut next = 0;
  while next < starts.len() {
    let children = starts[next].child_frames();
    starts.extend(children);
    next += 1;
  }
  let mut matched: Vec<(crate::frame::Frame, String)> = Vec::new();
  for start in starts {
    let Ok((frame, rest)) = resolve_frame_hops(start, selector).await else {
      continue;
    };
    if matched.iter().any(|(seen, _)| seen.id() == frame.id()) {
      continue;
    }
    if count_in(&frame, &rest).await.is_ok_and(|n| n > 0) {
      matched.push((frame, rest));
    }
  }
  if matched.len() > 1 {
    return Err(crate::error::FerriError::InvalidSelector {
      selector: format!("{ANY_FRAME} >> {selector}"),
      reason: "frameLocator() matched elements in multiple frames.".into(),
    });
  }
  Ok(
    matched
      .pop()
      .unwrap_or_else(|| (root.clone(), MATCHES_NOTHING.to_string())),
  )
}

/// Whether `selector`, starting with [`ANY_FRAME`], matches from `root` in
/// `frame` and in no other frame.
#[doc(hidden)]
pub async fn any_frame_resolves_to(root: &crate::frame::Frame, selector: &str, frame: &crate::frame::Frame) -> bool {
  let Ok(Some(rest)) = any_frame_rest(selector) else {
    return false;
  };
  match Box::pin(resolve_in_any_frame(root, rest)).await {
    Ok((resolved, rest)) => rest != MATCHES_NOTHING && resolved.id() == frame.id(),
    Err(_) => false,
  }
}

/// How many elements `selector` matches in `frame`, without strictness.
pub(super) async fn count_in(frame: &crate::frame::Frame, selector: &str) -> Result<usize> {
  let parts_json = selectors::build_parts_json(&selectors::parse(selector)?);
  let inner = frame.page_arc().inner();
  let fd = inner.injected_script().await?;
  let js = format!("{fd}.selCount({parts_json})");
  let val = if frame.is_main_frame() {
    inner.evaluate(&js).await
  } else {
    inner.evaluate_in_frame(&js, frame.id()).await
  }?
  .and_then(|v| v.as_u64())
  .unwrap_or(0);
  Ok(usize::try_from(val).unwrap_or(usize::MAX))
}
