//! Best-practices page-quality audits: doctype, image aspect ratio, responsive image sizing, and paste inputs.

use crate::{Artifacts, AuditCategory, AuditItem, AuditResult, Image, Rect, Viewport, failed, passed};

/// `doctype`: the page is not in quirks mode.
///
/// Lighthouse also reports limited-quirks mode, which it learns from an
/// `InspectorIssues` quirks-mode issue matched against the trace's main
/// frame. Neither exists here, and Lighthouse's own snapshot mode is in
/// the same position, so both take the same branch.
#[must_use]
pub fn doctype(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "doctype";
  const PASS: &str = "Page has the HTML doctype";
  const FAIL: &str = "Page lacks the HTML doctype, thus triggering quirks-mode";
  let category = AuditCategory::BestPractices;

  let Some(doctype) = &artifacts.doctype else {
    return failed(ID, category, FAIL, Some("Document must contain a doctype"), Vec::new());
  };
  if doctype.document_compat_mode == "CSS1Compat" {
    return passed(ID, category, PASS);
  }
  let explanation = if !doctype.public_id.is_empty() {
    "Expected publicId to be an empty string"
  } else if !doctype.system_id.is_empty() {
    "Expected systemId to be an empty string"
  } else if doctype.name != "html" {
    "Doctype name must be the string `html`"
  } else {
    "Document contains a `doctype` that triggers `quirks-mode`"
  };
  failed(ID, category, FAIL, Some(explanation), Vec::new())
}

/// Lighthouse's `URL.guessMimeType`, which both image audits use to skip
/// vector images.
pub(crate) fn guess_mime_type(src: &str) -> Option<&'static str> {
  let url = url::Url::parse(src).ok()?;
  if url.scheme() == "data" {
    let path = url.path();
    let (mime, _) = path.split_once([';', ','])?;
    return match mime {
      "image/png" => Some("image/png"),
      "image/jpeg" => Some("image/jpeg"),
      "image/svg+xml" => Some("image/svg+xml"),
      "image/webp" => Some("image/webp"),
      "image/gif" => Some("image/gif"),
      "image/avif" => Some("image/avif"),
      _ => None,
    };
  }
  let path = url.path().to_lowercase();
  let (_, ext) = path.rsplit_once('.')?;
  match ext {
    "png" => Some("image/png"),
    "jpeg" | "jpg" => Some("image/jpeg"),
    "svg" => Some("image/svg+xml"),
    "webp" => Some("image/webp"),
    "gif" => Some("image/gif"),
    "avif" => Some("image/avif"),
    _ => None,
  }
}

/// How far the drawn box may sit from the image's own aspect ratio
/// before it counts as distorted. Lighthouse's `THRESHOLD_PX`.
const ASPECT_RATIO_THRESHOLD_PX: f64 = 2.0;

