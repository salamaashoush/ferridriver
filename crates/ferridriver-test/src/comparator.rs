//! Playwright's screenshot comparators (`packages/utils/comparators.ts`):
//! `pixelmatch`, the default, and `ssim-cie94`. Both are ports, held to
//! the originals by `tests/comparator_differential.rs`, which replays
//! verdicts recorded from upstream's own code.

/// RGBA pixels, row-major, 4 bytes each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
  pub width: u32,
  pub height: u32,
  pub data: Vec<u8>,
}

impl Image {
  /// `padImageToSize`: grow to `width`x`height`, filling the new pixels
  /// with transparent black.
  #[must_use]
  pub fn padded(&self, width: u32, height: u32) -> Self {
    if self.width == width && self.height == height {
      return self.clone();
    }
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for y in 0..self.height.min(height) as usize {
      let row = self.width.min(width) as usize * 4;
      let from = y * self.width as usize * 4;
      let to = y * width as usize * 4;
      data[to..to + row].copy_from_slice(&self.data[from..from + row]);
    }
    Self { width, height, data }
  }
}

/// Which comparator a screenshot assertion uses (`comparator` option).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Comparator {
  #[default]
  Pixelmatch,
  SsimCie94,
}

impl std::str::FromStr for Comparator {
  type Err = String;

  fn from_str(name: &str) -> Result<Self, Self::Err> {
    match name {
      "pixelmatch" => Ok(Self::Pixelmatch),
      "ssim-cie94" => Ok(Self::SsimCie94),
      other => Err(format!("Configuration specifies unknown comparator \"{other}\"")),
    }
  }
}

/// The image formats a screenshot baseline can be stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
  Png,
  Jpeg,
  Webp,
}

impl ImageFormat {
  /// `getMimeTypeForPath`, for the image types `getComparator` knows.
  #[must_use]
  pub fn from_path(path: &std::path::Path) -> Option<Self> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
      "png" => Some(Self::Png),
      "jpg" | "jpeg" => Some(Self::Jpeg),
      "webp" => Some(Self::Webp),
      _ => None,
    }
  }

  fn decoder(self) -> image::ImageFormat {
    match self {
      Self::Png => image::ImageFormat::Png,
      Self::Jpeg => image::ImageFormat::Jpeg,
      Self::Webp => image::ImageFormat::WebP,
    }
  }

  /// `validateBuffer`: the magic bytes, checked before decoding.
  fn validate(self, buffer: &[u8]) -> Result<(), String> {
    let valid = match self {
      Self::Png => buffer.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]),
      Self::Jpeg => buffer.starts_with(&[255, 216]),
      Self::Webp => buffer.len() >= 12 && &buffer[..4] == b"RIFF" && &buffer[8..12] == b"WEBP",
    };
    let name = match self {
      Self::Png => "PNG",
      Self::Jpeg => "JPEG",
      Self::Webp => "WEBP",
    };
    if valid {
      Ok(())
    } else {
      Err(format!("Could not decode expected image as {name}."))
    }
  }
}

/// `ImageComparatorOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageComparatorOptions {
  pub threshold: Option<f64>,
  pub max_diff_pixels: Option<u64>,
  pub max_diff_pixel_ratio: Option<f64>,
  pub comparator: Comparator,
}

/// Why two images differ, and the diff image (PNG).
#[derive(Debug, Clone)]
pub struct ImageMismatch {
  pub message: String,
  pub diff: Vec<u8>,
}

