//! Images carrying more bytes than their pixels justify.
//!
//! Mirrors devtools-frontend `insights/ImageDelivery.ts`. Every constant
//! is carried over so the byte figures match what `DevTools` reports.
//!
//! Both upstream findings are covered: bytes-per-pixel against what a
//! modern format would achieve, and the intrinsic size against the size
//! the image was actually painted at.

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

/// Responsive-image advice has a higher bar, because acting on it means
/// producing and serving several breakpoints.
const BYTE_SAVINGS_THRESHOLD_RESPONSIVE: i64 = 12288;

#[must_use]
pub fn run(requests: &[NetworkRequest], images: &[PaintedImage]) -> Insight {
  let mut items = Vec::new();
  let mut total_savings = 0i64;

  for request in requests.iter().filter(|r| r.resource_type == "Image") {
    let bytes = request.encoded_data_length;
    // Without the intrinsic dimensions there is no pixel count to judge
    // the byte count against.
    let Some(image) = images.iter().find(|i| i.url == request.url) else {
      continue;
    };
    let pixels = image.width * image.height;
    if pixels <= 0 || bytes <= 0 {
      continue;
    }

    // Encoding and sizing are independent findings: an image can be
    // efficiently compressed and still be served far larger than it is
    // drawn, so neither check may short-circuit the other.
    let format = if request.mime_type == "image/gif" {
      (bytes > GIF_SIZE_THRESHOLD).then(|| {
        (
          f64_to_count(count_to_f64(bytes) * gif_percent_savings(request.decoded_body_length)),
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

    if let Some((savings, advice)) = format
      && savings > BYTE_SAVINGS_THRESHOLD
    {
      total_savings += savings;
      items.push(Item {
        label: format!("{} ({advice})", request.url),
        value: count_to_f64(savings),
        unit: "bytes",
      });
    }

    // Serving an image far larger than it is drawn wastes the difference.
    // CSS backgrounds are left alone, as upstream does.
    let displayed_pixels = image.displayed_width * image.displayed_height;
    if !image.is_css && displayed_pixels > 0 && displayed_pixels < pixels {
      let wasted_ratio = 1.0 - count_to_f64(displayed_pixels) / count_to_f64(pixels);
      let responsive_savings = f64_to_count((wasted_ratio * count_to_f64(bytes)).round());
      if responsive_savings > BYTE_SAVINGS_THRESHOLD_RESPONSIVE {
        total_savings += responsive_savings;
        items.push(Item {
          label: format!(
            "{} (served {}x{}, displayed {}x{}; use responsive images)",
            request.url, image.width, image.height, image.displayed_width, image.displayed_height
          ),
          value: count_to_f64(responsive_savings),
          unit: "bytes",
        });
      }
    }
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = items.is_empty();
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
          items.len(),
          count_to_f64(total_savings) / 1024.0
        )
      },
    }],
    metrics: vec![("wastedBytes".into(), count_to_f64(total_savings))],
    items,
  }
}

/// Lighthouse's fitted curve for how much a GIF would save as video.
/// Carried over verbatim; the shape is empirical, not derived.
fn gif_percent_savings(decoded_body_length: i64) -> f64 {
  (29.1 * count_to_f64(decoded_body_length).log10() - 100.7).round() / 100.0
}
