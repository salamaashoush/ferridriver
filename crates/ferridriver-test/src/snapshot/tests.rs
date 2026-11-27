//! Unit tests for value, ARIA, and screenshot snapshot matching.

use std::path::PathBuf;
use std::time::Duration;

use super::*;
use crate::config::UpdateSnapshotsMode;
use crate::expect::ScreenshotMatcherOptions;

fn png(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
  let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(paint(x, y)));
  let mut bytes = Vec::new();
  image
    .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
    .unwrap();
  bytes
}

fn solid(color: [u8; 4]) -> Vec<u8> {
  png(10, 10, |_, _| color)
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

fn scratch(label: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "ferri-snapshot-{label}-{}-{}",
    std::process::id(),
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_nanos()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

fn files(dir: &Path) -> ScreenshotFiles {
  ScreenshotFiles::new(dir.join("base/hero.png"), &dir.join("out/hero.png"), "hero.png".into())
}

async fn judge(files: &ScreenshotFiles, mode: UpdateSnapshotsMode, shot: Vec<u8>) -> Judged {
  judge_screenshot(
    files,
    &ScreenshotMatcherOptions::default(),
    mode,
    false,
    Duration::from_secs(5),
    move || {
      let shot = shot.clone();
      async move { Ok(shot) }
    },
  )
  .await
}

fn names(judged: &Judged) -> Vec<&str> {
  judged.attachments.iter().map(|a| a.name.as_str()).collect()
}

fn attached_bytes(judged: &Judged, name: &str) -> Vec<u8> {
  let attachment = judged.attachments.iter().find(|a| a.name == name).unwrap();
  let AttachmentBody::Path(path) = &attachment.body else {
    panic!("{name} is not a file");
  };
  std::fs::read(path).unwrap()
}

#[tokio::test]
async fn a_missing_baseline_is_written_and_attached() {
  let dir = scratch("written");
  let files = files(&dir);
  let judged = judge(&files, UpdateSnapshotsMode::Missing, solid(RED)).await;
  let record = judged.comparison.clone().unwrap();
  assert!(matches!(judged.result, Ok(None)));
  assert_eq!(record.status, SnapshotStatus::Written);
  assert!(!record.failed);
  assert_eq!(record.kind, SnapshotComparisonKind::Screenshot);
  assert_eq!(record.name, "hero.png");
  assert_eq!(record.actual.as_deref(), Some("hero-actual.png"));
  assert_eq!(names(&judged), ["hero-expected.png", "hero-actual.png"]);
  assert_eq!(std::fs::read(&files.baseline).unwrap(), solid(RED));
  assert_eq!(attached_bytes(&judged, "hero-actual.png"), solid(RED));
}

#[tokio::test]
async fn the_default_mode_writes_the_baseline_and_fails_softly() {
  let dir = scratch("default");
  let judged = judge(&files(&dir), UpdateSnapshotsMode::Default, solid(RED)).await;
  let record = judged.comparison.unwrap();
  assert!(matches!(judged.result, Ok(Some(_))));
  assert_eq!(record.status, SnapshotStatus::Written);
  assert!(record.failed);
}

#[tokio::test]
async fn none_reports_a_missing_baseline_without_writing_it() {
  let dir = scratch("none");
  let files = files(&dir);
  let judged = judge(&files, UpdateSnapshotsMode::None, solid(RED)).await;
  let record = judged.comparison.clone().unwrap();
  assert!(judged.result.is_err());
  assert_eq!(record.status, SnapshotStatus::Missing);
  assert!(record.failed);
  assert_eq!(names(&judged), Vec::<&str>::new());
  assert!(!files.baseline.exists());
}

#[tokio::test]
async fn a_match_is_passed_with_its_measurements() {
  let dir = scratch("passed");
  let files = files(&dir);
  write_baseline(&files.baseline, &solid(RED)).unwrap();
  let judged = judge(&files, UpdateSnapshotsMode::Missing, solid(RED)).await;
  let record = judged.comparison.clone().unwrap();
  assert!(matches!(judged.result, Ok(None)));
  assert_eq!(record.status, SnapshotStatus::Passed);
  assert_eq!((record.diff_pixels, record.diff_ratio), (Some(0), Some(0.0)));
  assert_eq!((record.width, record.height), (Some(10), Some(10)));
  assert_eq!(names(&judged), Vec::<&str>::new());
}

#[tokio::test]
async fn a_difference_fails_with_expected_actual_and_diff() {
  let dir = scratch("changed");
  let files = files(&dir);
  write_baseline(&files.baseline, &solid(RED)).unwrap();
  let judged = judge(&files, UpdateSnapshotsMode::Missing, solid(BLUE)).await;
  let record = judged.comparison.clone().unwrap();
  let failure = judged.result.as_ref().unwrap_err();
  assert!(
    failure.message.contains("Screenshot comparison failed"),
    "{}",
    failure.message
  );
  assert_eq!(record.status, SnapshotStatus::Changed);
  assert!(record.failed);
  assert_eq!(record.diff_pixels, Some(100));
  assert_eq!(record.diff_ratio, Some(1.0));
  assert_eq!(
    (
      record.expected.as_deref(),
      record.actual.as_deref(),
      record.diff.as_deref()
    ),
    (
      Some("hero-expected.png"),
      Some("hero-actual.png"),
      Some("hero-diff.png")
    )
  );
  assert_eq!(
    names(&judged),
    ["hero-expected.png", "hero-actual.png", "hero-diff.png"]
  );
  assert_eq!(attached_bytes(&judged, "hero-expected.png"), solid(RED));
  assert_eq!(attached_bytes(&judged, "hero-actual.png"), solid(BLUE));
  assert_eq!(
    std::fs::read(&files.baseline).unwrap(),
    solid(RED),
    "the baseline stays"
  );
}

#[tokio::test]
async fn an_update_rewrites_the_baseline_and_keeps_what_it_replaced() {
  for mode in [UpdateSnapshotsMode::Changed, UpdateSnapshotsMode::All] {
    let dir = scratch("updated");
    let files = files(&dir);
    write_baseline(&files.baseline, &solid(RED)).unwrap();
    let judged = judge(&files, mode, solid(BLUE)).await;
    let record = judged.comparison.clone().unwrap();
    assert!(matches!(judged.result, Ok(None)), "{mode:?}");
    assert_eq!(record.status, SnapshotStatus::Updated, "{mode:?}");
    assert!(!record.failed);
    assert_eq!(record.diff_pixels, Some(100));
    assert_eq!(
      names(&judged),
      ["hero-expected.png", "hero-actual.png", "hero-diff.png"]
    );
    assert_eq!(
      attached_bytes(&judged, "hero-expected.png"),
      solid(RED),
      "the replaced baseline"
    );
    assert_eq!(std::fs::read(&files.baseline).unwrap(), solid(BLUE));
  }
}

#[tokio::test]
async fn not_matchers_and_ignored_snapshots_record_nothing() {
  let dir = scratch("not");
  let files = files(&dir);
  write_baseline(&files.baseline, &solid(RED)).unwrap();
  let negated = judge_screenshot(
    &files,
    &ScreenshotMatcherOptions::default(),
    UpdateSnapshotsMode::Missing,
    true,
    Duration::from_secs(5),
    || async { Ok(solid(BLUE)) },
  )
  .await;
  assert!(negated.result.is_ok());
  assert!(negated.comparison.is_none());
  let ignored = judge_screenshot(
    &files,
    &ScreenshotMatcherOptions {
      ignore: true,
      ..ScreenshotMatcherOptions::default()
    },
    UpdateSnapshotsMode::Missing,
    false,
    Duration::from_secs(5),
    || async { Ok(solid(BLUE)) },
  )
  .await;
  assert!(ignored.comparison.is_none());
}

#[test]
fn the_comparator_counts_differing_pixels_either_way() {
  let base = solid(RED);
  let quarter = png(10, 10, |x, y| if x < 5 && y < 5 { BLUE } else { RED });
  let within = ImageComparatorOptions {
    max_diff_pixel_ratio: Some(0.5),
    ..ImageComparatorOptions::default()
  };
  let passed = measure_images(ImageFormat::Png, &quarter, &base, &within).unwrap();
  assert!(passed.mismatch.is_none());
  assert_eq!((passed.diff_pixels, passed.total_pixels), (25, 100));
  assert!((passed.diff_ratio() - 0.25).abs() < f64::EPSILON);
  let failed = measure_images(ImageFormat::Png, &quarter, &base, &ImageComparatorOptions::default()).unwrap();
  assert!(failed.mismatch.is_some());
  assert_eq!(failed.diff_pixels, 25);
}

fn value_info(dir: &Path, mode: UpdateSnapshotsMode) -> TestInfo {
  let mut info = TestInfo::new_anonymous();
  info.output_dir = dir.join("out");
  info.snapshot_dir = dir.join("snaps");
  info.snapshot_path_template = Some(format!("{}/snaps/{{arg}}{{ext}}", dir.display()));
  info.update_snapshots = mode;
  info
}

#[test]
fn value_snapshots_record_written_changed_and_updated() {
  let dir = scratch("value");
  let name = crate::snapshot_path::SnapshotName::One("value.txt".into());
  let options = ImageComparatorOptions::default();
  let text = |s: &str| SnapshotValue::Text(s.into());

  let written = match_value_snapshot(
    &value_info(&dir, UpdateSnapshotsMode::Missing),
    &text("one"),
    &name,
    &options,
    false,
  );
  let record = written.comparison.clone().unwrap();
  assert_eq!(
    (record.kind, record.status),
    (SnapshotComparisonKind::Snapshot, SnapshotStatus::Written)
  );
  assert_eq!(record.name, "value.txt");
  assert_eq!(names(&written), ["value-expected.txt", "value-actual.txt"]);
  assert_eq!(written.attachments[0].content_type, "text/plain");

  let passed = match_value_snapshot(
    &value_info(&dir, UpdateSnapshotsMode::None),
    &text("one"),
    &name,
    &options,
    false,
  );
  assert_eq!(passed.comparison.unwrap().status, SnapshotStatus::Passed);

  let changed = match_value_snapshot(
    &value_info(&dir, UpdateSnapshotsMode::None),
    &text("two"),
    &name,
    &options,
    false,
  );
  let record = changed.comparison.clone().unwrap();
  assert!(changed.result.is_err());
  assert_eq!(record.status, SnapshotStatus::Changed);
  assert_eq!(names(&changed), ["value-expected.txt", "value-actual.txt"]);

  let updated = match_value_snapshot(
    &value_info(&dir, UpdateSnapshotsMode::Changed),
    &text("two"),
    &name,
    &options,
    false,
  );
  assert_eq!(updated.comparison.as_ref().unwrap().status, SnapshotStatus::Updated);
  assert_eq!(attached_bytes(&updated, "value-expected.txt"), b"one");
}

#[tokio::test]
async fn stalled_retry_capture_is_bounded_by_timeout() {
  let dir = scratch("stalled-capture");
  let baseline = dir.join("snaps/stalled.png");
  std::fs::create_dir_all(baseline.parent().unwrap()).unwrap();
  std::fs::write(&baseline, solid(RED)).unwrap();
  let files = ScreenshotFiles::new(baseline, &dir.join("out/stalled.png"), "stalled.png".into());
  let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
  let started = std::time::Instant::now();
  let judged = judge_screenshot(
    &files,
    &crate::expect::ScreenshotMatcherOptions::default(),
    UpdateSnapshotsMode::None,
    false,
    Duration::from_millis(180),
    || {
      let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      async move {
        if n > 0 {
          tokio::time::sleep(Duration::from_secs(10)).await;
        }
        Ok(solid(BLUE))
      }
    },
  )
  .await;
  assert!(judged.result.is_err());
  assert!(
    started.elapsed() < Duration::from_secs(2),
    "stalled retry capture overran timeout: {:?}",
    started.elapsed()
  );
}