/// `image-aspect-ratio`: images are drawn at the shape they actually are.
#[must_use]
pub fn image_aspect_ratio(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "image-aspect-ratio";
  const PASS: &str = "Displays images with correct aspect ratio";
  const FAIL: &str = "Displays images with incorrect aspect ratio";
  let category = AuditCategory::BestPractices;

  let mut items = Vec::new();
  for image in &artifacts.images {
    let Some(natural) = image.natural_dimensions else {
      continue;
    };
    if guess_mime_type(&image.src) == Some("image/svg+xml")
      || natural.height <= 5.0
      || natural.width <= 5.0
      || image.displayed_width == 0.0
      || image.displayed_height == 0.0
      || image.object_fit != "fill"
    {
      continue;
    }
    let actual_ratio = natural.width / natural.height;
    let target_height = image.displayed_width / actual_ratio;
    let target_width = image.displayed_height * actual_ratio;
    let matches = if target_height < target_width {
      (target_height - image.displayed_height).abs() < ASPECT_RATIO_THRESHOLD_PX
    } else {
      (target_width - image.displayed_width).abs() < ASPECT_RATIO_THRESHOLD_PX
    };
    if !matches {
      items.push(AuditItem {
        selector: String::new(),
        snippet: elide_data_uri(&image.src),
        detail: format!(
          "displayed {} x {}, actual {} x {}",
          image.displayed_width, image.displayed_height, natural.width, natural.height
        ),
      });
    }
  }

  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

/// Below this drawn size an image must carry its full device-pixel
/// count; above it, three quarters will do. Lighthouse's
/// `SMALL_IMAGE_THRESHOLD` / `SMALL_IMAGE_FACTOR` / `LARGE_IMAGE_FACTOR`.
const SMALL_IMAGE_THRESHOLD: f64 = 64.0;
const SMALL_IMAGE_FACTOR: f64 = 1.0;
const LARGE_IMAGE_FACTOR: f64 = 0.75;

/// Device pixel ratios are quantised before use, so a 1.7x screen asks
/// for the same image as a 1.5x one.
fn quantize_dpr(dpr: f64) -> f64 {
  if dpr >= 2.0 {
    2.0
  } else if dpr >= 1.5 {
    1.5
  } else {
    1.0
  }
}

/// `image-size-responsive`: images carry enough pixels for the size they
/// are drawn at.
#[must_use]
pub fn image_size_responsive(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "image-size-responsive";
  const PASS: &str = "Serves images with appropriate resolution";
  const FAIL: &str = "Serves images with low resolution";
  let category = AuditCategory::BestPractices;
  let dpr = quantize_dpr(artifacts.viewport.device_pixel_ratio);

  let mut results: Vec<(f64, f64, AuditItem, String)> = Vec::new();
  for image in &artifacts.images {
    if !is_size_candidate(image) {
      continue;
    }
    let Some(natural) = image.natural_dimensions else {
      continue;
    };
    let factor = if image.displayed_width > SMALL_IMAGE_THRESHOLD || image.displayed_height > SMALL_IMAGE_THRESHOLD {
      LARGE_IMAGE_FACTOR
    } else {
      SMALL_IMAGE_FACTOR
    };
    let allowed_width = (factor * dpr * image.displayed_width).ceil();
    let allowed_height = (factor * dpr * image.displayed_height).ceil();
    if natural.width >= allowed_width && natural.height >= allowed_height {
      continue;
    }
    if !is_visible(&image.client_rect, &artifacts.viewport)
      || !is_smaller_than_viewport(&image.client_rect, &artifacts.viewport)
    {
      continue;
    }
    let expected_width = (dpr * image.displayed_width).ceil();
    let expected_height = (dpr * image.displayed_height).ceil();
    let expected_pixels = expected_width * expected_height;
    let actual_pixels = natural.width * natural.height;
    let url = elide_data_uri(&image.src);
    results.push((
      expected_pixels,
      actual_pixels,
      AuditItem {
        selector: String::new(),
        snippet: url.clone(),
        detail: format!(
          "displayed {} x {}, actual {} x {}, expected {expected_width} x {expected_height}",
          image.displayed_width, image.displayed_height, natural.width, natural.height
        ),
      },
      url,
    ));
  }

  // Upstream keeps one row per URL (the one asking for most pixels),
  // then orders by how far short each falls.
  results.sort_by(|a, b| a.3.cmp(&b.3));
  let mut deduplicated: Vec<(f64, f64, AuditItem, String)> = Vec::new();
  for result in results {
    match deduplicated.last_mut() {
      Some(previous) if previous.3 == result.3 => {
        if previous.0 < result.0 {
          *previous = result;
        }
      },
      _ => deduplicated.push(result),
    }
  }
  deduplicated.sort_by(|a, b| {
    (b.0 - b.1)
      .partial_cmp(&(a.0 - a.1))
      .unwrap_or(std::cmp::Ordering::Equal)
  });

  let items: Vec<AuditItem> = deduplicated.into_iter().map(|result| result.2).collect();
  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

fn is_size_candidate(image: &Image) -> bool {
  if image.displayed_width <= 1.0 || image.displayed_height <= 1.0 {
    return false;
  }
  let Some(natural) = image.natural_dimensions else {
    return false;
  };
  if natural.width == 0.0 || natural.height == 0.0 {
    return false;
  }
  if guess_mime_type(&image.src) == Some("image/svg+xml") {
    return false;
  }
  if image.object_fit != "fill" {
    return false;
  }
  if matches!(image.rendering.as_str(), "pixelated" | "crisp-edges") {
    return false;
  }
  // A srcset carrying a density descriptor means the author has already
  // said which pixels go with which screen.
  !has_density_descriptor(&image.srcset)
}

/// Lighthouse's `/ \d+(\.\d+)?x/` over the `srcset`.
pub(crate) fn has_density_descriptor(srcset: &str) -> bool {
  let is_digit = |c: char| c.is_ascii_digit();
  srcset.split(' ').skip(1).any(|rest| {
    let after_digits = rest.trim_start_matches(is_digit);
    if after_digits.len() == rest.len() {
      return false;
    }
    let after_fraction = match after_digits.strip_prefix('.') {
      Some(fraction) => {
        let after = fraction.trim_start_matches(is_digit);
        if after.len() == fraction.len() {
          return false;
        }
        after
      },
      None => after_digits,
    };
    after_fraction.starts_with('x')
  })
}

fn is_visible(rect: &Rect, viewport: &Viewport) -> bool {
  (rect.bottom - rect.top) * (rect.right - rect.left) > 0.0
    && rect.top <= viewport.inner_height
    && rect.bottom >= 0.0
    && rect.left <= viewport.inner_width
    && rect.right >= 0.0
}

fn is_smaller_than_viewport(rect: &Rect, viewport: &Viewport) -> bool {
  rect.bottom - rect.top <= viewport.inner_height && rect.right - rect.left <= viewport.inner_width
}

pub(crate) fn elide_data_uri(url: &str) -> String {
  if url.starts_with("data:") && url.chars().nth(100).is_some() {
    let head: String = url.chars().take(99).collect();
    format!("{head}\u{2026}")
  } else {
    url.to_string()
  }
}

/// `paste-preventing-inputs`: nothing stops someone pasting into a field.
#[must_use]
pub fn paste_preventing_inputs(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "paste-preventing-inputs";
  const PASS: &str = "Allows users to paste into input fields";
  const FAIL: &str = "Prevents users from pasting into input fields";
  let category = AuditCategory::BestPractices;

  let items: Vec<AuditItem> = artifacts
    .inputs
    .iter()
    .filter(|input| input.prevents_paste == Some(true))
    .map(|input| AuditItem {
      selector: input.selector.clone(),
      snippet: input.snippet.clone(),
      detail: input.r#type.clone(),
    })
    .collect();

  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}
