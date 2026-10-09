//! Playwright's `server/screenshotter.ts`: validating the options,
//! preparing every frame, masking, overriding the background and working
//! out which rect to capture. Each backend only takes the capture
//! ([`crate::backend::AnyPage::take_screenshot`]).

use std::sync::Arc;

use crate::backend::{AnyElement, AnyPage, BackgroundColor, ImageFormat, ScreenshotCapture, ScreenshotScale};
use crate::error::{FerriError, Result};
use crate::locator::Locator;
use crate::options::{
  AnimationsMode, CaretMode, ClipRect, ElementScreenshotOptions, ScreenshotFormat, ScreenshotOptions,
};
use crate::page::Page;

/// `inPagePrepareForScreenshots`, verbatim. Installs the screenshot style
/// in every shadow root, hides carets, finishes or cancels animations,
/// and leaves `window.__pwCleanupScreenshot` to undo all of it.
const PREPARE_FN: &str = r"function inPagePrepareForScreenshots(screenshotStyle, hideCaret, disableAnimations, syncAnimations) {
  if (syncAnimations) {
    const style = document.createElement('style');
    style.textContent = 'body {}';
    document.head.appendChild(style);
    document.documentElement.getBoundingClientRect();
    style.remove();
  }
  if (!screenshotStyle && !hideCaret && !disableAnimations)
    return;
  const collectRoots = (root, roots = []) => {
    roots.push(root);
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
    do {
      const node = walker.currentNode;
      const shadowRoot = node instanceof Element ? node.shadowRoot : null;
      if (shadowRoot)
        collectRoots(shadowRoot, roots);
    } while (walker.nextNode());
    return roots;
  };
  const roots = collectRoots(document);
  const cleanupCallbacks = [];
  if (screenshotStyle) {
    for (const root of roots) {
      const styleTag = document.createElement('style');
      styleTag.textContent = screenshotStyle;
      if (root === document)
        document.documentElement.append(styleTag);
      else
        root.append(styleTag);
      cleanupCallbacks.push(() => {
        styleTag.remove();
      });
    }
  }
  if (hideCaret) {
    const elements = new Map();
    for (const root of roots) {
      root.querySelectorAll('input,textarea,[contenteditable]').forEach(element => {
        elements.set(element, {
          value: element.style.getPropertyValue('caret-color'),
          priority: element.style.getPropertyPriority('caret-color')
        });
        element.style.setProperty('caret-color', 'transparent', 'important');
      });
    }
    cleanupCallbacks.push(() => {
      for (const [element, value] of elements)
        element.style.setProperty('caret-color', value.value, value.priority);
    });
  }
  if (disableAnimations) {
    const infiniteAnimationsToResume = new Set();
    const handleAnimations = root => {
      for (const animation of root.getAnimations()) {
        if (!animation.effect || animation.playbackRate === 0 || infiniteAnimationsToResume.has(animation))
          continue;
        const endTime = animation.effect.getComputedTiming().endTime;
        if (Number.isFinite(endTime)) {
          try {
            animation.finish();
          } catch (e) {
          }
        } else {
          try {
            animation.cancel();
            infiniteAnimationsToResume.add(animation);
          } catch (e) {
          }
        }
      }
    };
    for (const root of roots) {
      const handleRootAnimations = handleAnimations.bind(null, root);
      handleRootAnimations();
      root.addEventListener('transitionrun', handleRootAnimations);
      root.addEventListener('animationstart', handleRootAnimations);
      cleanupCallbacks.push(() => {
        root.removeEventListener('transitionrun', handleRootAnimations);
        root.removeEventListener('animationstart', handleRootAnimations);
      });
    }
    cleanupCallbacks.push(() => {
      for (const animation of infiniteAnimationsToResume) {
        try {
          animation.play();
        } catch (e) {
        }
      }
    });
  }
  window.__pwCleanupScreenshot = () => {
    for (const cleanupCallback of cleanupCallbacks)
      cleanupCallback();
    delete window.__pwCleanupScreenshot;
  };
}";

const RESTORE_JS: &str = "window.__pwCleanupScreenshot && window.__pwCleanupScreenshot()";

const HIDE_HIGHLIGHTS_JS: &str = "window.__fd && window.__fd.hideHighlight()";

