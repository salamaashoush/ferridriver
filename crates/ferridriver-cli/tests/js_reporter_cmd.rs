#![allow(clippy::expect_used, clippy::unwrap_used)]
//! A JS reporter selected by module path from a config, driven by a
//! real `ferridriver test` run across two backends.
//!
//! The unit suite (`ferridriver-script`'s `js_reporter`) proves the
//! object shapes against synthesized events; this proves the other
//! half — that a name outside the built-in table resolves to a module,
//! that the module is compiled before the run, and that the run's own
//! events reach it with a project suite per project.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome and Firefox.

use std::path::Path;
use std::process::{Command, Output};

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

const SPEC: &str = r"
import { test, expect } from '@ferridriver/test';

test('adds a row', async ({ page }) => {
  await page.setContent('<b>hi</b>');
  await test.step('read the text', async () => {
    expect(await page.locator('b').textContent()).toBe('hi');
  });
});
";

const CONFIG: &str = r#"
[test]
testMatch = ["specs/*.test.ts"]

[[test.reporter]]
name = "./reporters/counting-reporter.ts"
outputFile = "summary.json"

[[test.projects]]
name = "cdp-pipe"
[test.projects.browser]
browser = "chromium"
backend = "cdp-pipe"
headless = true

[[test.projects]]
name = "bidi"
[test.projects.browser]
browser = "firefox"
backend = "bidi"
headless = true
"#;

fn scratch() -> tempfile::TempDir {
  let dir = tempfile::tempdir().expect("tempdir");
  let root = dir.path();
  std::fs::create_dir_all(root.join("specs")).expect("mkdir specs");
  std::fs::create_dir_all(root.join("reporters")).expect("mkdir reporters");
  std::fs::write(root.join("specs/pay.test.ts"), SPEC).expect("write spec");
  std::fs::write(root.join("ferridriver.toml"), CONFIG).expect("write config");
  let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/counting-reporter.ts");
  std::fs::copy(fixture, root.join("reporters/counting-reporter.ts")).expect("copy reporter");
  dir
}

fn combined(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

#[test]
fn a_reporter_named_by_path_is_driven_by_a_real_run() {
  let dir = scratch();
  let output = Command::new(bin())
    .current_dir(dir.path())
    .args(["test"])
    .output()
    .expect("run ferridriver test");
  let text = combined(&output);
  assert!(output.status.success(), "run failed:\n{text}");

  let summary: serde_json::Value =
    serde_json::from_str(&std::fs::read_to_string(dir.path().join("summary.json")).expect("summary written"))
      .expect("summary is JSON");

  // One `onBegin` / `onEnd` for the whole run, one test per project.
  assert_eq!(summary["calls"]["onBegin"], 1, "{summary}");
  assert_eq!(summary["calls"]["onEnd"], 1, "{summary}");
  assert_eq!(summary["calls"]["onTestBegin"], 2, "{summary}");
  assert_eq!(summary["calls"]["onTestEnd"], 2, "{summary}");
  assert_eq!(
    summary["configuredCalled"], false,
    "a V1 reporter is never configured: {summary}"
  );

  let all_tests = summary["allTests"].as_array().expect("allTests");
  assert_eq!(all_tests.len(), 2, "one case per project: {summary}");
  let joined = summary["allTests"].to_string();
  assert!(joined.contains("cdp-pipe"), "{summary}");
  assert!(joined.contains("bidi"), "{summary}");

  assert_eq!(
    summary["entryTypes"],
    serde_json::json!(["project", "project"]),
    "the root suite's children are the projects: {summary}"
  );
  assert_eq!(
    summary["statuses"],
    serde_json::json!(["passed", "passed"]),
    "{summary}"
  );
  assert_eq!(
    summary["outcomes"],
    serde_json::json!(["expected", "expected"]),
    "{summary}"
  );
  assert!(
    summary["stepTitles"]
      .as_array()
      .expect("stepTitles")
      .iter()
      .any(|title| title == "read the text"),
    "the spec's own step reaches the reporter: {summary}"
  );
  assert_eq!(summary["errors"], serde_json::json!([]), "{summary}");
}

#[test]
fn a_reporter_that_cannot_load_fails_the_command() {
  let dir = scratch();
  std::fs::write(
    dir.path().join("reporters/counting-reporter.ts"),
    "export default class Broken { constructor() { throw new Error('no reporter for you'); } }\n",
  )
  .expect("overwrite reporter");
  let output = Command::new(bin())
    .current_dir(dir.path())
    .args(["test"])
    .output()
    .expect("run ferridriver test");
  let text = combined(&output);
  assert!(
    !output.status.success(),
    "a reporter that throws at construction must fail the command:\n{text}"
  );
  assert!(text.contains("no reporter for you"), "{text}");
}