/// `comparators.ts::compareImages`: `Ok(None)` when the images match
/// within the budget.
///
/// # Errors
/// When the expected image is not of its format, or either fails to decode.
pub fn compare_images(
  format: ImageFormat,
  actual: &[u8],
  expected: &[u8],
  options: &ImageComparatorOptions,
) -> Result<Option<ImageMismatch>, String> {
  format.validate(expected)?;
  let decode = |bytes: &[u8]| -> Result<Image, String> {
    let image = image::load_from_memory_with_format(bytes, format.decoder())
      .map_err(|e| e.to_string())?
      .to_rgba8();
    Ok(Image {
      width: image.width(),
      height: image.height(),
      data: image.into_raw(),
    })
  };
  let mut actual = decode(actual)?;
  let mut expected = decode(expected)?;
  let (width, height) = (expected.width.max(actual.width), expected.height.max(actual.height));
  let mut sizes_mismatch = String::new();
  if expected.width != actual.width || expected.height != actual.height {
    sizes_mismatch = format!(
      "Expected an image {}px by {}px, received {}px by {}px. ",
      expected.width, expected.height, actual.width, actual.height
    );
    actual = actual.padded(width, height);
    expected = expected.padded(width, height);
  }
  let (w, h) = (width as usize, height as usize);
  let mut diff = vec![0u8; w * h * 4];
  let count = match options.comparator {
    Comparator::SsimCie94 => ssim_cie94(&expected.data, &actual.data, Some(&mut diff), w, h, 1.0),
    Comparator::Pixelmatch => pixelmatch(
      &expected.data,
      &actual.data,
      Some(&mut diff),
      w,
      h,
      options.threshold.unwrap_or(0.2),
    ),
  };

  let pixels = f64::from(expected.width) * f64::from(expected.height);
  let by_ratio = options.max_diff_pixel_ratio.map(|ratio| pixels * ratio);
  let by_count = options
    .max_diff_pixels
    .map(|n| num_traits::ToPrimitive::to_f64(&n).unwrap_or(f64::MAX));
  let max_diff_pixels = match (by_count, by_ratio) {
    (Some(a), Some(b)) => a.min(b),
    (a, b) => a.or(b).unwrap_or(0.0),
  };
  let count_f = num_traits::ToPrimitive::to_f64(&count).unwrap_or(f64::MAX);
  let ratio = (count_f / pixels * 100.0).ceil() / 100.0;
  let pixels_mismatch = if count_f > max_diff_pixels {
    format!("{count} pixels (ratio {ratio:.2} of all image pixels) are different.")
  } else {
    String::new()
  };
  if pixels_mismatch.is_empty() && sizes_mismatch.is_empty() {
    return Ok(None);
  }
  let mut png = Vec::new();
  image::RgbaImage::from_raw(width, height, diff)
    .ok_or("diff image size")?
    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
    .map_err(|e| e.to_string())?;
  Ok(Some(ImageMismatch {
    message: sizes_mismatch + &pixels_mismatch,
    diff: png,
  }))
}

// ── pixelmatch ───────────────────────────────────────────────────────────
//
// ISC License, Copyright (c) 2019, Mapbox. Ported from
// `packages/utils/third_party/pixelmatch.js`.

const AA_COLOR: [u8; 3] = [255, 255, 0];
const DIFF_COLOR: [u8; 3] = [255, 0, 0];
const ALPHA: f64 = 0.1;

/// Count the pixels that differ beyond `threshold` (0..1, YIQ distance),
/// not counting anti-aliasing, painting the diff into `output`.
#[must_use]
pub fn pixelmatch(
  img1: &[u8],
  img2: &[u8],
  output: Option<&mut [u8]>,
  width: usize,
  height: usize,
  threshold: f64,
) -> usize {
  let len = width * height;
  let mut output = output;
  if img1[..len * 4] == img2[..len * 4] {
    if let Some(out) = output.as_deref_mut() {
      for i in 0..len {
        draw_gray_pixel(img1, i * 4, ALPHA, out);
      }
    }
    return 0;
  }

  // 35215 is the maximum possible value of the YIQ difference metric.
  let max_delta = 35215.0 * threshold * threshold;
  let mut diff = 0;
  for y in 0..height {
    for x in 0..width {
      let pos = (y * width + x) * 4;
      let delta = color_delta(img1, img2, pos, pos, false);
      if delta.abs() > max_delta {
        if antialiased(img1, x, y, width, height, img2) || antialiased(img2, x, y, width, height, img1) {
          if let Some(out) = output.as_deref_mut() {
            draw_pixel(out, pos, AA_COLOR);
          }
        } else {
          if let Some(out) = output.as_deref_mut() {
            draw_pixel(out, pos, DIFF_COLOR);
          }
          diff += 1;
        }
      } else if let Some(out) = output.as_deref_mut() {
        draw_gray_pixel(img1, pos, ALPHA, out);
      }
    }
  }
  diff
}