const FULL_PAGE_SIZE_JS: &str = r"JSON.stringify((() => {
  if (!document.body || !document.documentElement)
    return null;
  return {
    width: Math.max(
        document.body.scrollWidth, document.documentElement.scrollWidth,
        document.body.offsetWidth, document.documentElement.offsetWidth,
        document.body.clientWidth, document.documentElement.clientWidth
    ),
    height: Math.max(
        document.body.scrollHeight, document.documentElement.scrollHeight,
        document.body.offsetHeight, document.documentElement.offsetHeight,
        document.body.clientHeight, document.documentElement.clientHeight
    ),
  };
})())";

/// The element's border box in main-frame viewport coordinates, or
/// `null` when it has no box. Same-origin frames are walked through
/// `frameElement`, adding each frame's border and padding.
pub(crate) const MAIN_FRAME_BOX_FN: &str = r"function() {
  const r = this.getBoundingClientRect();
  if (r.width === 0 && r.height === 0 && r.x === 0 && r.y === 0) return null;
  let x = r.x, y = r.y;
  let win = this.ownerDocument && this.ownerDocument.defaultView;
  while (win && win !== win.parent && win.frameElement) {
    const frame = win.frameElement;
    const fr = frame.getBoundingClientRect();
    const style = win.parent.getComputedStyle(frame);
    x += fr.x + frame.clientLeft + (parseFloat(style.paddingLeft) || 0);
    y += fr.y + frame.clientTop + (parseFloat(style.paddingTop) || 0);
    win = win.parent;
  }
  return { x, y, width: r.width, height: r.height };
}";

/// `_waitAndScrollIntoViewIfNeeded` for one attempt: `error:notvisible`
/// while the element is hidden (the locator loop retries on it),
/// otherwise scroll it into view when it is not already fully inside.
const SCROLL_INTO_VIEW_IF_VISIBLE_FN: &str = r"function() {
  const fd = this.ownerDocument && this.ownerDocument.defaultView && this.ownerDocument.defaultView.__fd;
  if (fd && !fd.isVisible(this)) return 'error:notvisible';
  const r = this.getBoundingClientRect();
  const win = this.ownerDocument.defaultView;
  const inside = r.top >= 0 && r.left >= 0 && r.bottom <= win.innerHeight && r.right <= win.innerWidth;
  if (!inside) this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
  return 'done';
}";

/// The capture options page and element screenshots share.
pub(crate) struct Request<'a> {
  format: Option<ScreenshotFormat>,
  quality: Option<i64>,
  omit_background: bool,
  animations: Option<AnimationsMode>,
  caret: Option<CaretMode>,
  mask: &'a [Locator],
  mask_color: Option<&'a str>,
  scale: Option<crate::options::ScreenshotScale>,
  style: Option<&'a str>,
}

impl<'a> From<&'a ScreenshotOptions> for Request<'a> {
  fn from(o: &'a ScreenshotOptions) -> Self {
    Self {
      format: o.format,
      quality: o.quality,
      omit_background: o.omit_background.unwrap_or(false),
      animations: o.animations,
      caret: o.caret,
      mask: &o.mask,
      mask_color: o.mask_color.as_deref(),
      scale: o.scale,
      style: o.style.as_deref(),
    }
  }
}

impl<'a> From<&'a ElementScreenshotOptions> for Request<'a> {
  fn from(o: &'a ElementScreenshotOptions) -> Self {
    Self {
      format: o.format,
      quality: o.quality,
      omit_background: o.omit_background.unwrap_or(false),
      animations: o.animations,
      caret: o.caret,
      mask: &o.mask,
      mask_color: o.mask_color.as_deref(),
      scale: o.scale,
      style: o.style.as_deref(),
    }
  }
}

