//! Images carrying more bytes than their pixels justify.
//!
//! Mirrors devtools-frontend `insights/ImageDelivery.ts`. Every constant
//! is carried over so the byte figures match what `DevTools` reports.
//!
//! Both upstream findings are covered: bytes-per-pixel against what a
//! modern format would achieve, and the intrinsic size against the size
//! the image was actually painted at.

use rustc_hash::FxHashMap;

use crate::handlers::network::NetworkRequest;
use crate::handlers::paint::PaintedImage;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::{count_to_f64, f64_to_count};

/// Bytes per pixel a well-encoded AVIF achieves. Anything heavier than
/// this has room to shrink.
const TARGET_BYTES_PER_PIXEL_AVIF: f64 = 2.0 / 12.0;

/// GIFs smaller than this are not worth converting to video.
const GIF_SIZE_THRESHOLD: i64 = 100 * 1024;

/// A saving under this is noise.
const BYTE_SAVINGS_THRESHOLD: i64 = 4096;

/// Responsive-image advice has a higher bar where the markup already
/// carries breakpoints, because acting on it means producing and
/// serving another one.
const BYTE_SAVINGS_THRESHOLD_RESPONSIVE: i64 = 12288;

#[must_use]
pub fn run(requests: &[NetworkRequest], images: &[PaintedImage], lantern: Option<&crate::lantern::Context>) -> Insight {
  let mut items = Vec::new();
  let mut total_savings = 0i64;
  let mut wasted_by_url: FxHashMap<&str, f64> = FxHashMap::default();

  for request in requests.iter().filter(|r| r.resource_type == "Image") {
    // An SVG's bytes have nothing to do with the pixels it is drawn at.
    if request.mime_type == "image/svg+xml" {
      continue;
    }
    // Without the intrinsic dimensions there is no pixel count to judge
    // the byte count against.
    let Some(image) = images.iter().find(|i| i.url == request.url) else {
      continue;
    };
    let pixels = image.width * image.height;
    // Whichever of the two the server actually spent. A response that
    // compressed well is judged on what went over the wire, and one the
    // trace has no encoded length for is judged on what it decoded to.
    let bytes = min_positive(request.decoded_body_length, request.encoded_data_length);
    if pixels <= 0 || bytes <= 0 {
      continue;
    }

    // Encoding and sizing are independent findings: an image can be
    // efficiently compressed and still be served far larger than it is
    // drawn, so neither check may short-circuit the other.
    let format = if request.mime_type == "image/gif" {
      (bytes > GIF_SIZE_THRESHOLD).then(|| {
        (
          f64_to_count((count_to_f64(bytes) * gif_percent_savings(request.decoded_body_length)).round()),
          "use a video format instead of GIF",
        )
      })
    } else {
      let bytes_per_pixel = count_to_f64(bytes) / count_to_f64(pixels);
      (bytes_per_pixel > TARGET_BYTES_PER_PIXEL_AVIF).then(|| {
        let ideal = f64_to_count((TARGET_BYTES_PER_PIXEL_AVIF * count_to_f64(pixels)).round());
        let advice = if matches!(request.mime_type.as_str(), "image/webp" | "image/avif") {
          "increase compression"
        } else {
          "use a modern format (AVIF, WebP) or increase compression"
        };
        (bytes - ideal, advice)
      })
    };

    // The saving the format change alone would make. It is also the
    // baseline the sizing saving is measured against: bytes already
    // saved by re-encoding cannot be saved a second time by serving
    // fewer pixels, and adding both to the full byte count reports
    // savings the image does not have.
    let format_savings = format.map_or(0, |(savings, _)| savings.max(0));
    let mut image_savings = format_savings;
    let mut optimizations: Vec<(i64, String)> = Vec::new();
    if let Some((savings, advice)) = format {
      optimizations.push((savings, format!("{} ({advice})", request.url)));
    }

    // Serving an image far larger than it is drawn wastes the
    // difference. CSS backgrounds are left alone, as upstream does.
    let displayed_pixels = image.displayed_width * image.displayed_height;
    let wasted_ratio = 1.0 - count_to_f64(displayed_pixels) / count_to_f64(pixels);
    if !image.is_css && wasted_ratio > 0.0 {
      let responsive_savings = f64_to_count((wasted_ratio * count_to_f64(bytes)).round());
      // Markup that already carries breakpoints has to clear a higher
      // bar, because acting on the advice means adding another one.
      if !image.had_breakpoints || responsive_savings > BYTE_SAVINGS_THRESHOLD_RESPONSIVE {
        image_savings += f64_to_count((wasted_ratio * count_to_f64(bytes - format_savings)).round());
        optimizations.push((
          responsive_savings,
          format!(
            "{} (served {}x{}, displayed {}x{}; use responsive images)",
            request.url, image.width, image.height, image.displayed_width, image.displayed_height
          ),
        ));
      }
    }

    // A single optimization worth reporting brings the whole image's
    // saving with it, which is why the threshold is applied per
    // optimization and the total is not re-filtered.
    optimizations.retain(|(savings, _)| *savings > BYTE_SAVINGS_THRESHOLD);
    if optimizations.is_empty() {
      continue;
    }
    total_savings += image_savings;
    wasted_by_url.insert(request.url.as_str(), count_to_f64(image_savings));
    for (savings, label) in optimizations {
      items.push(Item {
        label,
        value: count_to_f64(savings),
        unit: "bytes",
      });
    }
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = wasted_by_url.is_empty();
  let mut metrics = vec![("wastedBytes".into(), count_to_f64(total_savings))];
  crate::insights::push_byte_savings(&mut metrics, lantern, &wasted_by_url);

  Insight {
    key: "ImageDelivery".into(),
    title: "Improve image delivery".into(),
    description: "Reducing the download time of images can improve the perceived load time of the page and LCP.".into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "imagesAreOptimized".into(),
      passed,
      detail: if passed {
        "No optimizable images".into()
      } else {
        format!(
          "{} images could save ~{:.0} KB",
          wasted_by_url.len(),
          count_to_f64(total_savings) / 1024.0
        )
      },
    }],
    metrics,
    items,
  }
}

/// The smaller of two byte counts, ignoring one that was never
/// recorded: a trace can carry a zero for either field.
fn min_positive(a: i64, b: i64) -> i64 {
  match (a > 0, b > 0) {
    (true, true) => a.min(b),
    (true, false) => a,
    (false, true) => b,
    (false, false) => 0,
  }
}

/// Lighthouse's fitted curve for how much a GIF would save as video.
/// Carried over verbatim; the shape is empirical, not derived.
fn gif_percent_savings(decoded_body_length: i64) -> f64 {
  (29.1 * count_to_f64(decoded_body_length).log10() - 100.7).round() / 100.0
}