/// "Anti-aliased Pixel and Intensity Slope Detector", V. Vysniauskas, 2009.
fn antialiased(img: &[u8], x1: usize, y1: usize, width: usize, height: usize, img2: &[u8]) -> bool {
  let x0 = x1.saturating_sub(1);
  let y0 = y1.saturating_sub(1);
  let x2 = (x1 + 1).min(width - 1);
  let y2 = (y1 + 1).min(height - 1);
  let pos = (y1 * width + x1) * 4;
  let mut zeroes = usize::from(x1 == x0 || x1 == x2 || y1 == y0 || y1 == y2);
  let mut min = 0.0;
  let mut max = 0.0;
  let (mut min_x, mut min_y, mut max_x, mut max_y) = (0, 0, 0, 0);

  for x in x0..=x2 {
    for y in y0..=y2 {
      if x == x1 && y == y1 {
        continue;
      }
      let delta = color_delta(img, img, pos, (y * width + x) * 4, true);
      if delta == 0.0 {
        zeroes += 1;
        if zeroes > 2 {
          return false;
        }
      } else if delta < min {
        min = delta;
        min_x = x;
        min_y = y;
      } else if delta > max {
        max = delta;
        max_x = x;
        max_y = y;
      }
    }
  }
  if min == 0.0 || max == 0.0 {
    return false;
  }
  (has_many_siblings(img, min_x, min_y, width, height) && has_many_siblings(img2, min_x, min_y, width, height))
    || (has_many_siblings(img, max_x, max_y, width, height) && has_many_siblings(img2, max_x, max_y, width, height))
}

fn has_many_siblings(img: &[u8], x1: usize, y1: usize, width: usize, height: usize) -> bool {
  let x0 = x1.saturating_sub(1);
  let y0 = y1.saturating_sub(1);
  let x2 = (x1 + 1).min(width - 1);
  let y2 = (y1 + 1).min(height - 1);
  let pos = (y1 * width + x1) * 4;
  let mut zeroes = usize::from(x1 == x0 || x1 == x2 || y1 == y0 || y1 == y2);
  for x in x0..=x2 {
    for y in y0..=y2 {
      if x == x1 && y == y1 {
        continue;
      }
      let pos2 = (y * width + x) * 4;
      if img[pos..pos + 4] == img[pos2..pos2 + 4] {
        zeroes += 1;
      }
      if zeroes > 2 {
        return true;
      }
    }
  }
  false
}

/// "Measuring perceived color difference using YIQ NTSC transmission
/// color space in mobile applications", Y. Kotsarenko and F. Ramos.
fn color_delta(img1: &[u8], img2: &[u8], pos1: usize, pos2: usize, y_only: bool) -> f64 {
  let (mut r1, mut g1, mut b1, a1) = (
    f64::from(img1[pos1]),
    f64::from(img1[pos1 + 1]),
    f64::from(img1[pos1 + 2]),
    img1[pos1 + 3],
  );
  let (mut r2, mut g2, mut b2, a2) = (
    f64::from(img2[pos2]),
    f64::from(img2[pos2 + 1]),
    f64::from(img2[pos2 + 2]),
    img2[pos2 + 3],
  );
  if img1[pos1..pos1 + 4] == img2[pos2..pos2 + 4] {
    return 0.0;
  }
  if a1 < 255 {
    let alpha = f64::from(a1) / 255.0;
    r1 = blend(r1, alpha);
    g1 = blend(g1, alpha);
    b1 = blend(b1, alpha);
  }
  if a2 < 255 {
    let alpha = f64::from(a2) / 255.0;
    r2 = blend(r2, alpha);
    g2 = blend(g2, alpha);
    b2 = blend(b2, alpha);
  }
  let luma1 = rgb2y(r1, g1, b1);
  let luma2 = rgb2y(r2, g2, b2);
  let luma = luma1 - luma2;
  if y_only {
    return luma;
  }
  let hue = rgb2i(r1, g1, b1) - rgb2i(r2, g2, b2);
  let saturation = rgb2q(r1, g1, b1) - rgb2q(r2, g2, b2);
  let delta = 0.5053 * luma * luma + 0.299 * hue * hue + 0.1957 * saturation * saturation;
  // Negative when the second pixel is darker.
  if luma1 > luma2 { -delta } else { delta }
}

fn rgb2y(r: f64, g: f64, b: f64) -> f64 {
  r * 0.298_895_31 + g * 0.586_622_47 + b * 0.114_482_23
}

fn rgb2i(r: f64, g: f64, b: f64) -> f64 {
  r * 0.595_977_99 - g * 0.274_176_10 - b * 0.321_801_89
}

fn rgb2q(r: f64, g: f64, b: f64) -> f64 {
  r * 0.211_470_17 - g * 0.522_617_11 + b * 0.311_146_94
}

/// Blend a semi-transparent color with white.
fn blend(c: f64, a: f64) -> f64 {
  255.0 + (c - 255.0) * a
}