impl Request<'_> {
  /// `validateScreenshotOptions`, with Playwright's messages.
  pub(crate) fn validate(&self, clip: Option<ClipRect>) -> Result<ImageFormat> {
    let format = match self.format.unwrap_or_default() {
      ScreenshotFormat::Png => ImageFormat::Png,
      ScreenshotFormat::Jpeg => ImageFormat::Jpeg,
      ScreenshotFormat::Webp => ImageFormat::Webp,
    };
    if let Some(quality) = self.quality {
      if format == ImageFormat::Png {
        return Err(FerriError::invalid_argument(
          "quality",
          "options.quality is unsupported for the png screenshots",
        ));
      }
      if !(0..=100).contains(&quality) {
        return Err(FerriError::invalid_argument(
          "quality",
          format!("Expected options.quality to be between 0 and 100 (inclusive), got {quality}"),
        ));
      }
    }
    if let Some(clip) = clip {
      if clip.width <= 0.0 || clip.width.is_nan() {
        return Err(FerriError::invalid_argument(
          "clip",
          "Expected options.clip.width to be greater than 0.",
        ));
      }
      if clip.height <= 0.0 || clip.height.is_nan() {
        return Err(FerriError::invalid_argument(
          "clip",
          "Expected options.clip.height to be greater than 0.",
        ));
      }
    }
    Ok(format)
  }

  fn scale(&self) -> ScreenshotScale {
    match self.scale.unwrap_or_default() {
      crate::options::ScreenshotScale::Css => ScreenshotScale::Css,
      crate::options::ScreenshotScale::Device => ScreenshotScale::Device,
    }
  }
}

#[derive(Debug, Clone, Copy)]
struct Size {
  width: f64,
  height: f64,
}

/// `screenshotPage`.
pub(crate) async fn screenshot_page(page: &Page, options: &ScreenshotOptions) -> Result<Vec<u8>> {
  let request = Request::from(options);
  let format = request.validate(options.clip)?;
  let _queue = page.screenshot_queue.lock().await;
  let viewport = original_viewport_size(page).await?;
  prepare(page, &request).await;
  let result = async {
    if options.full_page.unwrap_or(false) {
      // Firefox reports the scrolling box before the reflow that adds a
      // vertical scrollbar has run, so the same page measured 400px or
      // 385px wide depending on which side of that flush it landed.
      page
        .inner
        .evaluate("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))")
        .await?;
      let full = full_page_size(page).await?;
      let fits_viewport = full.width <= viewport.width && full.height <= viewport.height;
      let document_rect = match options.clip {
        Some(clip) => trim_clip_to_size(clip, full)?,
        None => rect(0.0, 0.0, full),
      };
      capture(page, format, Some(document_rect), None, fits_viewport, &request).await
    } else {
      let viewport_rect = match options.clip {
        Some(clip) => trim_clip_to_size(clip, viewport)?,
        None => rect(0.0, 0.0, viewport),
      };
      capture(page, format, None, Some(viewport_rect), true, &request).await
    }
  }
  .await;
  restore(page).await;
  result
}

/// The element half of `screenshotElement`, for an element already
/// resolved and with the page's screenshot queue already held.
///
/// Returns `error:notvisible` while the element is hidden, which the
/// locator's retry loop keeps retrying until its deadline.
pub(crate) async fn screenshot_resolved_element(
  page: &Page,
  element: &AnyElement,
  format: ImageFormat,
  request: &Request<'_>,
) -> Result<Vec<u8>> {
  let viewport = original_viewport_size(page).await?;
  prepare(page, request).await;
  let result = async {
    let scrolled = element.call_js_fn_value(SCROLL_INTO_VIEW_IF_VISIBLE_FN).await?;
    if scrolled.as_ref().and_then(serde_json::Value::as_str) == Some("error:notvisible") {
      return Err(FerriError::backend("error:notvisible"));
    }
    let bounding_box = element
      .call_js_fn_value(MAIN_FRAME_BOX_FN)
      .await?
      .and_then(|v| parse_rect(&v))
      .ok_or_else(|| FerriError::backend("Node is either not visible or not an HTMLElement"))?;
    if bounding_box.width == 0.0 {
      return Err(FerriError::backend("Node has 0 width."));
    }
    if bounding_box.height == 0.0 {
      return Err(FerriError::backend("Node has 0 height."));
    }
    let fits_viewport = bounding_box.width <= viewport.width && bounding_box.height <= viewport.height;
    let scroll = eval_json(&page.inner, "JSON.stringify({ x: window.scrollX, y: window.scrollY })").await?;
    let document_rect = enclosing_int_rect(ClipRect {
      x: bounding_box.x + scroll.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      y: bounding_box.y + scroll.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      width: bounding_box.width,
      height: bounding_box.height,
    });
    capture(page, format, Some(document_rect), None, fits_viewport, request).await
  }
  .await;
  restore(page).await;
  result
}

