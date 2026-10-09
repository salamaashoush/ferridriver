//! The screenshot comparators against the code they are ported from.
//!
//! `fixtures/comparator/upstream.json` holds Playwright's own pixelmatch
//! and ssim-cie94 verdicts on each image pair, recorded by
//! `scripts/comparator-diff/engine.mjs`. Every count and every diff
//! image here has to match it exactly.

use std::path::{Path, PathBuf};

use ferridriver_test::comparator::{Image, pixelmatch, ssim_cie94};
use sha2::{Digest as _, Sha256};

fn fixtures() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/comparator")
}

fn load(path: &Path) -> image::ImageResult<Image> {
  let image = image::open(path)?.to_rgba8();
  Ok(Image {
    width: image.width(),
    height: image.height(),
    data: image.into_raw(),
  })
}

fn hex(bytes: &[u8]) -> String {
  use std::fmt::Write as _;
  Sha256::digest(bytes).iter().fold(String::new(), |mut out, b| {
    let _ = write!(out, "{b:02x}");
    out
  })
}

#[test]
fn every_verdict_and_diff_image_matches_upstream() -> Result<(), Box<dyn std::error::Error>> {
  let recording: serde_json::Value =
    serde_json::from_str(&std::fs::read_to_string(fixtures().join("upstream.json")).expect("recording")).expect("json");
  let cases = recording["cases"].as_object().expect("cases");
  assert!(cases.len() >= 10, "the recording lost its cases");
  let mut mismatches = Vec::new();
  for (name, expected) in cases {
    let a = load(&fixtures().join(name).join("expected.png"))?;
    let b = load(&fixtures().join(name).join("actual.png"))?;
    let (width, height) = (a.width.max(b.width), a.height.max(b.height));
    let (a, b) = (a.padded(width, height), b.padded(width, height));
    assert_eq!(
      (u64::from(width), u64::from(height)),
      (
        expected["width"].as_u64().unwrap_or(0),
        expected["height"].as_u64().unwrap_or(0)
      ),
      "{name}: padded size"
    );
    let (w, h) = (width as usize, height as usize);
    for (threshold, verdict) in expected["pixelmatch"].as_object().expect("pixelmatch") {
      let mut diff = vec![0u8; w * h * 4];
      let count = pixelmatch(
        &a.data,
        &b.data,
        Some(&mut diff),
        w,
        h,
        threshold.parse().expect("threshold"),
      );
      if count as u64 != verdict["count"].as_u64().unwrap_or(u64::MAX)
        || hex(&diff) != verdict["diff"].as_str().unwrap_or("")
      {
        mismatches.push(format!(
          "{name} pixelmatch {threshold}: {count} vs {}",
          verdict["count"]
        ));
      }
    }
    let mut diff = vec![0u8; w * h * 4];
    // `comparators.ts::compareImages` passes expected first.
    let count = ssim_cie94(&a.data, &b.data, Some(&mut diff), w, h, 1.0);
    let verdict = &expected["ssim"];
    if count as u64 != verdict["count"].as_u64().unwrap_or(u64::MAX)
      || hex(&diff) != verdict["diff"].as_str().unwrap_or("")
    {
      mismatches.push(format!("{name} ssim-cie94: {count} vs {}", verdict["count"]));
    }
  }
  assert!(
    mismatches.is_empty(),
    "differs from upstream:\n{}",
    mismatches.join("\n")
  );
  Ok(())
}