fn draw_pixel(output: &mut [u8], pos: usize, [r, g, b]: [u8; 3]) {
  output[pos] = r;
  output[pos + 1] = g;
  output[pos + 2] = b;
  output[pos + 3] = 255;
}

fn draw_gray_pixel(img: &[u8], i: usize, alpha: f64, output: &mut [u8]) {
  let value = blend(
    rgb2y(f64::from(img[i]), f64::from(img[i + 1]), f64::from(img[i + 2])),
    alpha * f64::from(img[i + 3]) / 255.0,
  );
  let v = to_byte(value);
  draw_pixel(output, i, [v, v, v]);
}

/// A `Uint8Array` store: truncate toward zero, wrap modulo 256.
fn to_byte(value: f64) -> u8 {
  num_traits::ToPrimitive::to_u8(&value.trunc().rem_euclid(256.0)).unwrap_or(0)
}

// ── ssim-cie94 ───────────────────────────────────────────────────────────
//
// Ported from `packages/utils/image_tools/{compare,imageChannel,stats,colorUtils}.ts`.

const SSIM_WINDOW_RADIUS: usize = 15;
const VARIANCE_WINDOW_RADIUS: usize = 1;

/// Count the pixels whose CIE94 color difference exceeds
/// `max_color_delta_e94` and that SSIM does not explain as anti-aliasing,
/// painting the diff into `output`.
#[must_use]
pub fn ssim_cie94(
  actual: &[u8],
  expected: &[u8],
  output: Option<&mut [u8]>,
  width: usize,
  height: usize,
  max_color_delta_e94: f64,
) -> usize {
  let padding = SSIM_WINDOW_RADIUS.max(VARIANCE_WINDOW_RADIUS);
  let [r1, g1, b1] = Channel::into_rgb(width, height, expected, padding);
  let [r2, g2, b2] = Channel::into_rgb(width, height, actual, padding);
  let mut output = output;
  let mut draw = |x: usize, y: usize, rgb: [u8; 3]| {
    if let Some(out) = output.as_deref_mut() {
      draw_pixel(out, ((y - padding) * width + (x - padding)) * 4, rgb);
    }
  };

  let mut stats: Option<[FastStats; 3]> = None;
  let mut diff_count = 0;
  for y in padding..r1.height - padding {
    for x in padding..r1.width - padding {
      let gray = || {
        let value = to_byte(blend(
          f64::from(rgb2gray(r1.get(x, y), g1.get(x, y), b1.get(x, y))),
          0.1,
        ));
        [value, value, value]
      };
      if r1.get(x, y) == r2.get(x, y) && g1.get(x, y) == g2.get(x, y) && b1.get(x, y) == b2.get(x, y) {
        draw(x, y, gray());
        continue;
      }
      let delta = color_delta_e94(
        [r1.get(x, y), g1.get(x, y), b1.get(x, y)],
        [r2.get(x, y), g2.get(x, y), b2.get(x, y)],
      );
      if delta <= max_color_delta_e94 {
        draw(x, y, gray());
        continue;
      }
      let [fast_r, fast_g, fast_b] = stats.get_or_insert_with(|| {
        [
          FastStats::new(&r1, &r2),
          FastStats::new(&g1, &g2),
          FastStats::new(&b1, &b2),
        ]
      });
      let (vx1, vy1, vx2, vy2) = r1.window(x, y, VARIANCE_WINDOW_RADIUS);
      let var1 = fast_r.variance_c1(vx1, vy1, vx2, vy2)
        + fast_g.variance_c1(vx1, vy1, vx2, vy2)
        + fast_b.variance_c1(vx1, vy1, vx2, vy2);
      let var2 = fast_r.variance_c2(vx1, vy1, vx2, vy2)
        + fast_g.variance_c2(vx1, vy1, vx2, vy2)
        + fast_b.variance_c2(vx1, vy1, vx2, vy2);
      // Part of a flood fill in either image: not anti-aliasing.
      if var1 == 0.0 || var2 == 0.0 {
        draw(x, y, DIFF_COLOR);
        diff_count += 1;
        continue;
      }
      let (sx1, sy1, sx2, sy2) = r1.window(x, y, SSIM_WINDOW_RADIUS);
      let ssim_rgb =
        (ssim(fast_r, sx1, sy1, sx2, sy2) + ssim(fast_g, sx1, sy1, sx2, sy2) + ssim(fast_b, sx1, sy1, sx2, sy2)) / 3.0;
      if ssim_rgb >= 0.99 {
        draw(x, y, AA_COLOR);
      } else {
        draw(x, y, DIFF_COLOR);
        diff_count += 1;
      }
    }
  }
  diff_count
}

