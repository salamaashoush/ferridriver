//! The LCP element, and the request that delivered it.
//!
//! An image LCP has a network request behind it and a text LCP does not,
//! and every LCP insight branches on that.
//!
//! Mirrors devtools-frontend `handlers/LargestImagePaintHandler.ts`,
//! which recovers the request by matching the candidate's `imageUrl`.
//! Current Chrome does not emit `imageUrl` on the candidate at all, so
//! the fallback here matches on the `imageLoadStart` / `imageLoadEnd`
//! timings the candidate does carry. Both paths are kept: a trace from
//! a Chrome that names the URL should not be matched by timing.

use crate::event::{Micro, TraceEvent};
use crate::handlers::meta::Meta;
use crate::handlers::network::NetworkRequest;
use crate::units::micros_to_ms;

/// How far a request's observed load may sit from the timings the
/// candidate reports and still be considered the same fetch. The two are
/// measured by different subsystems, so they agree closely but not
/// exactly.
const MATCH_TOLERANCE_MS: f64 = 30.0;

#[derive(Debug, Clone, Default)]
pub struct LargestPaint {
  /// `image` or `text`, as the winning candidate reported it.
  pub kind: String,
  /// URL of the LCP image, when the trace named one.
  pub image_url: String,
  /// The `loading` attribute on the LCP element, when it had one.
  pub loading_attr: String,
  /// Index into the request list for the LCP image.
  pub request: Option<usize>,
}

/// The candidate fields this needs, in the units Chrome writes them:
/// `image_load_*` are milliseconds from the navigation.
#[derive(Default)]
struct Candidate {
  kind: String,
  image_url: String,
  loading_attr: String,
  image_load_start: Option<f64>,
  image_load_end: Option<f64>,
}

impl LargestPaint {
  /// The last candidate wins: candidates supersede one another as the
  /// page paints, and only the final one is the LCP.
  #[must_use]
  pub fn from_events(events: &[TraceEvent], meta: &Meta, requests: &[NetworkRequest], time_origin: Micro) -> Self {
    let mut winner = Candidate::default();

    for event in events {
      if event.name != "largestContentfulPaint::Candidate" {
        continue;
      }
      let frame = event
        .data()
        .and_then(|d| d.get("frame"))
        .or_else(|| event.args_get("frame"))
        .and_then(serde_json::Value::as_str);
      if let Some(frame) = frame
        && !meta.main_frame_id.is_empty()
        && frame != meta.main_frame_id
      {
        continue;
      }
      let Some(data) = event.data() else { continue };
      let str_of = |k: &str| {
        data
          .get(k)
          .and_then(serde_json::Value::as_str)
          .unwrap_or_default()
          .to_string()
      };
      winner = Candidate {
        kind: str_of("type"),
        image_url: str_of("imageUrl"),
        loading_attr: str_of("loadingAttr"),
        image_load_start: data.get("imageLoadStart").and_then(serde_json::Value::as_f64),
        image_load_end: data.get("imageLoadEnd").and_then(serde_json::Value::as_f64),
      };
    }

    let mut paint = Self {
      kind: winner.kind,
      image_url: winner.image_url,
      loading_attr: winner.loading_attr,
      request: None,
    };
    if !paint.is_image() {
      return paint;
    }

    paint.request = if paint.image_url.is_empty() {
      match_by_timing(requests, time_origin, winner.image_load_start, winner.image_load_end)
    } else {
      requests.iter().position(|r| r.url == paint.image_url)
    };
    // Naming the URL is what makes the finding legible, so recover it
    // from whichever request the timings picked.
    if paint.image_url.is_empty()
      && let Some(index) = paint.request
    {
      paint.image_url.clone_from(&requests[index].url);
    }
    paint
  }

  /// Whether the LCP was an image, which decides between the two-part
  /// and four-part breakdown.
  #[must_use]
  pub fn is_image(&self) -> bool {
    self.kind == "image"
  }
}

/// The image request whose observed load best matches what the candidate
/// reported, or `None` when nothing lines up closely enough.
fn match_by_timing(
  requests: &[NetworkRequest],
  time_origin: Micro,
  load_start: Option<f64>,
  load_end: Option<f64>,
) -> Option<usize> {
  let (load_start, load_end) = (load_start?, load_end?);

  requests
    .iter()
    .enumerate()
    .filter(|(_, r)| r.resource_type == "Image")
    .map(|(i, r)| {
      let start_delta = (micros_to_ms(r.start_time - time_origin) - load_start).abs();
      let end_delta = (micros_to_ms(r.timing.finish_time - time_origin) - load_end).abs();
      (i, start_delta + end_delta)
    })
    .filter(|(_, delta)| *delta <= MATCH_TOLERANCE_MS * 2.0)
    .min_by(|a, b| a.1.total_cmp(&b.1))
    .map(|(i, _)| i)
}

/// An image the page actually painted, with both the size the file
/// carries and the size it was drawn at.
///
/// From `PaintImage`, which names `srcWidth`/`srcHeight` for the
/// intrinsic size and `width`/`height` for the painted one.
#[derive(Debug, Clone)]
pub struct PaintedImage {
  pub url: String,
  /// Intrinsic width of the file.
  pub width: i64,
  /// Intrinsic height of the file.
  pub height: i64,
  /// Largest width it was painted at.
  pub displayed_width: i64,
  pub displayed_height: i64,
  /// CSS backgrounds are excluded from the responsive-image advice
  /// upstream, because serving breakpoints for them is disproportionate
  /// effort.
  pub is_css: bool,
}

/// Every painted image, keeping the LARGEST painted size per URL: an
/// image drawn in several places is only oversized relative to the
/// biggest one.
#[must_use]
pub fn painted_images(events: &[TraceEvent]) -> Vec<PaintedImage> {
  let mut images: Vec<PaintedImage> = Vec::new();

  for event in events.iter().filter(|e| e.name == "PaintImage") {
    let Some(data) = event.data() else { continue };
    let num = |k: &str| data.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
    let url = data
      .get("url")
      .and_then(serde_json::Value::as_str)
      .unwrap_or_default()
      .to_string();
    if url.is_empty() {
      continue;
    }
    let (width, height) = (num("srcWidth"), num("srcHeight"));
    let (displayed_width, displayed_height) = (num("width"), num("height"));

    match images.iter_mut().find(|i| i.url == url) {
      Some(existing) => {
        if displayed_width * displayed_height > existing.displayed_width * existing.displayed_height {
          existing.displayed_width = displayed_width;
          existing.displayed_height = displayed_height;
        }
      },
      None => images.push(PaintedImage {
        url,
        width,
        height,
        displayed_width,
        displayed_height,
        is_css: data.get("isCSS").and_then(serde_json::Value::as_bool).unwrap_or(false),
      }),
    }
  }
  images
}
