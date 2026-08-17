#![allow(clippy::expect_used, clippy::unwrap_used)]
//! A step names its own file, and that file survives every hop.
//!
//! `test.step(title, body, { location })` and every BDD step point at a
//! file the spec does not contain. Nothing proves that from inside a
//! spec — the location is carried by the reporter event stream — so this
//! drives the real binary and reads it back out of the two places a
//! consumer picks it up: the `blob` report a shard writes, and the
//! merged HTML `merge-reports` rebuilds from it.
//!
//! The live test-server protocol (`onStepBegin`) is covered by
//! `test_server_ui.rs`.

use std::process::Command;

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

/// A scratch project whose only test opens a step that claims to live
/// in a `.feature` file, plus a skipped one, so the blob carries both a
/// foreign location and a step annotation.
fn write_project(root: &std::path::Path) {
  std::fs::create_dir_all(root.join("specs")).expect("mkdir");
  std::fs::write(
    root.join("specs/steps.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('reports where its steps happened', async ({ page }) => {\n\
     \x20 await test.step('Given the checkout page', async () => {\n\
     \x20   await page.setContent('<h1>checkout</h1>');\n\
     \x20 }, { location: { file: 'features/checkout.feature', line: 12, column: 3 } });\n\
     \x20 await test.step.skip('Then it charges the card', async () => {\n\
     \x20   throw new Error('never runs');\n\
     \x20 });\n\
     \x20 await expect(page.locator('h1')).toHaveText('checkout');\n\
     });\n",
  )
  .expect("write spec");
  std::fs::write(
    root.join("ferridriver.toml"),
    "[test]\n\
     testDir = \"specs\"\n\
     testMatch = [\"**/*.spec.ts\"]\n\
     workers = 1\n\
     retries = 0\n\
     reporter = [{ name = \"blob\" }]\n\
     \n\
     [test.browser]\n\
     browser = \"chromium\"\n\
     backend = \"cdp-pipe\"\n\
     headless = true\n",
  )
  .expect("write config");
}

fn run(root: &std::path::Path, args: &[&str]) -> std::process::Output {
  Command::new(bin())
    .args(args)
    // Without this the repository's own ferridriver.toml layers in
    // underneath (the scratch project sits inside the repo) and its
    // project matrix decides the run.
    .arg("--no-inherit")
    .arg("-c")
    .arg(root.join("ferridriver.toml"))
    .current_dir(root)
    .output()
    .expect("spawn ferridriver")
}

/// Every JSONL line of the blob zip the run wrote.
fn blob_lines(zip: &std::path::Path) -> Vec<serde_json::Value> {
  let file = std::fs::File::open(zip).expect("open blob");
  let mut archive = zip::ZipArchive::new(file).expect("read blob zip");
  let mut lines = Vec::new();
  for i in 0..archive.len() {
    let mut entry = archive.by_index(i).expect("zip entry");
    if std::path::Path::new(entry.name())
      .extension()
      .is_none_or(|ext| !ext.eq_ignore_ascii_case("jsonl"))
    {
      continue;
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text).expect("read jsonl");
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
      lines.push(serde_json::from_str(line).expect("blob line is json"));
    }
  }
  lines
}

#[test]
fn a_steps_own_file_survives_the_blob_and_the_merge() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());

  let out = run(root.path(), &["test"]);
  assert!(
    out.status.success(),
    "the scratch suite must pass:\n{}\n{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );

  let zip = root.path().join("test-results/report.zip");
  assert!(zip.exists(), "no blob report at {}", zip.display());
  let lines = blob_lines(&zip);

  // The live event carries it, so a streaming consumer (the UI, a JS
  // reporter) sees the location while the step is still open.
  let started = lines
    .iter()
    .find(|line| line["kind"] == "step-started" && line["title"] == "Given the checkout page")
    .unwrap_or_else(|| panic!("no step-started for the located step: {lines:#?}"));
  assert_eq!(started["location"]["file"], "features/checkout.feature");
  assert_eq!(started["location"]["line"], 12);
  assert_eq!(started["location"]["column"], 3);

  // And so does the recorded step on the finished outcome, which is
  // what a batch reporter rebuilds its tree from.
  let outcome = lines
    .iter()
    .find(|line| line["kind"] == "test-finished")
    .expect("a finished test");
  let steps = outcome["outcome"]["steps"].as_array().expect("steps");
  let located = steps
    .iter()
    .find(|step| step["title"] == "Given the checkout page")
    .unwrap_or_else(|| panic!("no recorded step: {steps:#?}"));
  assert_eq!(located["location"]["file"], "features/checkout.feature");

  // `test.step.skip` records the step as skipped and annotates it.
  let skipped = steps
    .iter()
    .find(|step| step["title"] == "Then it charges the card")
    .expect("the skipped step");
  assert_eq!(skipped["status"], "skipped");
  assert_eq!(skipped["annotations"][0]["info"]["type_name"], "skip");

  // The schema says 3 — an older reader refuses rather than silently
  // dropping what it cannot understand.
  let header = lines.iter().find(|line| line["kind"] == "header").expect("a header");
  assert_eq!(header["schema"], 3);

  // merge-reports replays the stream into a fresh HTML report, and the
  // location has to be there too: that is the only file a shard's
  // reader ever sees.
  let merged = run(
    root.path(),
    &[
      "merge-reports",
      root.path().join("test-results").to_str().expect("path"),
      "--reporter",
      "html",
      "--output-dir",
      root.path().join("merged").to_str().expect("path"),
    ],
  );
  assert!(
    merged.status.success(),
    "merge-reports failed:\n{}\n{}",
    String::from_utf8_lossy(&merged.stdout),
    String::from_utf8_lossy(&merged.stderr)
  );
  let html = std::fs::read_to_string(root.path().join("merged/report.html")).expect("merged report");
  assert!(
    html.contains("features/checkout.feature:12:3"),
    "the merged report lost the step's file"
  );
}

/// A blob written before the location became structured still merges:
/// its `"file:line"` strings are read back rather than dropped.
#[test]
fn a_pre_schema_3_blob_still_carries_its_step_locations() {
  use ferridriver_test::reporter::blob::WireStepLocation;

  let legacy: WireStepLocation = serde_json::from_str("\"features/legacy.feature:7\"").expect("legacy form");
  let location = legacy.into_runtime().expect("a location");
  assert_eq!(location.file, "features/legacy.feature");
  assert_eq!(location.line, 7);

  let current: WireStepLocation =
    serde_json::from_str(r#"{"file":"spec.ts","line":4,"column":2}"#).expect("current form");
  let location = current.into_runtime().expect("a location");
  assert_eq!(
    (location.file.as_str(), location.line, location.column),
    ("spec.ts", 4, 2)
  );
}