struct Channel {
  width: usize,
  height: usize,
  data: Vec<u8>,
}

impl Channel {
  fn into_rgb(width: usize, height: usize, data: &[u8], padding: usize) -> [Self; 3] {
    const EVEN: [u8; 3] = [255, 0, 255];
    const ODD: [u8; 3] = [0, 255, 0];
    let new_width = width + 2 * padding;
    let new_height = height + 2 * padding;
    let mut channels = [
      vec![0u8; new_width * new_height],
      vec![0u8; new_width * new_height],
      vec![0u8; new_width * new_height],
    ];
    for y in 0..new_height {
      for x in 0..new_width {
        let index = y * new_width + x;
        if y >= padding && y < new_height - padding && x >= padding && x < new_width - padding {
          let offset = ((y - padding) * width + (x - padding)) * 4;
          let alpha = if data[offset + 3] == 255 {
            1.0
          } else {
            f64::from(data[offset + 3]) / 255.0
          };
          for (c, channel) in channels.iter_mut().enumerate() {
            channel[index] = to_byte(blend(f64::from(data[offset + c]), alpha));
          }
        } else {
          let color = if (y + x) % 2 == 0 { EVEN } else { ODD };
          for (c, channel) in channels.iter_mut().enumerate() {
            channel[index] = color[c];
          }
        }
      }
    }
    channels.map(|data| Self {
      width: new_width,
      height: new_height,
      data,
    })
  }

  fn get(&self, x: usize, y: usize) -> u8 {
    self.data[y * self.width + x]
  }

  /// The square of `radius` around (x, y), clamped to the channel
  /// (`ImageChannel.boundXY` on both corners).
  fn window(&self, x: usize, y: usize, radius: usize) -> (usize, usize, usize, usize) {
    (
      x.saturating_sub(radius).min(self.width - 1),
      y.saturating_sub(radius).min(self.height - 1),
      (x + radius).min(self.width - 1),
      (y + radius).min(self.height - 1),
    )
  }
}

/// Summed-area tables over two channels (`stats.ts::FastStats`).
struct FastStats {
  width: usize,
  c1: Vec<f64>,
  c2: Vec<f64>,
  sq1: Vec<f64>,
  sq2: Vec<f64>,
  mult: Vec<f64>,
}

impl FastStats {
  fn new(a: &Channel, b: &Channel) -> Self {
    let (width, height) = (a.width, a.height);
    let n = width * height;
    let mut s = Self {
      width,
      c1: vec![0.0; n],
      c2: vec![0.0; n],
      sq1: vec![0.0; n],
      sq2: vec![0.0; n],
      mult: vec![0.0; n],
    };
    let recalc = |mx: &mut Vec<f64>, idx: usize, initial: f64, x: usize, y: usize| {
      mx[idx] = initial;
      if y > 0 {
        mx[idx] += mx[(y - 1) * width + x];
      }
      if x > 0 {
        mx[idx] += mx[y * width + x - 1];
      }
      if x > 0 && y > 0 {
        mx[idx] -= mx[(y - 1) * width + x - 1];
      }
    };
    for y in 0..height {
      for x in 0..width {
        let idx = y * width + x;
        let (v1, v2) = (f64::from(a.data[idx]), f64::from(b.data[idx]));
        recalc(&mut s.c1, idx, v1, x, y);
        recalc(&mut s.c2, idx, v2, x, y);
        recalc(&mut s.sq1, idx, v1 * v1, x, y);
        recalc(&mut s.sq2, idx, v2 * v2, x, y);
        recalc(&mut s.mult, idx, v1 * v2, x, y);
      }
    }
    s
  }

  fn sum(&self, table: &[f64], x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    let w = self.width;
    let mut result = table[y2 * w + x2];
    if y1 > 0 {
      result -= table[(y1 - 1) * w + x2];
    }
    if x1 > 0 {
      result -= table[y2 * w + x1 - 1];
    }
    if x1 > 0 && y1 > 0 {
      result += table[(y1 - 1) * w + x1 - 1];
    }
    result
  }

  fn count(x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    f64::from(u32::try_from((y2 - y1 + 1) * (x2 - x1 + 1)).unwrap_or(u32::MAX))
  }