/// `_screenshot`: background override and masks around one engine capture.
async fn capture(
  page: &Page,
  format: ImageFormat,
  document_rect: Option<ClipRect>,
  viewport_rect: Option<ClipRect>,
  fits_viewport: bool,
  request: &Request<'_>,
) -> Result<Vec<u8>> {
  // jpeg does not support transparency.
  let set_background = request.omit_background && format != ImageFormat::Jpeg;
  if set_background {
    page
      .inner
      .set_background_color(Some(BackgroundColor::TRANSPARENT))
      .await?;
  }
  let masked = match mask_elements(page, request).await {
    Ok(masked) => masked,
    Err(e) => {
      if set_background {
        let _ = page.inner.set_background_color(None).await;
      }
      return Err(e);
    },
  };
  let quality = match format {
    ImageFormat::Jpeg => Some(request.quality.unwrap_or(80)),
    ImageFormat::Webp => Some(request.quality.unwrap_or(100)),
    ImageFormat::Png => None,
  }
  .map(|q| u8::try_from(q.clamp(0, 100)).unwrap_or(100));
  let result = page
    .inner
    .take_screenshot(ScreenshotCapture {
      format,
      document_rect,
      viewport_rect,
      quality,
      fits_viewport,
      scale: request.scale(),
    })
    .await;
  if masked {
    evaluate_in_all_frames(page, HIDE_HIGHLIGHTS_JS).await;
  }
  if set_background {
    let restored = page.inner.set_background_color(None).await;
    if result.is_ok() {
      restored?;
    }
  }
  result
}

/// `_maskElements`: paint each mask locator's matches in its own frame.
async fn mask_elements(page: &Page, request: &Request<'_>) -> Result<bool> {
  if request.mask.is_empty() {
    return Ok(false);
  }
  evaluate_in_all_frames(page, HIDE_HIGHLIGHTS_JS).await;
  let color = request.mask_color.unwrap_or("#F0F");
  let arg = crate::protocol::argument_from_serde(&color)?;
  for locator in request.mask {
    let applied = locator
      .evaluate_all(
        "(elements, color) => window.__fd._injected.addMaskedElements(elements, color)",
        arg.clone(),
        Some(true),
      )
      .await;
    if let Err(e) = applied {
      evaluate_in_all_frames(page, HIDE_HIGHLIGHTS_JS).await;
      return Err(e);
    }
  }
  Ok(true)
}

/// `_preparePageForScreenshot`: every frame, errors ignored, then wait
/// for fonts.
async fn prepare(page: &Page, request: &Request<'_>) {
  let style = serde_json::to_string(&request.style).unwrap_or_else(|_| "null".into());
  let hide_caret = request.caret != Some(CaretMode::Initial);
  let disable_animations = request.animations == Some(AnimationsMode::Disabled);
  let sync_animations = page.inner.should_toggle_style_sheet_to_sync_animations();
  let expression = format!("({PREPARE_FN})({style}, {hide_caret}, {disable_animations}, {sync_animations})");
  evaluate_in_all_frames(page, &expression).await;
  evaluate_in_all_frames(page, "document.fonts.ready.then(() => true)").await;
}

/// `_restorePageAfterScreenshot`.
async fn restore(page: &Page) {
  evaluate_in_all_frames(page, RESTORE_JS).await;
}

/// `safeNonStallingEvaluateInAllFrames`: a frame that cannot run it (it
/// is navigating, or gone) is skipped.
async fn evaluate_in_all_frames(page: &Page, expression: &str) {
  let main = page.with_frame_cache(crate::frame_cache::FrameCache::main_frame_id);
  let ids: Vec<Arc<str>> = page.with_frame_cache(|c| c.live_ids().collect());
  let inner = &page.inner;
  let runs = ids.iter().map(|id| {
    let is_main = main.as_deref() == Some(&**id);
    async move {
      let _ = if is_main {
        inner.evaluate(expression).await
      } else {
        inner.evaluate_in_frame(expression, id).await
      };
    }
  });
  if ids.is_empty() {
    let _ = inner.evaluate(expression).await;
  } else {
    futures::future::join_all(runs).await;
  }
}

