#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Config-honouring tests for the `ferridriver test` subcommand.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome, exactly like the
//! `backends` suite.

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

/// Build a throwaway workspace with one always-failing spec, run it, and
/// report whether a failure screenshot was written.
fn run_failing_suite(screenshot_on_failure: bool) -> bool {
  let dir = std::env::temp_dir().join(format!(
    "ferri-screenshot-{}-{screenshot_on_failure}",
    std::process::id()
  ));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("create workspace");

  std::fs::write(
    dir.join("specs/fail.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('always fails', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>x</h1>');\n\
     \x20 expect(await page.title()).toBe('never');\n\
     });\n",
  )
  .expect("write spec");

  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "[test]\n\
       testDir = {:?}\n\
       testMatch = [\"**/*.spec.ts\"]\n\
       workers = 1\n\
       retries = 0\n\
       screenshotOnFailure = {screenshot_on_failure}\n\
       outputDir = {:?}\n\
       reporter = [{{ name = \"null\" }}]\n\
       \n\
       [test.browser]\n\
       browser = \"chromium\"\n\
       backend = \"cdp-pipe\"\n\
       headless = true\n",
      dir.join("specs").to_string_lossy(),
      dir.join("out").to_string_lossy(),
    ),
  )
  .expect("write config");

  let out = Command::new(bin())
    .arg("test")
    .arg("-c")
    .arg(dir.join("ferridriver.toml"))
    .output()
    .expect("spawn ferridriver test");
  assert!(
    !out.status.success(),
    "the suite is supposed to fail; if it passed the probe proves nothing: {}",
    String::from_utf8_lossy(&out.stderr)
  );

  let mut found = false;
  let mut stack = vec![dir.join("out")];
  while let Some(path) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&path) else {
      continue;
    };
    for entry in entries.flatten() {
      let p = entry.path();
      if p.is_dir() {
        stack.push(p);
      } else if p.extension().is_some_and(|e| e == "png") {
        found = true;
      }
    }
  }
  let _ = std::fs::remove_dir_all(&dir);
  found
}

/// `screenshotOnFailure` was parsed from config and then ignored by the
/// test worker, which captured unconditionally — a wasted
/// `Page.captureScreenshot` (~12ms) on every failing test, and an
/// artifact the user explicitly asked not to produce.
#[test]
fn screenshot_on_failure_config_is_honoured() {
  assert!(
    run_failing_suite(true),
    "screenshotOnFailure = true must write a failure screenshot"
  );
  assert!(
    !run_failing_suite(false),
    "screenshotOnFailure = false must write no screenshot"
  );
}