  fn mean_c1(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    self.sum(&self.c1, x1, y1, x2, y2) / Self::count(x1, y1, x2, y2)
  }

  fn mean_c2(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    self.sum(&self.c2, x1, y1, x2, y2) / Self::count(x1, y1, x2, y2)
  }

  fn variance_c1(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    let n = Self::count(x1, y1, x2, y2);
    (self.sum(&self.sq1, x1, y1, x2, y2) - self.sum(&self.c1, x1, y1, x2, y2).powi(2) / n) / n
  }

  fn variance_c2(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    let n = Self::count(x1, y1, x2, y2);
    (self.sum(&self.sq2, x1, y1, x2, y2) - self.sum(&self.c2, x1, y1, x2, y2).powi(2) / n) / n
  }

  fn covariance(&self, x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
    let n = Self::count(x1, y1, x2, y2);
    (self.sum(&self.mult, x1, y1, x2, y2) - self.sum(&self.c1, x1, y1, x2, y2) * self.sum(&self.c2, x1, y1, x2, y2) / n)
      / n
  }
}

fn ssim(stats: &FastStats, x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
  const DYNAMIC_RANGE: f64 = 255.0;
  let mean1 = stats.mean_c1(x1, y1, x2, y2);
  let mean2 = stats.mean_c2(x1, y1, x2, y2);
  let var1 = stats.variance_c1(x1, y1, x2, y2);
  let var2 = stats.variance_c2(x1, y1, x2, y2);
  let cov = stats.covariance(x1, y1, x2, y2);
  let c1 = (0.01 * DYNAMIC_RANGE).powi(2);
  let c2 = (0.03 * DYNAMIC_RANGE).powi(2);
  (2.0 * mean1 * mean2 + c1) * (2.0 * cov + c2) / (mean1.powi(2) + mean2.powi(2) + c1) / (var1 + var2 + c2)
}

/// The exact integer formula from SSIM.js.
fn rgb2gray(r: u8, g: u8, b: u8) -> u32 {
  (77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b) + 128) >> 8
}

/// CIE94 color difference; 1.0 is a just-noticeable difference.
fn color_delta_e94(rgb1: [u8; 3], rgb2: [u8; 3]) -> f64 {
  let [l1, a1, b1] = xyz2lab(srgb2xyz(rgb1));
  let [l2, a2, b2] = xyz2lab(srgb2xyz(rgb2));
  let delta_l = l1 - l2;
  let delta_a = a1 - a2;
  let delta_b = b1 - b2;
  let c1 = (a1.powi(2) + b1.powi(2)).sqrt();
  let c2 = (a2.powi(2) + b2.powi(2)).sqrt();
  let delta_c = c1 - c2;
  let delta_h = delta_a.powi(2) + delta_b.powi(2) - delta_c.powi(2);
  let delta_h = if delta_h < 0.0 { 0.0 } else { delta_h.sqrt() };
  let s_c = 1.0 + 0.045 * c1;
  let s_h = 1.0 + 0.015 * c1;
  (delta_l.powi(2) + (delta_c / s_c).powi(2) + (delta_h / s_h).powi(2)).sqrt()
}

fn srgb2xyz([r, g, b]: [u8; 3]) -> [f64; 3] {
  let linear = |c: u8| {
    let c = f64::from(c) / 255.0;
    if c > 0.04045 {
      ((c + 0.055) / 1.055).powf(2.4)
    } else {
      c / 12.92
    }
  };
  let (r, g, b) = (linear(r), linear(g), linear(b));
  [
    r * 0.4124 + g * 0.3576 + b * 0.1805,
    r * 0.2126 + g * 0.7152 + b * 0.0722,
    r * 0.0193 + g * 0.1192 + b * 0.9505,
  ]
}

fn xyz2lab([x, y, z]: [f64; 3]) -> [f64; 3] {
  const SIGMA_POW2: f64 = 6.0 * 6.0 / 29.0 / 29.0;
  const SIGMA_POW3: f64 = 6.0 * 6.0 * 6.0 / 29.0 / 29.0 / 29.0;
  let f = |v: f64| {
    if v > SIGMA_POW3 {
      v.powf(1.0 / 3.0)
    } else {
      v / 3.0 / SIGMA_POW2 + 4.0 / 29.0
    }
  };
  let fx = f(x / 0.950_489);
  let fy = f(y);
  let fz = f(z / 1.088_840);
  [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}