/// `_originalViewportSize`: the emulated viewport, else the window's.
async fn original_viewport_size(page: &Page) -> Result<Size> {
  if let Some((width, height)) = page.viewport_size() {
    let side = |v: i64| f64::from(i32::try_from(v).unwrap_or(i32::MAX));
    return Ok(Size {
      width: side(width),
      height: side(height),
    });
  }
  let value = eval_json(
    &page.inner,
    "JSON.stringify({ width: window.innerWidth, height: window.innerHeight })",
  )
  .await?;
  size_of(&value).ok_or_else(|| FerriError::backend("could not read the viewport size"))
}

async fn full_page_size(page: &Page) -> Result<Size> {
  let value = eval_json(&page.inner, FULL_PAGE_SIZE_JS).await?;
  size_of(&value).ok_or_else(|| FerriError::backend("could not read the full page size: the document has no body"))
}

async fn eval_json(inner: &AnyPage, expression: &str) -> Result<serde_json::Value> {
  match inner.evaluate(expression).await? {
    Some(serde_json::Value::String(text)) => Ok(serde_json::from_str(&text)?),
    Some(other) => Ok(other),
    None => Ok(serde_json::Value::Null),
  }
}

fn size_of(value: &serde_json::Value) -> Option<Size> {
  Some(Size {
    width: value.get("width")?.as_f64()?,
    height: value.get("height")?.as_f64()?,
  })
}

pub(crate) fn parse_rect(value: &serde_json::Value) -> Option<ClipRect> {
  let value = match value {
    serde_json::Value::String(text) => serde_json::from_str(text).ok()?,
    other => other.clone(),
  };
  Some(ClipRect {
    x: value.get("x")?.as_f64()?,
    y: value.get("y")?.as_f64()?,
    width: value.get("width")?.as_f64()?,
    height: value.get("height")?.as_f64()?,
  })
}

fn rect(x: f64, y: f64, size: Size) -> ClipRect {
  ClipRect {
    x,
    y,
    width: size.width,
    height: size.height,
  }
}

/// `trimClipToSize`.
fn trim_clip_to_size(clip: ClipRect, size: Size) -> Result<ClipRect> {
  let x1 = clip.x.min(size.width).max(0.0);
  let y1 = clip.y.min(size.height).max(0.0);
  let x2 = (clip.x + clip.width).min(size.width).max(0.0);
  let y2 = (clip.y + clip.height).min(size.height).max(0.0);
  let result = ClipRect {
    x: x1,
    y: y1,
    width: x2 - x1,
    height: y2 - y1,
  };
  if result.width > 0.0 && result.height > 0.0 {
    Ok(result)
  } else {
    Err(FerriError::invalid_argument(
      "clip",
      "Clipped area is either empty or outside the resulting image",
    ))
  }
}

/// `helper.enclosingIntRect`.
fn enclosing_int_rect(r: ClipRect) -> ClipRect {
  let x = (r.x + 1e-3).floor();
  let y = (r.y + 1e-3).floor();
  let x2 = (r.x + r.width - 1e-3).ceil();
  let y2 = (r.y + r.height - 1e-3).ceil();
  ClipRect {
    x,
    y,
    width: x2 - x,
    height: y2 - y,
  }
}

#[cfg(test)]
mod tests {
  use super::{ClipRect, Size, enclosing_int_rect, trim_clip_to_size};

  #[test]
  fn a_clip_is_trimmed_to_the_image() {
    let size = Size {
      width: 100.0,
      height: 50.0,
    };
    let trimmed = trim_clip_to_size(
      ClipRect {
        x: -10.0,
        y: 40.0,
        width: 30.0,
        height: 30.0,
      },
      size,
    )
    .expect("overlaps");
    assert_eq!(
      (trimmed.x, trimmed.y, trimmed.width, trimmed.height),
      (0.0, 40.0, 20.0, 10.0)
    );
    let outside = trim_clip_to_size(
      ClipRect {
        x: 120.0,
        y: 0.0,
        width: 10.0,
        height: 10.0,
      },
      size,
    );
    assert!(outside.is_err());
  }

  #[test]
  fn an_element_rect_widens_to_whole_pixels() {
    let r = enclosing_int_rect(ClipRect {
      x: 10.4,
      y: 0.9995,
      width: 20.2,
      height: 5.0,
    });
    assert_eq!((r.x, r.y, r.width, r.height), (10.0, 1.0, 21.0, 5.0));
  }
}
